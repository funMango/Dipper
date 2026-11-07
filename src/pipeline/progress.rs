//! Progress for one dip: a terminal bar, or one log line per row boundary.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

pub trait Progress {
    fn start(&mut self, dip: &str, total: Option<u64>);
    fn advance(&mut self, rows: u64);
    fn finish(&mut self);
}

/// Non-interactive progress. Crossing several boundaries in one chunk still prints one line.
pub struct LogProgress<W: Write> {
    out: W,
    every: u64,
    next_at: u64,
    started: Instant,
    dip: String,
    total: Option<u64>,
}

impl<W: Write> LogProgress<W> {
    pub fn new(out: W, every: u64) -> Self {
        Self {
            out,
            every,
            next_at: every.max(1),
            started: Instant::now(),
            dip: String::new(),
            total: None,
        }
    }

    pub fn writer(&self) -> &W {
        &self.out
    }
}

impl<W: Write> Progress for LogProgress<W> {
    fn start(&mut self, dip: &str, total: Option<u64>) {
        self.dip = dip.to_string();
        self.total = total;
        self.started = Instant::now();
        self.next_at = self.every.max(1);
    }

    fn advance(&mut self, rows: u64) {
        if self.every == 0 || rows < self.next_at {
            return;
        }
        let steps = rows / self.every;
        self.next_at = self.every.saturating_mul(steps.saturating_add(1));
        let line = log_line(&self.dip, rows, self.total, self.started.elapsed());
        let _ = writeln!(self.out, "{line}");
    }

    fn finish(&mut self) {
        let _ = self.out.flush();
    }
}

/// Terminal progress bar on stderr.
pub struct BarProgress {
    bar: indicatif::ProgressBar,
}

impl BarProgress {
    fn new() -> Self {
        Self {
            bar: indicatif::ProgressBar::hidden(),
        }
    }
}

impl Progress for BarProgress {
    fn start(&mut self, dip: &str, total: Option<u64>) {
        self.bar = match total {
            Some(total) => indicatif::ProgressBar::new(total),
            None => indicatif::ProgressBar::new_spinner(),
        };
        let template = if total.is_some() {
            "{msg} [{bar:40}] {pos}/{len} ({percent}%) eta {eta} {per_sec}"
        } else {
            "{msg} {spinner} {pos} rows {per_sec}"
        };
        let style = indicatif::ProgressStyle::with_template(template)
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar());
        self.bar.set_style(style);
        self.bar.set_message(dip.to_string());
    }

    fn advance(&mut self, rows: u64) {
        self.bar.set_position(rows);
    }

    fn finish(&mut self) {
        self.bar.finish_and_clear();
    }
}

/// A bar when stderr is a terminal, otherwise boundary log lines.
pub fn for_stderr(every: u64) -> Box<dyn Progress> {
    if std::io::stderr().is_terminal() {
        Box::new(BarProgress::new())
    } else {
        Box::new(LogProgress::new(std::io::stderr(), every))
    }
}

/// Fixed progress line used by [`LogProgress`] and the unit tests.
pub fn log_line(dip: &str, rows: u64, total: Option<u64>, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        (rows as f64 / secs).round() as u64
    } else {
        0
    };
    let secs_text = format!("{secs:.1}");
    match total {
        None => format!("progress: {dip}: {rows} rows, {rate} rows/s, {secs_text}s"),
        Some(total) => {
            let pct = if total == 0 {
                0.0
            } else {
                (rows as f64) * 100.0 / (total as f64)
            };
            let eta = if rows == 0 || secs == 0.0 {
                0
            } else {
                ((total.saturating_sub(rows) as f64) * secs / (rows as f64)).round() as u64
            };
            format!(
                "progress: {dip}: {rows}/{total} rows ({pct:.1}%), {rate} rows/s, {secs_text}s, eta {eta}s"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_without_and_with_a_total() {
        let elapsed = Duration::from_secs(2);
        assert_eq!(
            log_line("orders", 4000, None, elapsed),
            "progress: orders: 4000 rows, 2000 rows/s, 2.0s"
        );
        assert_eq!(
            log_line("orders", 4000, Some(8000), elapsed),
            "progress: orders: 4000/8000 rows (50.0%), 2000 rows/s, 2.0s, eta 2s"
        );
        assert_eq!(
            log_line("orders", 1000, None, Duration::from_millis(1500)),
            "progress: orders: 1000 rows, 667 rows/s, 1.5s"
        );
    }

    #[test]
    fn one_chunk_crossing_several_boundaries_prints_one_line() {
        let mut progress = LogProgress::new(Vec::<u8>::new(), 1000);
        progress.start("orders", None);
        progress.advance(2500);
        let text = String::from_utf8(progress.writer().clone()).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        progress.advance(5000);
        let text = String::from_utf8(progress.writer().clone()).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");

        let mut stepped = LogProgress::new(Vec::<u8>::new(), 1000);
        stepped.start("orders", Some(3000));
        stepped.advance(1000);
        stepped.advance(2000);
        stepped.advance(3000);
        let text = String::from_utf8(stepped.writer().clone()).unwrap();
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.contains("/3000 rows ("), "{text}");
    }
}
