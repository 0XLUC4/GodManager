//! Segmented HTTP(S) downloads. Several connections share one file; a connection left without
//! work splits the biggest part still to fetch, and a pause keeps every part for the resume.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ureq::ResponseExt as _;

/// Marks a part whose end is not known (server sent no size).
pub const UNKNOWN_END: u64 = u64::MAX;
/// A part is split in two only when both halves get at least this much.
const MIN_SPLIT: u64 = 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const RETRIES: u32 = 5;
/// Extension of a file while it downloads.
pub const PART_EXTENSION: &str = "godpart";

/// One byte range of the file: `start..end`, written up to `pos`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub start: u64,
    pub pos: u64,
    pub end: u64,
}

impl Part {
    fn left(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }

    fn complete(&self) -> bool {
        self.pos >= self.end
    }
}

/// Shared speed limit of all HTTP connections (token bucket).
#[derive(Default)]
pub struct Limiter {
    bytes_per_second: AtomicU64,
    bucket: Mutex<Option<(Instant, f64)>>,
}

impl Limiter {
    /// `0` removes the limit.
    pub fn set(&self, bytes_per_second: u64) {
        self.bytes_per_second
            .store(bytes_per_second, Ordering::Relaxed);
    }

    fn take(&self, bytes: usize) {
        let rate = self.bytes_per_second.load(Ordering::Relaxed) as f64;
        if rate <= 0.0 {
            return;
        }
        let wait = {
            let Ok(mut bucket) = self.bucket.lock() else {
                return;
            };
            let now = Instant::now();
            let (last, tokens) = bucket.unwrap_or((now, rate));
            let tokens =
                (tokens + now.duration_since(last).as_secs_f64() * rate).min(rate) - bytes as f64;
            *bucket = Some((now, tokens));
            (-tokens / rate).max(0.0)
        };
        if wait > 0.0 {
            thread::sleep(Duration::from_secs_f64(wait.min(2.0)));
        }
    }
}

/// What to download, and from where to go on.
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub folder: PathBuf,
    /// Final file of an earlier start: a resume writes to it again.
    pub path: Option<PathBuf>,
    pub connections: usize,
    /// Parts saved at the pause; empty for a new download.
    pub parts: Vec<Part>,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    Downloading,
    Done,
    Stopped,
    Failed(String),
}

/// Snapshot of a download for the UI and for saving.
#[derive(Clone, Debug)]
pub struct Progress {
    pub path: Option<PathBuf>,
    pub total: Option<u64>,
    pub done: u64,
    pub parts: Vec<Part>,
    /// Connections reading right now.
    pub connections: usize,
    /// The server accepts ranges: the download can pause and resume.
    pub resumable: bool,
    pub phase: Phase,
}

struct Slot {
    part: Part,
    busy: bool,
}

struct State {
    slots: Vec<Slot>,
    path: Option<PathBuf>,
    total: Option<u64>,
    resumable: bool,
    phase: Phase,
}

struct Shared {
    stop: AtomicBool,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A worker that panicked leaves valid offsets behind: keep going with them.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

pub struct HttpTask {
    shared: Arc<Shared>,
}

impl HttpTask {
    pub fn start(request: Request, agent: ureq::Agent, limiter: Arc<Limiter>) -> Self {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            state: Mutex::new(State {
                slots: Vec::new(),
                path: request.path.clone(),
                total: request.total,
                resumable: false,
                phase: Phase::Connecting,
            }),
        });
        let worker = shared.clone();
        let spawned = thread::Builder::new()
            .name("http-download".into())
            .spawn(move || run(&worker, &request, &agent, &limiter));
        if let Err(err) = spawned {
            shared.lock().phase = Phase::Failed(format!("Cannot start the download: {err}"));
        }
        Self { shared }
    }

    pub fn progress(&self) -> Progress {
        let state = self.shared.lock();
        let parts: Vec<Part> = state.slots.iter().map(|slot| slot.part).collect();
        Progress {
            path: state.path.clone(),
            total: state.total,
            done: parts.iter().map(|part| part.pos - part.start).sum(),
            connections: state
                .slots
                .iter()
                .filter(|slot| slot.busy && !slot.part.complete())
                .count(),
            parts,
            resumable: state.resumable,
            phase: state.phase.clone(),
        }
    }

    /// Ask every connection to end. Takes effect at once for the UI; a connection blocked on the
    /// network never writes again.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let mut state = self.shared.lock();
        if matches!(state.phase, Phase::Connecting | Phase::Downloading) {
            state.phase = Phase::Stopped;
        }
    }
}

