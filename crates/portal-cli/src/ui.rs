//! Terminal output: progress bar, sizes and messages.

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use console::style;
use indicatif::{
    DecimalBytes, HumanCount, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle,
};
use portal_net::{Event, Reporter, Route};

/// Which side of the transfer this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Send,
    Receive,
}

/// "1,284 files, 2.3 GB" (or just the size for a single file).
pub fn describe(files: u64, bytes: u64, single_file: bool) -> String {
    if single_file {
        return DecimalBytes(bytes).to_string();
    }
    let noun = if files == 1 { "file" } else { "files" };
    format!("{} {noun}, {}", HumanCount(files), DecimalBytes(bytes))
}

fn route_label(route: Route) -> String {
    match route {
        Route::Direct(addr) => format!("{} {}", addr.ip(), style("[direct]").green()),
        Route::Relay => style("[relay]").yellow().to_string(),
        Route::Unknown => String::new(),
    }
}

/// A progress bar driven by [`Event`]s; hidden until data starts flowing.
pub struct Progress {
    bar: ProgressBar,
    /// When data started flowing.
    started: Arc<Mutex<Option<Instant>>>,
}

/// How a finished transfer went.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    elapsed: Duration,
    bytes: u64,
}

impl Stats {
    /// " in 3.2s (48.1 MB/s)", or " (already up to date)" if no data had to move.
    pub fn suffix(stats: Option<Self>) -> String {
        let Some(Self { elapsed, bytes }) = stats else {
            return String::new();
        };
        if bytes == 0 {
            return " (already up to date)".into();
        }
        let secs = elapsed.as_secs_f64();
        let speed = if secs > 0.0 {
            format!(" ({}/s)", DecimalBytes((bytes as f64 / secs) as u64))
        } else {
            String::new()
        };
        format!(" in {}{speed}", duration(elapsed))
    }
}

/// "0.4s", "42.0s", "3m 07s", "1h 05m".
fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{:.1}s", d.as_secs_f64())
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    }
}

/// A bar for reading `total_bytes` of files before sending them; hidden
/// until it is given a draw target.
pub fn reading_bar(total_bytes: u64) -> ProgressBar {
    let style = ProgressStyle::with_template(
        "Reading files [{bar:20}] {percent:>3}%  {decimal_bytes_per_sec:>10}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("█░");
    let bar = ProgressBar::hidden();
    bar.set_style(style);
    bar.set_length(total_bytes);
    bar
}

impl Progress {
    pub fn new() -> Self {
        let style = ProgressStyle::with_template(
            "[{bar:20}] {percent:>3}%  {decimal_bytes_per_sec:>10}  ETA {eta_short}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .with_key(
            "eta_short",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                let secs = state.eta().as_secs();
                let _ = write!(w, "{}:{:02}", secs / 60, secs % 60);
            },
        )
        .progress_chars("█░");
        let bar = ProgressBar::hidden();
        bar.set_style(style);
        Self {
            bar,
            started: Arc::new(Mutex::new(None)),
        }
    }

    /// A [`Reporter`] that updates this bar and prints milestones for `role`.
    pub fn reporter(&self, role: Role) -> Reporter {
        let bar = self.bar.clone();
        let started = self.started.clone();
        let route = Arc::new(Mutex::new(Route::Unknown));
        let say = move |bar: &ProgressBar, line: String| bar.suspend(|| println!("{line}"));
        Arc::new(move |event| match event {
            Event::Connected { route: r, .. } => {
                if let Ok(mut current) = route.lock() {
                    *current = r;
                }
                if role == Role::Send {
                    say(&bar, format!("Receiver connected {}", route_label(r)));
                }
            }
            Event::Offer(offer) => {
                let r = route.lock().map(|r| *r).unwrap_or(Route::Unknown);
                let from = match r {
                    Route::Unknown => String::new(),
                    _ => format!(" from {}", route_label(r)),
                };
                say(
                    &bar,
                    format!(
                        "Receiving {} ({}){from}",
                        style(format!("\"{}\"", offer.name)).bold(),
                        describe(offer.files, offer.bytes, false),
                    ),
                );
            }
            Event::Started {
                total_bytes,
                skipped_bytes,
            } => {
                if skipped_bytes > 0 {
                    let why = match role {
                        Role::Send => "already on the receiver or repeated",
                        Role::Receive => "already here or repeated",
                    };
                    say(
                        &bar,
                        format!("{} {why}, not sent again", DecimalBytes(skipped_bytes)),
                    );
                }
                if let Ok(mut started) = started.lock() {
                    *started = Some(Instant::now());
                }
                bar.reset_elapsed();
                bar.set_length(total_bytes);
                bar.set_draw_target(ProgressDrawTarget::stderr());
            }
            Event::Progress { bytes } => bar.inc(bytes),
        })
    }

    /// Hides the bar; returns how the transfer went if data started flowing.
    pub fn finish(&self) -> Option<Stats> {
        self.bar.finish_and_clear();
        let started = (*self.started.lock().ok()?)?;
        Some(Stats {
            elapsed: started.elapsed(),
            bytes: self.bar.position(),
        })
    }
}

/// Whether there is a person at the terminal who can answer questions.
pub fn can_ask() -> bool {
    std::io::stdin().is_terminal() && console::Term::stderr().is_term()
}

