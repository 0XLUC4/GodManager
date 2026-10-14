//! BitTorrent through librqbit: magnet links and .torrent files, DHT, trackers, resume.
//! The engine runs on its own tokio runtime; everything here is called from plain threads.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerOptions, Magnet, ManagedTorrent, Session,
    SessionOptions, SessionPersistenceConfig, TorrentStatsState, api::TorrentIdOrHash, dht::Id20,
};

/// Where a torrent comes from.
#[derive(Clone, Debug)]
pub enum Source {
    Magnet(String),
    File(Vec<u8>),
}

/// What the infohash and the name of a source are, without starting it.
pub struct Identity {
    pub hash: String,
    pub name: Option<String>,
    /// More than one file: they go to a folder of their own.
    pub multi_file: Option<bool>,
}

pub fn identify(source: &Source) -> Result<Identity, String> {
    match source {
        Source::Magnet(link) => {
            let magnet = Magnet::parse(link).map_err(|err| format!("Not a magnet link: {err}"))?;
            let hash = magnet
                .as_id20()
                .ok_or("The magnet link has no BitTorrent v1 hash")?;
            let name = link
                .split(['?', '&'])
                .find_map(|param| param.strip_prefix("dn="))
                .map(|name| {
                    percent_encoding::percent_decode_str(&name.replace('+', " "))
                        .decode_utf8_lossy()
                        .into_owned()
                });
            Ok(Identity {
                hash: hash.as_string(),
                name,
                multi_file: None,
            })
        }
        Source::File(bytes) => {
            let torrent = librqbit::torrent_from_bytes(bytes)
                .map_err(|err| format!("Not a torrent file: {err}"))?;
            let info = &torrent.info.data;
            let name = info
                .name
                .as_ref()
                .map(|name| String::from_utf8_lossy(name.as_ref()).into_owned());
            Ok(Identity {
                hash: torrent.info_hash.as_string(),
                name,
                multi_file: Some(info.files.is_some()),
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Asking peers for the list of files (magnet link).
    Resolving,
    /// Checking what is on the disk already.
    Checking,
    Downloading,
    /// Done and sharing with others.
    Seeding,
    Paused,
    /// Done and stopped.
    Finished,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub done: u64,
    pub wanted: bool,
}

#[derive(Clone, Debug)]
pub struct View {
    pub name: Option<String>,
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub uploaded: u64,
    pub down_bps: f64,
    pub up_bps: f64,
    pub eta: Option<u64>,
    pub peers: usize,
    pub folder: PathBuf,
    pub files: Vec<File>,
}

pub struct Torrents {
    runtime: tokio::runtime::Runtime,
    session: Arc<Session>,
    /// Torrents being added, by hash: `Some(error)` once adding failed.
    adding: Arc<Mutex<HashMap<String, Option<String>>>>,
    default_folder: PathBuf,
}

impl Torrents {
    /// Starts the engine. `data_dir` keeps the list of torrents and what each has done.
    pub fn start(data_dir: &Path, default_folder: PathBuf, client: &str) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("torrents")
            .build()
            .map_err(|err| format!("Cannot start the torrent engine: {err}"))?;
        let options = SessionOptions {
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(data_dir.to_path_buf()),
            }),
            listen: Some(ListenerOptions {
                enable_upnp_port_forwarding: true,
                ..Default::default()
            }),
            client_name_and_version: Some(client.to_owned()),
            ..Default::default()
        };
        let session = runtime
            .block_on(Session::new_with_opts(default_folder.clone(), options))
            .map_err(|err| format!("Cannot start the torrent engine: {err:#}"))?;
        Ok(Self {
            runtime,
            session,
            adding: Arc::default(),
            default_folder,
        })
    }

    fn handle(&self, hash: &str) -> Option<Arc<ManagedTorrent>> {
        let id = hash.parse::<Id20>().ok()?;
        self.session.get(TorrentIdOrHash::Hash(id))
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.handle(hash).is_some()
            || self
                .adding
                .lock()
                .is_ok_and(|adding| adding.get(hash).is_some_and(Option::is_none))
    }

    /// Adds in the background: a magnet link first needs the list of files from peers.
    pub fn add(&self, source: Source, folder: &Path, paused: bool) -> Result<String, String> {
        let identity = identify(&source)?;
        let hash = identity.hash.clone();
        if self.contains(&hash) {
            return Ok(hash);
        }
        let output_folder = (folder != self.default_folder).then(|| {
            let own_folder = identity.multi_file.unwrap_or(true);
            let name = identity.name.as_deref().map(crate::http::sanitize);
            match (own_folder, name) {
                (true, Some(name)) => folder.join(name),
                (true, None) => folder.join(&hash),
                (false, _) => folder.to_path_buf(),
            }
        });
        let options = AddTorrentOptions {
            paused,
            // Pieces already on the disk are checked and kept: an interrupted torrent goes on.
            overwrite: true,
            output_folder: output_folder.map(|folder| folder.to_string_lossy().into_owned()),
            ..Default::default()
        };
        if let Ok(mut adding) = self.adding.lock() {
            adding.insert(hash.clone(), None);
        }
        let session = self.session.clone();
        let adding = self.adding.clone();
        let key = hash.clone();
        self.runtime.spawn(async move {
            let add = match source {
                Source::Magnet(link) => AddTorrent::from_url(link),
                Source::File(bytes) => AddTorrent::from_bytes(bytes),
            };
            let result = session.add_torrent(add, Some(options)).await;
            if let Ok(mut adding) = adding.lock() {
                match result {
                    Ok(_) => {
                        adding.remove(&key);
                    }
                    Err(err) => {
                        adding.insert(key, Some(format!("{err:#}")));
                    }
                }
            }
        });
        Ok(hash)
    }

    pub fn view(&self, hash: &str) -> Option<View> {
        let Some(handle) = self.handle(hash) else {
            let adding = self.adding.lock().ok()?;
            let error = adding.get(hash)?.clone();
            return Some(View {
                name: None,
                phase: error.map_or(Phase::Resolving, Phase::Failed),
                done: 0,
                total: 0,
                uploaded: 0,
                down_bps: 0.0,
                up_bps: 0.0,
                eta: None,
                peers: 0,
                folder: self.default_folder.clone(),
                files: Vec::new(),
            });
        };
        let stats = handle.stats();
        let phase = match stats.state {
            TorrentStatsState::Initializing { .. } => Phase::Checking,
            TorrentStatsState::Live if stats.finished => Phase::Seeding,
            TorrentStatsState::Live => Phase::Downloading,
            TorrentStatsState::Paused if stats.finished => Phase::Finished,
            TorrentStatsState::Paused => Phase::Paused,
            TorrentStatsState::Error => Phase::Failed(
                stats
                    .error
                    .clone()
                    .unwrap_or_else(|| "Unknown error".into()),
            ),
        };
        let wanted: Option<HashSet<usize>> =
            handle.only_files().map(|files| files.into_iter().collect());
        let files = handle
            .with_metadata(|metadata| {
                metadata
                    .file_infos
                    .iter()
                    .enumerate()
                    .map(|(index, info)| File {
                        path: info.relative_filename.to_string_lossy().into_owned(),
                        size: info.len,
                        done: stats.file_progress.get(index).copied().unwrap_or(0),
                        wanted: wanted.as_ref().is_none_or(|set| set.contains(&index)),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let live = stats.live.as_ref();
        Some(View {
            name: handle.name(),
            phase,
            done: stats.progress_bytes,
            total: stats.total_bytes,
            uploaded: stats.uploaded_bytes,
            down_bps: live.map_or(0.0, |live| live.download_speed.mbps * 1024.0 * 1024.0),
            up_bps: live.map_or(0.0, |live| live.upload_speed.mbps * 1024.0 * 1024.0),
            eta: live
                .filter(|live| live.download_speed.mbps > 0.0 && !stats.finished)
                .map(|live| {
                    let left = stats.total_bytes.saturating_sub(stats.progress_bytes) as f64;
                    (left / (live.download_speed.mbps * 1024.0 * 1024.0)) as u64
                }),
            peers: live.map_or(0, |live| live.snapshot.peer_stats.live as usize),
            folder: handle.output_folder().to_path_buf(),
            files,
        })
    }

    pub fn pause(&self, hash: &str) -> Result<(), String> {
        let handle = self.handle(hash).ok_or("This torrent is not loaded")?;
        if handle.is_paused() {
            return Ok(());
        }
        self.runtime
            .block_on(self.session.pause(&handle))
            .map_err(|err| format!("Cannot pause: {err:#}"))
    }

    pub fn resume(&self, hash: &str) -> Result<(), String> {
        let handle = self.handle(hash).ok_or("This torrent is not loaded")?;
        if !handle.is_paused() {
            return Ok(());
        }
        self.runtime
            .block_on(self.session.unpause(&handle))
            .map_err(|err| format!("Cannot resume: {err:#}"))
    }

    pub fn remove(&self, hash: &str, delete_files: bool) -> Result<(), String> {
        if let Ok(mut adding) = self.adding.lock() {
            adding.remove(hash);
        }
        let Ok(id) = hash.parse::<Id20>() else {
            return Ok(());
        };
        if self.session.get(TorrentIdOrHash::Hash(id)).is_none() {
            return Ok(());
        }
        self.runtime
            .block_on(self.session.delete(TorrentIdOrHash::Hash(id), delete_files))
            .map_err(|err| format!("Cannot remove: {err:#}"))
    }

    /// Download only the files at these indexes.
    pub fn set_wanted(&self, hash: &str, files: &HashSet<usize>) -> Result<(), String> {
        let handle = self.handle(hash).ok_or("This torrent is not loaded")?;
        self.runtime
            .block_on(self.session.update_only_files(&handle, files))
            .map_err(|err| format!("Cannot change the files: {err:#}"))
    }

    /// Writes what each torrent has done, then closes every connection.
    pub fn stop(self) {
        self.runtime.block_on(self.session.stop());
    }

    /// Bytes per second; `0` is no limit.
    pub fn set_limits(&self, down: u64, up: u64) {
        let rate = |bps: u64| NonZeroU32::new(u32::try_from(bps).unwrap_or(u32::MAX));
        self.session.ratelimits.set_download_bps(rate(down));
        self.session.ratelimits.set_upload_bps(rate(up));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnet_identity() {
        let link = "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Fexplodie.org%3A6969";
        let identity = identify(&Source::Magnet(link.into())).unwrap();
        assert_eq!(identity.hash, "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c");
        assert_eq!(identity.name.as_deref(), Some("Big Buck Bunny"));
        assert!(identify(&Source::Magnet("magnet:?dn=x".into())).is_err());
    }
}
