//! Every download in one list: starts what waits, follows what runs, remembers it all.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::http::{self, HttpTask, Limiter, Part, Phase};
use crate::kind::Kind;
use crate::torrent::{self, Torrents};

const STORE_FILE: &str = "downloads.json";
/// Seconds between two saves while something downloads.
const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub folder: PathBuf,
    /// Connections of one HTTP download.
    pub connections: usize,
    /// HTTP downloads running at the same time.
    pub parallel: usize,
    /// KB/s, `0` for no limit.
    pub down_limit_kb: u64,
    pub up_limit_kb: u64,
    /// Finished torrents keep sharing with others.
    pub seed: bool,
    pub dark: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            folder: default_folder(),
            connections: 8,
            parallel: 3,
            down_limit_kb: 0,
            up_limit_kb: 0,
            seed: true,
            dark: false,
        }
    }
}

/// The Downloads folder of the user.
pub fn default_folder() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("Downloads")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Http {
        url: String,
    },
    /// A `.torrent` file is kept as `torrents/<hash>.torrent` in the data folder.
    Torrent {
        hash: String,
        magnet: Option<String>,
    },
}

/// What a download is doing, as saved. A running HTTP download is saved as `Queued`; a torrent
/// that runs is `Queued` too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Saved {
    Queued,
    Paused,
    Finished,
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub name: String,
    pub source: Source,
    pub folder: PathBuf,
    pub path: Option<PathBuf>,
    /// Unix seconds.
    pub added: u64,
    pub finished: Option<u64>,
    pub size: Option<u64>,
    pub state: Saved,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub connections: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    records: Vec<Record>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Torrent engine not started yet.
    Starting,
    Queued,
    Connecting,
    Downloading,
    Resolving,
    Checking,
    Paused,
    Seeding,
    Finished,
    Failed(String),
}

impl Status {
    pub fn active(&self) -> bool {
        matches!(
            self,
            Status::Connecting | Status::Downloading | Status::Resolving | Status::Checking
        )
    }

    pub fn label(&self) -> &str {
        match self {
            Status::Starting => "Starting",
            Status::Queued => "Waiting",
            Status::Connecting => "Connecting",
            Status::Downloading => "Downloading",
            Status::Resolving => "Getting the file list",
            Status::Checking => "Checking files",
            Status::Paused => "Paused",
            Status::Seeding => "Sharing",
            Status::Finished => "Finished",
            Status::Failed(_) => "Failed",
        }
    }
}

/// One line of the list, ready to draw.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: u64,
    pub name: String,
    pub kind: Kind,
    pub torrent: bool,
    pub status: Status,
    pub done: u64,
    pub total: Option<u64>,
    pub down_bps: f64,
    pub up_bps: f64,
    pub eta: Option<u64>,
    pub peers: usize,
    pub connections: usize,
    pub parts: Vec<Part>,
    pub uploaded: u64,
    pub resumable: bool,
    /// The address, or the magnet link / infohash.
    pub link: String,
    pub folder: PathBuf,
    pub path: Option<PathBuf>,
    pub added: u64,
    pub files: Vec<torrent::File>,
}