/// Asks a yes/no question; Enter means yes. Anything unreadable means no.
pub fn confirm(question: &str) -> bool {
    let term = console::Term::stderr();
    if term.write_str(&format!("{question} [Y/n] ")).is_err() {
        return false;
    }
    match term.read_line() {
        Ok(answer) => matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes"),
        Err(_) => false,
    }
}

pub fn success(message: impl std::fmt::Display) {
    println!("{} {message}", style("✓").green().bold());
}

pub fn warn(message: impl std::fmt::Display) {
    eprintln!("{} {message}", style("warning:").yellow().bold());
}

/// A check result line for `portal doctor`: ✓, ! or ✗ and the message.
pub fn check(level: Level, message: impl std::fmt::Display) {
    let mark = match level {
        Level::Ok => style("✓").green().bold(),
        Level::Warn => style("!").yellow().bold(),
        Level::Fail => style("✗").red().bold(),
    };
    println!("{mark} {message}");
}

/// How a check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

/// Keeps a terminal window open until Enter is pressed, for windows that
/// close as soon as Portal exits (started from the file manager's menu).
pub fn pause() {
    if can_ask() {
        let term = console::Term::stderr();
        let _ = term.write_str("\nPress Enter to close this window.");
        let _ = term.read_line();
    }
}

/// Makes untrusted text safe to print to a terminal: line breaks and tabs
/// stay, other control characters (which could drive the terminal) are
/// shown escaped.
pub fn terminal_safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Parses a speed such as `10MB/s`, `500k`, `2 MiB/s` or `1000000` (bytes
/// per second). k/M/G are decimal, Ki/Mi/Gi binary; the "B" and "/s" are
/// optional.
pub fn parse_rate(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase().replace(' ', "");
    let unit = lower.strip_suffix("/s").unwrap_or(&lower);
    let unit = unit.strip_suffix('b').unwrap_or(unit);
    let split = unit
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(unit.len());
    let (number, suffix) = unit.split_at(split);
    let multiplier: f64 = match suffix {
        "" => 1.0,
        "k" => 1e3,
        "m" => 1e6,
        "g" => 1e9,
        "ki" => 1024.0,
        "mi" => 1024.0 * 1024.0,
        "gi" => 1024.0 * 1024.0 * 1024.0,
        _ => {
            return Err(format!(
                "unknown unit in \"{text}\"; use e.g. 500k or 10MB/s"
            ));
        }
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("\"{text}\" is not a speed; use e.g. 500k or 10MB/s"))?;
    let bytes = value * multiplier;
    if !(1000.0..=1e15).contains(&bytes) {
        return Err("the speed limit must be at least 1 kB/s".into());
    }
    Ok(bytes as u64)
}

/// A speed for display, e.g. "10.00 MB/s".
pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", DecimalBytes(bytes_per_sec))
}

/// This computer's name, to tell paired devices apart.
pub fn hostname() -> String {
    let from_env = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();
    let name = from_env.or_else(|| {
        let out = std::process::Command::new("hostname").output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    });
    let name = name.unwrap_or_default();
    let name = name.trim();
    let name = name.strip_suffix(".local").unwrap_or(name);
    if name.is_empty() {
        "this device".to_owned()
    } else {
        name.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_sizes_like_the_readme() {
        assert_eq!(describe(1284, 2_300_000_000, false), "1,284 files, 2.30 GB");
        assert_eq!(describe(1, 5, false), "1 file, 5 B");
        assert_eq!(describe(1, 5, true), "5 B");
    }

    #[test]
    fn parses_speed_limits() {
        assert_eq!(parse_rate("10MB/s"), Ok(10_000_000));
        assert_eq!(parse_rate("500k"), Ok(500_000));
        assert_eq!(parse_rate("1.5 M"), Ok(1_500_000));
        assert_eq!(parse_rate("2MiB/s"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_rate("250000"), Ok(250_000));
        assert_eq!(parse_rate("1g"), Ok(1_000_000_000));
        assert!(parse_rate("10 apples").is_err());
        assert!(parse_rate("fast").is_err());
        assert!(parse_rate("10").is_err(), "below 1 kB/s");
        assert_eq!(rate(10_000_000), "10.00 MB/s");
    }

    #[test]
    fn untrusted_text_cannot_drive_the_terminal() {
        assert_eq!(terminal_safe("a\tb\nc"), "a\tb\nc");
        assert_eq!(terminal_safe("\x1b[2Jboom\r"), "\\u{1b}[2Jboom\\r");
    }

    #[test]
    fn formats_durations_and_speed() {
        assert_eq!(duration(Duration::from_millis(400)), "0.4s");
        assert_eq!(duration(Duration::from_secs(187)), "3m 07s");
        assert_eq!(duration(Duration::from_secs(3900)), "1h 05m");
        let stats = Stats {
            elapsed: Duration::from_secs(2),
            bytes: 10_000_000,
        };
        assert_eq!(Stats::suffix(Some(stats)), " in 2.0s (5.00 MB/s)");
        let nothing = Stats {
            elapsed: Duration::from_secs(1),
            bytes: 0,
        };
        assert_eq!(Stats::suffix(Some(nothing)), " (already up to date)");
        assert_eq!(Stats::suffix(None), "");
    }
}
