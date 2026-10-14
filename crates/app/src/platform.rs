//! Files and folders of the system: pickers, Explorer, the data folder, link handlers.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `%APPDATA%\GodManager`: the list of downloads and the torrent engine state.
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::APP_NAME)
}

pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    rfd::FileDialog::new().set_directory(start).pick_folder()
}

pub fn pick_torrents() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Torrent files", &["torrent"])
        .pick_files()
        .unwrap_or_default()
}

/// Open a file with its program, or a folder in Explorer.
pub fn open(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{} is not there any more", path.display()));
    }
    let opened = if cfg!(windows) {
        Command::new("explorer").arg(path).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(path).spawn()
    } else {
        Command::new("xdg-open").arg(path).spawn()
    };
    opened
        .map(|_| ())
        .map_err(|err| format!("Cannot open {}: {err}", path.display()))
}

/// Show a file selected in its folder; a missing file opens the folder it was meant for.
pub fn reveal(path: &Path, folder: &Path) -> Result<(), String> {
    if !path.exists() {
        return open(folder);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Explorer wants the quotes around the path only, not around the whole argument.
        Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("Cannot show {}: {err}", path.display()))
    }
    #[cfg(not(windows))]
    open(path.parent().unwrap_or(folder))
}

/// Make this program the one that opens magnet links and .torrent files (current user only).
#[cfg(windows)]
pub fn associate() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| format!("Cannot find the program: {err}"))?;
    let exe = exe.display().to_string();
    let command = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let classes = r"HKCU\Software\Classes";
    let entries: [(String, Option<&str>, &str); 9] = [
        (format!(r"{classes}\magnet"), None, "URL:Magnet link"),
        (format!(r"{classes}\magnet"), Some("URL Protocol"), ""),
        (format!(r"{classes}\magnet\DefaultIcon"), None, &icon),
        (
            format!(r"{classes}\magnet\shell\open\command"),
            None,
            &command,
        ),
        (format!(r"{classes}\.torrent"), None, "GodManager.torrent"),
        (
            format!(r"{classes}\.torrent"),
            Some("Content Type"),
            "application/x-bittorrent",
        ),
        (
            format!(r"{classes}\GodManager.torrent"),
            None,
            "Torrent file",
        ),
        (
            format!(r"{classes}\GodManager.torrent\DefaultIcon"),
            None,
            &icon,
        ),
        (
            format!(r"{classes}\GodManager.torrent\shell\open\command"),
            None,
            &command,
        ),
    ];
    for (key, name, value) in entries {
        let mut reg = Command::new("reg");
        reg.args(["add", &key, "/f", "/t", "REG_SZ", "/d", value]);
        match name {
            Some(name) => reg.args(["/v", name]),
            None => reg.arg("/ve"),
        };
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let status = reg
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|err| format!("Cannot write to the registry: {err}"))?;
        if !status.success() {
            return Err(format!("Cannot write {key} to the registry"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn associate() -> Result<(), String> {
    Err("Only on Windows".into())
}