impl Row {
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        Some((self.done as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}

pub enum Event {
    Finished(String),
    Failed(String, String),
}

/// Bytes per second, smoothed.
#[derive(Default)]
struct Meter {
    last: Option<(Instant, u64)>,
    bps: f64,
}

impl Meter {
    fn sample(&mut self, now: Instant, done: u64) -> f64 {
        match self.last {
            Some((when, before)) => {
                let elapsed = now.duration_since(when).as_secs_f64();
                if elapsed >= 0.5 {
                    let current = done.saturating_sub(before) as f64 / elapsed;
                    self.bps = if self.bps == 0.0 {
                        current
                    } else {
                        self.bps * 0.6 + current * 0.4
                    };
                    self.last = Some((now, done));
                }
            }
            None => self.last = Some((now, done)),
        }
        self.bps
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

struct Entry {
    record: Record,
    task: Option<HttpTask>,
    meter: Meter,
    resumable: bool,
}

pub struct Manager {
    data_dir: PathBuf,
    pub settings: Settings,
    entries: Vec<Entry>,
    next_id: u64,
    agent: ureq::Agent,
    limiter: Arc<Limiter>,
    torrents: Option<Torrents>,
    starting: Option<Receiver<Result<Torrents, String>>>,
    /// The torrent engine could not start.
    pub torrent_error: Option<String>,
    rows: Vec<Row>,
    dirty: bool,
    saved_at: Instant,
    /// Last save error, shown once.
    pub save_error: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

impl Manager {
    /// Reads the list in `data_dir` and starts the torrent engine in the background.
    pub fn load(data_dir: PathBuf, client: &str) -> Self {
        let store: Store = std::fs::read_to_string(data_dir.join(STORE_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let next_id = store
            .records
            .iter()
            .map(|record| record.id + 1)
            .max()
            .unwrap_or(1)
            .max(store.next_id);
        let entries = store
            .records
            .into_iter()
            .map(|record| Entry {
                record,
                task: None,
                meter: Meter::default(),
                resumable: true,
            })
            .collect();
        let (sender, receiver) = mpsc::channel();
        let torrent_dir = data_dir.join("torrents");
        let folder = store.settings.folder.clone();
        let engine_client = client.to_owned();
        let spawned = std::thread::Builder::new()
            .name("torrent-start".into())
            .spawn(move || {
                let _ = std::fs::create_dir_all(&torrent_dir);
                let _ = sender.send(Torrents::start(&torrent_dir, folder, &engine_client));
            });
        let limiter = Arc::new(Limiter::default());
        limiter.set(store.settings.down_limit_kb * 1024);
        let mut manager = Self {
            agent: http::agent(client),
            data_dir,
            settings: store.settings,
            entries,
            next_id,
            limiter,
            torrents: None,
            starting: spawned.is_ok().then_some(receiver),
            torrent_error: spawned
                .err()
                .map(|err| format!("Cannot start the torrent engine: {err}")),
            rows: Vec::new(),
            dirty: false,
            saved_at: Instant::now(),
            save_error: None,
        };
        manager.rebuild_rows(Instant::now());
        manager
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn torrents_ready(&self) -> bool {
        self.torrents.is_some()
    }

    fn entry(&mut self, id: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.record.id == id)
    }

    fn push(&mut self, name: String, source: Source, folder: PathBuf, start: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            record: Record {
                id,
                name,
                source,
                folder,
                path: None,
                added: now_secs(),
                finished: None,
                size: None,
                state: if start { Saved::Queued } else { Saved::Paused },
                parts: Vec::new(),
                connections: self.settings.connections,
            },
            task: None,
            meter: Meter::default(),
            resumable: true,
        });
        self.dirty = true;
        id
    }

    /// Adds one address: `http(s)://…`, `magnet:?…` or a bare infohash.
    pub fn add_link(
        &mut self,
        link: &str,
        folder: Option<PathBuf>,
        start: bool,
    ) -> Result<u64, String> {
        let link = link.trim();
        let folder = folder.unwrap_or_else(|| self.settings.folder.clone());
        let is_hash = link.len() == 40 && link.chars().all(|ch| ch.is_ascii_hexdigit());
        if link.starts_with("magnet:") || is_hash {
            let magnet = if is_hash {
                format!("magnet:?xt=urn:btih:{link}")
            } else {
                link.to_owned()
            };
            return self.add_torrent(torrent::Source::Magnet(magnet), folder, start);
        }
        let lower = link.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(format!("Not a web address or a magnet link: {link}"));
        }
        let name = http::guess_name(link);
        Ok(self.push(
            name,
            Source::Http {
                url: link.to_owned(),
            },
            folder,
            start,
        ))
    }

    pub fn add_torrent_file(
        &mut self,
        path: &Path,
        folder: Option<PathBuf>,
        start: bool,
    ) -> Result<u64, String> {
        let bytes =
            std::fs::read(path).map_err(|err| format!("Cannot read {}: {err}", path.display()))?;
        let folder = folder.unwrap_or_else(|| self.settings.folder.clone());
        self.add_torrent(torrent::Source::File(bytes), folder, start)
    }

    fn add_torrent(
        &mut self,
        source: torrent::Source,
        folder: PathBuf,
        start: bool,
    ) -> Result<u64, String> {
        let identity = torrent::identify(&source)?;
        let known = self.entries.iter().find(|entry| {
            matches!(&entry.record.source, Source::Torrent { hash, .. } if *hash == identity.hash)
        });
        if let Some(entry) = known {
            return Err(format!("Already in the list: {}", entry.record.name));
        }
        let magnet = match &source {
            torrent::Source::Magnet(link) => Some(link.clone()),
            torrent::Source::File(bytes) => {
                let saved = torrent_copy(&self.data_dir, &identity.hash);
                let written = saved
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&saved, bytes));
                written.map_err(|err| format!("Cannot keep the torrent file: {err}"))?;
                None
            }
        };
        let name = identity
            .name
            .clone()
            .unwrap_or_else(|| format!("Torrent {}", &identity.hash[..8]));
        let hash = identity.hash.clone();
        let id = self.push(
            name,
            Source::Torrent { hash, magnet },
            folder.clone(),
            start,
        );
        if let Some(torrents) = &self.torrents
            && let Err(err) = torrents.add(source, &folder, !start)
        {
            self.fail(id, err);
        }
        Ok(id)
    }

    fn fail(&mut self, id: u64, error: String) {
        if let Some(entry) = self.entry(id) {
            entry.record.state = Saved::Failed(error);
        }
        self.dirty = true;
    }

    pub fn pause(&mut self, id: u64) -> Result<(), String> {
        let torrents = self.torrents.as_ref();
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.record.id == id)
            .ok_or("No such download")?;
        match &entry.record.source {
            Source::Http { .. } => {
                if let Some(task) = entry.task.take() {
                    task.stop();
                    let progress = task.progress();
                    entry.record.parts = progress.parts;
                    entry.record.path = progress.path;
                    entry.record.size = progress.total;
                }
                if entry.record.state == Saved::Queued {
                    entry.record.state = Saved::Paused;
                }
            }
            Source::Torrent { hash, .. } => {
                torrents
                    .ok_or("The torrent engine is not ready")?
                    .pause(hash)?;
                if entry.record.state == Saved::Queued {
                    entry.record.state = Saved::Paused;
                }
            }
        }
        entry.meter.reset();
        self.dirty = true;
        Ok(())
    }

    pub fn resume(&mut self, id: u64) -> Result<(), String> {
        let torrents = self.torrents.as_ref();
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.record.id == id)
            .ok_or("No such download")?;
        if let Source::Torrent { hash, magnet } = &entry.record.source {
            let torrents = torrents.ok_or("The torrent engine is not ready")?;
            if torrents.contains(hash) {
                torrents.resume(hash)?;
            } else {
                // Failed while adding: try again from the source.
                let source = match magnet {
                    Some(link) => torrent::Source::Magnet(link.clone()),
                    None => torrent::Source::File(
                        std::fs::read(torrent_copy(&self.data_dir, hash))
                            .map_err(|err| format!("The torrent file is gone: {err}"))?,
                    ),
                };
                torrents.remove(hash, false)?;
                torrents.add(source, &entry.record.folder, false)?;
            }
            if entry.record.state != Saved::Finished {
                entry.record.state = Saved::Queued;
            }
        } else if entry.record.state != Saved::Finished && entry.task.is_none() {
            entry.record.state = Saved::Queued;
        }
        self.dirty = true;
        Ok(())
    }