/// Agent shared by every download: pooled connections, sane timeouts.
pub fn agent(user_agent: &str) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(user_agent)
        .build()
        .into()
}

fn run(shared: &Shared, request: &Request, agent: &ureq::Agent, limiter: &Limiter) {
    let result = download(shared, request, agent, limiter);
    let mut state = shared.lock();
    state.phase = match result {
        Ok(()) => Phase::Done,
        Err(_) if shared.stopped() => Phase::Stopped,
        Err(err) => Phase::Failed(err),
    };
}

fn download(
    shared: &Shared,
    request: &Request,
    agent: &ureq::Agent,
    limiter: &Limiter,
) -> Result<(), String> {
    let response = get(agent, &request.url, 0, UNKNOWN_END)?;
    let status = response.status().as_u16();
    let ranges = status == 206;
    let total = if ranges {
        content_range_total(response.headers())
    } else {
        header(response.headers(), "content-length").and_then(|len| len.parse().ok())
    };
    let resume = ranges
        && !request.parts.is_empty()
        && request.total == total
        && request.path.as_ref().is_some_and(|path| {
            std::fs::metadata(part_path(path)).is_ok_and(|meta| Some(meta.len()) == total)
        });
    let path = match (&request.path, resume) {
        (Some(path), true) => path.clone(),
        _ => {
            let name = file_name(response.headers(), &response.get_uri().to_string());
            unique_path(&request.folder, &name)
        }
    };
    let part_file = part_path(&path);
    let parts = if resume {
        request.parts.clone()
    } else {
        std::fs::create_dir_all(&request.folder)
            .map_err(|err| format!("Cannot create {}: {err}", request.folder.display()))?;
        let file = File::create(&part_file)
            .map_err(|err| format!("Cannot write {}: {err}", part_file.display()))?;
        if let Some(total) = total {
            file.set_len(total)
                .map_err(|err| format!("Not enough room on the disk: {err}"))?;
        }
        match total {
            Some(total) if ranges => split(total, request.connections),
            _ => vec![Part {
                start: 0,
                pos: 0,
                end: total.unwrap_or(UNKNOWN_END),
            }],
        }
    };
    {
        let mut state = shared.lock();
        if shared.stopped() {
            return Err("Stopped".into());
        }
        state.slots = parts
            .into_iter()
            .map(|part| Slot { part, busy: false })
            .collect();
        state.path = Some(path.clone());
        state.total = total;
        state.resumable = ranges;
        state.phase = Phase::Downloading;
    }

    // The first answer starts at byte 0: the first part reads it instead of asking again.
    let mut first = None;
    if !resume {
        shared.lock().slots[0].busy = true;
        first = Some((0, response.into_body().into_reader()));
    } else {
        drop(response);
    }
    let connections = if ranges {
        request.connections.max(1)
    } else {
        1
    };
    let errors: Vec<Option<String>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..connections)
            .map(|index| {
                let first = if index == 0 { first.take() } else { None };
                let part_file = &part_file;
                scope.spawn(move || {
                    connection(shared, agent, &request.url, part_file, limiter, first)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Some("A connection crashed".into()))
            })
            .collect()
    });

    let complete = shared.lock().slots.iter().all(|slot| slot.part.complete());
    if !complete {
        if shared.stopped() {
            return Err("Stopped".into());
        }
        let error = errors.into_iter().flatten().last();
        return Err(error.unwrap_or_else(|| "The download did not finish".into()));
    }
    let written: u64 = shared
        .lock()
        .slots
        .iter()
        .map(|slot| slot.part.pos - slot.part.start)
        .sum();
    // The name may have been taken while the file downloaded.
    let target = if path.exists() {
        unique_path(
            path.parent().unwrap_or(&request.folder),
            &display_name(&path),
        )
    } else {
        path
    };
    std::fs::rename(&part_file, &target)
        .map_err(|err| format!("Cannot rename {}: {err}", part_file.display()))?;
    let mut state = shared.lock();
    state.total = Some(state.total.unwrap_or(written));
    state.path = Some(target);
    Ok(())
}

