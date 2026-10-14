//! Real downloads from a small local server that serves byte ranges.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use godmanager_core::http::{self, HttpTask, Limiter, Phase, Request};

const SIZE: usize = 6 * 1024 * 1024 + 123;

fn content() -> Vec<u8> {
    (0..SIZE).map(|index| (index * 7 % 251) as u8).collect()
}

/// Serves `content()` as `/file.bin`, with ranges unless `ranges` is false.
/// `slow` adds a pause per answer so a test can stop in the middle.
fn serve(ranges: bool, slow: Arc<AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let body = Arc::new(content());
        for stream in listener.incoming().flatten() {
            let body = body.clone();
            let slow = slow.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (start, end) = value.trim().split_once('-').unwrap();
                        let start: usize = start.parse().unwrap();
                        let end = end
                            .parse::<usize>()
                            .map_or(SIZE - 1, |end| end.min(SIZE - 1));
                        range = Some((start, end));
                    }
                    line.clear();
                }
                let mut stream = stream;
                let (status, slice, extra) = match range.filter(|_| ranges) {
                    Some((start, end)) => (
                        "206 Partial Content",
                        &body[start..=end],
                        format!("Content-Range: bytes {start}-{end}/{SIZE}\r\n"),
                    ),
                    None => ("200 OK", &body[..], String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    slice.len()
                );
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in slice.chunks(256 * 1024) {
                    if slow.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    if stream.write_all(chunk).is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{address}/file.bin")
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("godmanager-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn wait(task: &HttpTask, until: impl Fn(&Phase) -> bool) -> http::Progress {
    let start = Instant::now();
    loop {
        let progress = task.progress();
        if until(&progress.phase) {
            return progress;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "stuck: {:?}",
            progress.phase
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn request(url: &str, folder: &PathBuf) -> Request {
    Request {
        url: url.to_owned(),
        folder: folder.clone(),
        path: None,
        connections: 8,
        parts: Vec::new(),
        total: None,
    }
}

#[test]
fn downloads_with_many_connections() {
    let url = serve(true, Arc::default());
    let dir = folder("many");
    let task = HttpTask::start(request(&url, &dir), http::agent("test"), Arc::default());
    let progress = wait(&task, |phase| {
        matches!(phase, Phase::Done | Phase::Failed(_))
    });
    assert_eq!(progress.phase, Phase::Done);
    assert!(progress.resumable);
    let path = progress.path.unwrap();
    assert_eq!(path, dir.join("file.bin"));
    assert_eq!(std::fs::read(&path).unwrap(), content());
    assert!(!http::part_path(&path).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn downloads_from_a_server_without_ranges() {
    let url = serve(false, Arc::default());
    let dir = folder("plain");
    let task = HttpTask::start(request(&url, &dir), http::agent("test"), Arc::default());
    let progress = wait(&task, |phase| {
        matches!(phase, Phase::Done | Phase::Failed(_))
    });
    assert_eq!(progress.phase, Phase::Done);
    assert!(!progress.resumable);
    assert_eq!(std::fs::read(progress.path.unwrap()).unwrap(), content());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pauses_and_resumes_where_it_stopped() {
    let slow = Arc::new(AtomicBool::new(true));
    let url = serve(true, slow.clone());
    let dir = folder("resume");
    let limiter = Arc::new(Limiter::default());
    let task = HttpTask::start(request(&url, &dir), http::agent("test"), limiter.clone());
    let start = Instant::now();
    while task.progress().done < 512 * 1024 {
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(10));
    }
    task.stop();
    let paused = task.progress();
    assert_eq!(paused.phase, Phase::Stopped);
    assert!(paused.done < SIZE as u64);
    // Let the stopped connections notice before the resume writes the same file.
    std::thread::sleep(Duration::from_millis(300));
    slow.store(false, Ordering::Relaxed);
    let resumed = HttpTask::start(
        Request {
            path: paused.path.clone(),
            parts: task.progress().parts,
            total: paused.total,
            ..request(&url, &dir)
        },
        http::agent("test"),
        limiter,
    );
    let progress = wait(&resumed, |phase| {
        matches!(phase, Phase::Done | Phase::Failed(_))
    });
    assert_eq!(progress.phase, Phase::Done);
    assert_eq!(
        progress.path, paused.path,
        "the resume writes the same file"
    );
    assert_eq!(std::fs::read(progress.path.unwrap()).unwrap(), content());
    let _ = std::fs::remove_dir_all(dir);
}