    /// Download a finished or failed HTTP file again, from the start.
    pub fn restart(&mut self, id: u64) {
        let Some(entry) = self.entry(id) else {
            return;
        };
        if !matches!(entry.record.source, Source::Http { .. }) {
            return;
        }
        if let Some(task) = entry.task.take() {
            task.stop();
        }
        if let Some(path) = &entry.record.path
            && entry.record.state != Saved::Finished
        {
            let _ = std::fs::remove_file(http::part_path(path));
        }
        entry.record.parts.clear();
        entry.record.path = None;
        entry.record.size = None;
        entry.record.finished = None;
        entry.record.state = Saved::Queued;
        entry.meter.reset();
        self.dirty = true;
    }

    /// Take a download off the list; `delete_files` also removes what it wrote.
    pub fn remove(&mut self, id: u64, delete_files: bool) -> Result<(), String> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.record.id == id)
            .ok_or("No such download")?;
        let entry = &mut self.entries[index];
        match &entry.record.source {
            Source::Http { .. } => {
                if let Some(task) = entry.task.take() {
                    task.stop();
                    entry.record.path = task.progress().path;
                }
                if let Some(path) = &entry.record.path {
                    let _ = std::fs::remove_file(http::part_path(path));
                    if delete_files && entry.record.state == Saved::Finished {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            Source::Torrent { hash, .. } => {
                if let Some(torrents) = &self.torrents {
                    torrents.remove(hash, delete_files)?;
                } else if delete_files {
                    return Err("The torrent engine is not ready".into());
                }
                let _ = std::fs::remove_file(torrent_copy(&self.data_dir, hash));
            }
        }
        self.entries.remove(index);
        self.dirty = true;
        self.rebuild_rows(Instant::now());
        Ok(())
    }

    /// Choose which files of a torrent download.
    pub fn set_file_wanted(&mut self, id: u64, file: usize, wanted: bool) -> Result<(), String> {
        let row = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .ok_or("No such download")?;
        let mut set: HashSet<usize> = row
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.wanted)
            .map(|(index, _)| index)
            .collect();
        if wanted {
            set.insert(file);
        } else {
            set.remove(&file);
        }
        if set.is_empty() {
            return Err("Keep at least one file".into());
        }
        let hash = match &self.entry(id).ok_or("No such download")?.record.source {
            Source::Torrent { hash, .. } => hash.clone(),
            Source::Http { .. } => return Err("Not a torrent".into()),
        };
        self.torrents
            .as_ref()
            .ok_or("The torrent engine is not ready")?
            .set_wanted(&hash, &set)
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.limiter.set(settings.down_limit_kb * 1024);
        if let Some(torrents) = &self.torrents {
            torrents.set_limits(settings.down_limit_kb * 1024, settings.up_limit_kb * 1024);
        }
        self.settings = settings;
        self.dirty = true;
    }

    /// Follow every download; call often (each frame is fine).
    pub fn tick(&mut self) -> Vec<Event> {
        let now = Instant::now();
        let mut events = Vec::new();
        self.receive_engine();
        let mut opened_torrents = Vec::new();

        for entry in &mut self.entries {
            let Some(task) = &entry.task else {
                continue;
            };
            let progress = task.progress();
            entry.record.path = progress.path.clone();
            entry.record.size = progress.total;
            entry.record.parts = progress.parts;
            entry.resumable = progress.resumable;
            if let Some(path) = &progress.path {
                entry.record.name = http::display_name(path);
            }
            match progress.phase {
                Phase::Done => {
                    entry.task = None;
                    entry.record.state = Saved::Finished;
                    entry.record.finished = Some(now_secs());
                    entry.record.parts.clear();
                    entry.meter.reset();
                    events.push(Event::Finished(entry.record.name.clone()));
                    if let Some(path) = progress.path
                        && path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
                    {
                        opened_torrents.push((path, entry.record.folder.clone()));
                    }
                    self.dirty = true;
                }
                Phase::Failed(err) => {
                    entry.task = None;
                    entry.record.state = Saved::Failed(err.clone());
                    entry.meter.reset();
                    events.push(Event::Failed(entry.record.name.clone(), err));
                    self.dirty = true;
                }
                Phase::Stopped => entry.task = None,
                Phase::Connecting | Phase::Downloading => {}
            }
        }
        // A .torrent file that finished downloading starts as a torrent.
        for (path, folder) in opened_torrents {
            if let Err(err) = self.add_torrent_file(&path, Some(folder), true) {
                events.push(Event::Failed(http::display_name(&path), err));
            }
        }
        self.schedule();
        if let Some(torrents) = &self.torrents {
            for entry in &mut self.entries {
                let Source::Torrent { hash, .. } = &entry.record.source else {
                    continue;
                };
                let Some(view) = torrents.view(hash) else {
                    continue;
                };
                if let Some(name) = &view.name {
                    entry.record.name.clone_from(name);
                }
                if view.total > 0 {
                    entry.record.size = Some(view.total);
                }
                let complete = matches!(
                    view.phase,
                    torrent::Phase::Seeding | torrent::Phase::Finished
                );
                if complete && entry.record.state != Saved::Finished {
                    entry.record.state = Saved::Finished;
                    entry.record.finished = Some(now_secs());
                    events.push(Event::Finished(entry.record.name.clone()));
                    self.dirty = true;
                }
                if view.phase == torrent::Phase::Seeding && !self.settings.seed {
                    let _ = torrents.pause(hash);
                }
            }
        }
        self.rebuild_rows(now);
        let running = self.entries.iter().any(|entry| entry.task.is_some());
        if self.dirty || (running && now.duration_since(self.saved_at) >= SAVE_EVERY) {
            self.save();
        }
        events
    }

    fn receive_engine(&mut self) {
        let Some(receiver) = &self.starting else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("The torrent engine stopped".into()),
        };
        self.starting = None;
        match result {
            Ok(torrents) => {
                torrents.set_limits(
                    self.settings.down_limit_kb * 1024,
                    self.settings.up_limit_kb * 1024,
                );
                // Torrents the engine does not remember (added before it was ready) start now.
                for entry in &mut self.entries {
                    let Source::Torrent { hash, magnet } = &entry.record.source else {
                        continue;
                    };
                    if torrents.contains(hash) || entry.record.state == Saved::Finished {
                        continue;
                    }
                    let source = match magnet {
                        Some(link) => Ok(torrent::Source::Magnet(link.clone())),
                        None => std::fs::read(torrent_copy(&self.data_dir, hash))
                            .map(torrent::Source::File)
                            .map_err(|err| format!("The torrent file is gone: {err}")),
                    };
                    let paused = entry.record.state != Saved::Queued;
                    let added = source
                        .and_then(|source| torrents.add(source, &entry.record.folder, paused));
                    if let Err(err) = added {
                        entry.record.state = Saved::Failed(err);
                    }
                }
                self.torrents = Some(torrents);
            }
            Err(err) => self.torrent_error = Some(err),
        }
    }

    /// Start waiting HTTP downloads while there is room.
    fn schedule(&mut self) {
        let mut running = self
            .entries
            .iter()
            .filter(|entry| entry.task.is_some())
            .count();
        for entry in &mut self.entries {
            if running >= self.settings.parallel.max(1) {
                break;
            }
            let Source::Http { url } = &entry.record.source else {
                continue;
            };
            if entry.task.is_some() || entry.record.state != Saved::Queued {
                continue;
            }
            let request = http::Request {
                url: url.clone(),
                folder: entry.record.folder.clone(),
                path: entry.record.path.clone(),
                connections: entry.record.connections.max(1),
                parts: entry.record.parts.clone(),
                total: entry.record.size,
            };
            entry.task = Some(HttpTask::start(
                request,
                self.agent.clone(),
                self.limiter.clone(),
            ));
            entry.meter.reset();
            running += 1;
        }
    }

    fn rebuild_rows(&mut self, now: Instant) {
        let torrents = self.torrents.as_ref();
        let engine_failed = self.torrent_error.is_some();
        let mut rows: Vec<Row> = self
            .entries
            .iter_mut()
            .map(|entry| {
                let record = &entry.record;
                let saved_status = match &record.state {
                    Saved::Queued => Status::Queued,
                    Saved::Paused => Status::Paused,
                    Saved::Finished => Status::Finished,
                    Saved::Failed(err) => Status::Failed(err.clone()),
                };
                let mut row = Row {
                    id: record.id,
                    name: record.name.clone(),
                    kind: Kind::of(&record.name),
                    torrent: matches!(record.source, Source::Torrent { .. }),
                    status: saved_status.clone(),
                    done: if record.state == Saved::Finished {
                        record.size.unwrap_or(0)
                    } else {
                        record.parts.iter().map(|part| part.pos - part.start).sum()
                    },
                    total: record.size,
                    down_bps: 0.0,
                    up_bps: 0.0,
                    eta: None,
                    peers: 0,
                    connections: 0,
                    parts: record.parts.clone(),
                    uploaded: 0,
                    resumable: entry.resumable,
                    link: String::new(),
                    folder: record.folder.clone(),
                    path: record.path.clone(),
                    added: record.added,
                    files: Vec::new(),
                };
                match &record.source {
                    Source::Http { url } => {
                        row.link.clone_from(url);
                        if let Some(task) = &entry.task {
                            let progress = task.progress();
                            row.status = match progress.phase {
                                Phase::Connecting => Status::Connecting,
                                _ => Status::Downloading,
                            };
                            row.done = progress.done;
                            row.connections = progress.connections;
                            row.parts = progress.parts;
                            row.down_bps = entry.meter.sample(now, progress.done);
                            row.eta = row.total.filter(|_| row.down_bps > 1.0).map(|total| {
                                (total.saturating_sub(row.done) as f64 / row.down_bps) as u64
                            });
                        }
                    }
                    Source::Torrent { hash, magnet } => {
                        row.link = magnet.clone().unwrap_or_else(|| hash.clone());
                        match torrents.and_then(|torrents| torrents.view(hash)) {
                            Some(view) => {
                                row.status = match view.phase {
                                    torrent::Phase::Resolving => Status::Resolving,
                                    torrent::Phase::Checking => Status::Checking,
                                    torrent::Phase::Downloading => Status::Downloading,
                                    torrent::Phase::Seeding => Status::Seeding,
                                    torrent::Phase::Paused => Status::Paused,
                                    torrent::Phase::Finished => Status::Finished,
                                    torrent::Phase::Failed(err) => Status::Failed(err),
                                };
                                row.done = view.done;
                                row.total = (view.total > 0).then_some(view.total).or(record.size);
                                row.down_bps = view.down_bps;
                                row.up_bps = view.up_bps;
                                row.eta = view.eta;
                                row.peers = view.peers;
                                row.uploaded = view.uploaded;
                                row.folder = view.folder.clone();
                                row.path = view.files.first().map(|file| {
                                    if view.files.len() == 1 {
                                        view.folder.join(&file.path)
                                    } else {
                                        view.folder.clone()
                                    }
                                });
                                row.files = view.files;
                            }
                            None if torrents.is_none() && !engine_failed => {
                                if !matches!(saved_status, Status::Finished | Status::Paused) {
                                    row.status = Status::Starting;
                                }
                            }
                            None => {}
                        }
                    }
                }
                row
            })
            .collect();
        rows.sort_by(|a, b| b.added.cmp(&a.added).then(b.id.cmp(&a.id)));
        self.rows = rows;
    }

    pub fn save(&mut self) {
        self.dirty = false;
        self.saved_at = Instant::now();
        let store = Store {
            settings: self.settings.clone(),
            next_id: self.next_id,
            records: self
                .entries
                .iter()
                .map(|entry| entry.record.clone())
                .collect(),
        };
        let result = serde_json::to_string_pretty(&store)
            .map_err(|err| err.to_string())
            .and_then(|text| write_atomic(&self.data_dir.join(STORE_FILE), text.as_bytes()));
        if let Err(err) = result {
            self.save_error = Some(format!("Cannot save the list of downloads: {err}"));
        }
    }

    /// Stop every connection, keep where each download is, and close the torrent engine.
    pub fn shutdown(&mut self) {
        for entry in &mut self.entries {
            if let Some(task) = entry.task.take() {
                task.stop();
                let progress = task.progress();
                entry.record.parts = progress.parts;
                entry.record.path = progress.path;
                entry.record.size = progress.total;
            }
        }
        self.save();
        if let Some(torrents) = self.torrents.take() {
            torrents.stop();
        }
    }

    /// Total download and upload speed.
    pub fn speeds(&self) -> (f64, f64) {
        self.rows.iter().fold((0.0, 0.0), |(down, up), row| {
            (down + row.down_bps, up + row.up_bps)
        })
    }
}