/// Equal parts, at most `connections`, none smaller than [`MIN_SPLIT`].
fn split(total: u64, connections: usize) -> Vec<Part> {
    let count = (total / MIN_SPLIT).clamp(1, connections.max(1) as u64);
    let size = total.div_ceil(count);
    (0..count)
        .map(|index| {
            let start = index * size;
            Part {
                start,
                pos: start,
                end: (start + size).min(total),
            }
        })
        .filter(|part| part.start < part.end)
        .collect()
}

/// One connection: take parts until none is left, retrying a part a few times.
fn connection(
    shared: &Shared,
    agent: &ureq::Agent,
    url: &str,
    part_file: &Path,
    limiter: &Limiter,
    mut first: Option<(usize, ureq::BodyReader<'static>)>,
) -> Option<String> {
    let mut file = match OpenOptions::new().write(true).open(part_file) {
        Ok(file) => file,
        Err(err) => return Some(format!("Cannot write {}: {err}", part_file.display())),
    };
    loop {
        if shared.stopped() {
            return None;
        }
        let (index, mut reader) = match first.take() {
            Some((index, reader)) => (index, Some(reader)),
            None => match claim(shared) {
                Some(index) => (index, None),
                None => return None,
            },
        };
        let mut attempt = 0;
        loop {
            let body = match reader.take() {
                Some(body) => Ok(body),
                None => {
                    let part = shared.lock().slots[index].part;
                    get(agent, url, part.pos, part.end).and_then(|response| {
                        if response.status().as_u16() == 206 || part.pos == 0 {
                            Ok(response.into_body().into_reader())
                        } else {
                            Err("The server stopped accepting parts of the file".into())
                        }
                    })
                }
            };
            match body.and_then(|body| copy(shared, index, body, &mut file, limiter)) {
                Ok(()) => break,
                Err(_) if shared.stopped() => return None,
                Err(err) => {
                    attempt += 1;
                    if attempt >= RETRIES {
                        shared.lock().slots[index].busy = false;
                        return Some(err);
                    }
                    thread::sleep(Duration::from_secs(u64::from(attempt)));
                }
            }
        }
    }
}

/// A free part, or the second half of the biggest one being read.
fn claim(shared: &Shared) -> Option<usize> {
    let mut state = shared.lock();
    if let Some(index) = state
        .slots
        .iter()
        .position(|slot| !slot.busy && !slot.part.complete())
    {
        state.slots[index].busy = true;
        return Some(index);
    }
    let (index, part) = state
        .slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.part.end != UNKNOWN_END && slot.part.left() >= 2 * MIN_SPLIT)
        .max_by_key(|(_, slot)| slot.part.left())
        .map(|(index, slot)| (index, slot.part))?;
    let middle = part.pos + part.left() / 2;
    state.slots[index].part.end = middle;
    state.slots.push(Slot {
        part: Part {
            start: middle,
            pos: middle,
            end: part.end,
        },
        busy: true,
    });
    Some(state.slots.len() - 1)
}

fn copy(
    shared: &Shared,
    index: usize,
    mut body: impl Read,
    file: &mut File,
    limiter: &Limiter,
) -> Result<(), String> {
    let mut buffer = vec![0; CHUNK];
    loop {
        let part = shared.lock().slots[index].part;
        if part.complete() {
            return Ok(());
        }
        // Never read past the end: another connection may have taken the rest.
        let want = CHUNK.min(usize::try_from(part.left()).unwrap_or(CHUNK));
        let read = body
            .read(&mut buffer[..want])
            .map_err(|err| format!("Connection lost: {err}"))?;
        if shared.stopped() {
            return Err("Stopped".into());
        }
        if read == 0 {
            if part.end == UNKNOWN_END {
                shared.lock().slots[index].part.end = part.pos;
                return Ok(());
            }
            return Err("The server closed the connection early".into());
        }
        limiter.take(read);
        file.seek(SeekFrom::Start(part.pos))
            .and_then(|_| file.write_all(&buffer[..read]))
            .map_err(|err| format!("Cannot write the file: {err}"))?;
        shared.lock().slots[index].part.pos += read as u64;
    }
}

fn get(
    agent: &ureq::Agent,
    url: &str,
    from: u64,
    end: u64,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let range = if end == UNKNOWN_END {
        format!("bytes={from}-")
    } else {
        format!("bytes={from}-{}", end - 1)
    };
    agent
        .get(url)
        .header("Range", range)
        .call()
        .map_err(|err| match err {
            ureq::Error::StatusCode(code) => format!("The server answered {code}"),
            other => format!("Cannot reach the server: {other}"),
        })
}

fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_owned())
}

/// `bytes 0-999/1000` → `1000`.
fn content_range_total(headers: &ureq::http::HeaderMap) -> Option<u64> {
    header(headers, "content-range")?
        .rsplit('/')
        .next()?
        .parse()
        .ok()
}

/// Name from `Content-Disposition`, else from the last segment of the address.
fn file_name(headers: &ureq::http::HeaderMap, url: &str) -> String {
    let from_header = header(headers, "content-disposition").and_then(|value| disposition(&value));
    let from_url = || {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let path = path.split_once("://").map_or(path, |(_, rest)| rest);
        let (_, file_path) = path.split_once('/')?;
        let last = file_path.trim_end_matches('/').rsplit('/').next()?;
        (!last.is_empty()).then(|| decode(last))
    };
    sanitize(&from_header.or_else(from_url).unwrap_or_default())
}

fn disposition(value: &str) -> Option<String> {
    let params: Vec<&str> = value.split(';').map(str::trim).collect();
    let extended = params.iter().find_map(|param| {
        let rest = param.strip_prefix("filename*=")?;
        let encoded = rest.split_once("''").map_or(rest, |(_, name)| name);
        Some(decode(encoded.trim_matches('"')))
    });
    extended.or_else(|| {
        params.iter().find_map(|param| {
            let name = param.strip_prefix("filename=")?;
            Some(name.trim_matches('"').to_owned())
        })
    })
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

/// A name Windows accepts, without any folder in it.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    if clean.is_empty() {
        "download".to_owned()
    } else {
        clean.to_owned()
    }
}

/// `name.zip`, else `name (1).zip`, `name (2).zip`… free of any file and of any download.
pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    let taken = |path: &Path| path.exists() || part_path(path).exists();
    if !taken(&candidate) {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|index| folder.join(format!("{stem} ({index}){extension}")))
        .find(|path| !taken(path))
        .unwrap_or(candidate)
}

/// File written while downloading: `name.zip.godpart`.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{PART_EXTENSION}"));
    path.with_file_name(name)
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// Name a download gets before the server answers.
pub fn guess_name(url: &str) -> String {
    file_name(&ureq::http::HeaderMap::new(), url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_in_equal_parts() {
        let parts = split(10 * MIN_SPLIT + 3, 4);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts.last().unwrap().end, 10 * MIN_SPLIT + 3);
        assert!(parts.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert_eq!(split(1000, 8).len(), 1);
    }

    #[test]
    fn a_free_connection_takes_half_of_the_biggest_part() {
        let shared = Shared {
            stop: AtomicBool::new(false),
            state: Mutex::new(State {
                slots: vec![Slot {
                    part: Part {
                        start: 0,
                        pos: 0,
                        end: 8 * MIN_SPLIT,
                    },
                    busy: true,
                }],
                path: None,
                total: None,
                resumable: true,
                phase: Phase::Downloading,
            }),
        };
        assert_eq!(claim(&shared), Some(1));
        let state = shared.lock();
        assert_eq!(state.slots[0].part.end, 4 * MIN_SPLIT);
        assert_eq!(state.slots[1].part.start, 4 * MIN_SPLIT);
        assert_eq!(state.slots[1].part.end, 8 * MIN_SPLIT);
    }

    #[test]
    fn names_from_headers_and_addresses() {
        let mut headers = ureq::http::HeaderMap::new();
        assert_eq!(
            file_name(&headers, "https://example.com/files/My%20File.zip?x=1"),
            "My File.zip"
        );
        assert_eq!(file_name(&headers, "https://example.com/"), "download");
        assert_eq!(file_name(&headers, "https://example.com"), "download");
        headers.insert(
            "content-disposition",
            "attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            file_name(&headers, "https://example.com/get?id=3"),
            "résumé.pdf"
        );
        assert_eq!(sanitize("a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize("../../evil.exe"), "evil.exe");
    }

    #[test]
    fn free_names_get_a_number() {
        let folder = std::env::temp_dir().join(format!("godmanager-names-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&folder, "a.txt"), folder.join("a (1).txt"));
        assert_eq!(unique_path(&folder, "b.txt"), folder.join("b.txt"));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
