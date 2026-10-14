//! Human-readable sizes, speeds and durations.

/// `1536` → `1.5 KB`. Powers of 1024, as Windows Explorer shows them.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

/// Bytes per second → `2.4 MB/s`.
pub fn speed(bytes_per_second: f64) -> String {
    format!("{}/s", size(bytes_per_second.max(0.0) as u64))
}

/// Seconds → `1 h 05 min`, `3 min 20 s`, `12 s`.
pub fn duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds} s"),
        (0, _) => format!("{minutes} min {seconds:02} s"),
        (hours, _) if hours >= 48 => format!("{} days", hours / 24),
        _ => format!("{hours} h {minutes:02} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(250 * 1024 * 1024), "250 MB");
        assert_eq!(size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(12), "12 s");
        assert_eq!(duration(200), "3 min 20 s");
        assert_eq!(duration(3900), "1 h 05 min");
        assert_eq!(duration(5 * 86400), "5 days");
    }
}
