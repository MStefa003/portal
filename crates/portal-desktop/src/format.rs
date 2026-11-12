//! Sizes, counts and times in the words the window shows.

use std::time::Duration;

/// "1.28 GB", in decimal units like the command line tool.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// "1,284".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// "1,284 files, 2.30 GB" (or just the size for a single file).
pub fn describe(files: u64, bytes_total: u64, single_file: bool) -> String {
    if single_file {
        return bytes(bytes_total);
    }
    let noun = if files == 1 { "file" } else { "files" };
    format!("{} {noun}, {}", count(files), bytes(bytes_total))
}

/// "1:05", or "1:02:05" past an hour.
pub fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_command_line_tool() {
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(5_000_000), "5.00 MB");
        assert_eq!(bytes(2_300_000_000), "2.30 GB");
        assert_eq!(count(1284), "1,284");
        assert_eq!(count(1_000_000), "1,000,000");
        assert_eq!(count(12), "12");
        assert_eq!(describe(1, 10, false), "1 file, 10 B");
        assert_eq!(describe(2, 5_000_000, true), "5.00 MB");
        assert_eq!(duration(Duration::from_secs(65)), "1:05");
        assert_eq!(duration(Duration::from_secs(3725)), "1:02:05");
    }
}
