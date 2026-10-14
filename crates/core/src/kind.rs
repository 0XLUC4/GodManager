//! What a file is, from its extension: drives the icon and the sidebar categories.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Music,
    Picture,
    Document,
    Archive,
    Program,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Video,
        Kind::Music,
        Kind::Picture,
        Kind::Document,
        Kind::Archive,
        Kind::Program,
        Kind::Other,
    ];

    pub fn of(name: &str) -> Kind {
        let extension = Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is = |list: &[&str]| list.contains(&extension.as_str());
        if is(&[
            "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v", "mpg", "mpeg", "ts", "3gp",
        ]) {
            Kind::Video
        } else if is(&[
            "mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma", "alac",
        ]) {
            Kind::Music
        } else if is(&[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "tif", "tiff", "heic", "ico",
        ]) {
            Kind::Picture
        } else if is(&[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "txt", "md", "epub",
            "csv", "rtf",
        ]) {
            Kind::Document
        } else if is(&[
            "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "iso", "tgz", "cab",
        ]) {
            Kind::Archive
        } else if is(&[
            "exe", "msi", "msix", "appx", "bat", "cmd", "ps1", "apk", "dmg", "deb", "rpm",
            "appimage",
        ]) {
            Kind::Program
        } else {
            Kind::Other
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Kind::Video => "Video",
            Kind::Music => "Music",
            Kind::Picture => "Pictures",
            Kind::Document => "Documents",
            Kind::Archive => "Archives",
            Kind::Program => "Programs",
            Kind::Other => "Other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_extensions() {
        assert_eq!(Kind::of("movie.MKV"), Kind::Video);
        assert_eq!(Kind::of("setup.exe"), Kind::Program);
        assert_eq!(Kind::of("ubuntu.iso"), Kind::Archive);
        assert_eq!(Kind::of("README"), Kind::Other);
    }
}