/// Copy of a `.torrent` file, kept in the data folder.
fn torrent_copy(data_dir: &Path, hash: &str) -> PathBuf {
    data_dir.join("torrents").join(format!("{hash}.torrent"))
}

fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let temp = target.with_extension("json.tmp");
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, target))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("godmanager-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn links_are_sorted_into_http_and_torrents() {
        let dir = temp_dir("links");
        let mut manager = Manager::load(dir.clone(), "test");
        assert!(
            manager
                .add_link("ftp://example.com/a", None, false)
                .is_err()
        );
        let id = manager
            .add_link("https://example.com/a/file.zip", None, false)
            .unwrap();
        let hash = "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c";
        let torrent = manager.add_link(hash, None, false).unwrap();
        assert!(
            manager
                .add_link(&format!("magnet:?xt=urn:btih:{hash}"), None, false)
                .is_err()
        );
        manager.save();
        let again = Manager::load(dir.clone(), "test");
        assert_eq!(again.entries.len(), 2);
        let http = again
            .entries
            .iter()
            .find(|entry| entry.record.id == id)
            .unwrap();
        assert_eq!(http.record.name, "file.zip");
        assert_eq!(http.record.state, Saved::Paused);
        let row = again.rows().iter().find(|row| row.id == torrent).unwrap();
        assert!(row.torrent);
        let _ = std::fs::remove_dir_all(dir);
    }
}
