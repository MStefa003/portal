//! Terminal output: progress bar, sizes and messages.

use std::sync::{Arc, Mutex};

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
        Self { bar }
    }

    /// A [`Reporter`] that updates this bar and prints milestones for `role`.
    pub fn reporter(&self, role: Role) -> Reporter {
        let bar = self.bar.clone();
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
                    say(
                        &bar,
                        format!("{} skipped (duplicate data)", DecimalBytes(skipped_bytes)),
                    );
                }
                bar.set_length(total_bytes);
                bar.set_draw_target(ProgressDrawTarget::stderr());
            }
            Event::Progress { bytes } => bar.inc(bytes),
        })
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

pub fn success(message: impl std::fmt::Display) {
    println!("{} {message}", style("✓").green().bold());
}

pub fn warn(message: impl std::fmt::Display) {
    eprintln!("{} {message}", style("warning:").yellow().bold());
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
}
