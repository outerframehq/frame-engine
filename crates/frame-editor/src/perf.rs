// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Frame timing and memory for the performance overlay.
//!
//! The editor marks the start and end of each frame's work. Every half
//! second the numbers so far are summed up into a summary, so the overlay
//! holds still long enough to read.

use std::time::{Duration, Instant};

/// How often the summary is refreshed.
const PERIOD: Duration = Duration::from_millis(500);

/// One period's numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Summary {
    /// Frames drawn per second.
    pub fps: f32,
    /// Average time from one frame to the next, in milliseconds.
    pub frame_ms: f32,
    /// The longest gap between two frames, in milliseconds.
    pub worst_ms: f32,
    /// Average time the editor spent working on a frame (simulation, UI,
    /// building and submitting the draw), in milliseconds. The rest of a
    /// frame is waiting for the screen.
    pub work_ms: f32,
    /// Memory the process holds, in bytes, where the system says.
    pub resident: Option<u64>,
}

/// Collects frame times and sums them up every period.
pub(crate) struct FrameStats {
    period_start: Instant,
    last_begin: Option<Instant>,
    frames: u32,
    gap_total: Duration,
    gap_worst: Duration,
    work_total: Duration,
    summary: Summary,
}

impl FrameStats {
    pub(crate) fn new(now: Instant) -> Self {
        FrameStats {
            period_start: now,
            last_begin: None,
            frames: 0,
            gap_total: Duration::ZERO,
            gap_worst: Duration::ZERO,
            work_total: Duration::ZERO,
            summary: Summary::default(),
        }
    }

    /// A frame's work starts.
    pub(crate) fn begin(&mut self, now: Instant) {
        if let Some(last) = self.last_begin {
            let gap = now.saturating_duration_since(last);
            self.gap_total += gap;
            self.gap_worst = self.gap_worst.max(gap);
            self.frames += 1;
        }
        self.last_begin = Some(now);
    }

    /// A frame's work is done (everything is submitted to the graphics card).
    pub(crate) fn end(&mut self, now: Instant) {
        if let Some(begin) = self.last_begin {
            self.work_total += now.saturating_duration_since(begin);
        }
        let elapsed = now.saturating_duration_since(self.period_start);
        if elapsed >= PERIOD && self.frames > 0 {
            let n = self.frames as f32;
            self.summary = Summary {
                fps: n / elapsed.as_secs_f32(),
                frame_ms: self.gap_total.as_secs_f32() * 1000.0 / n,
                worst_ms: self.gap_worst.as_secs_f32() * 1000.0,
                work_ms: self.work_total.as_secs_f32() * 1000.0 / n,
                resident: resident_bytes(),
            };
            self.period_start = now;
            self.frames = 0;
            self.gap_total = Duration::ZERO;
            self.gap_worst = Duration::ZERO;
            self.work_total = Duration::ZERO;
        }
    }

    pub(crate) fn summary(&self) -> Summary {
        self.summary
    }

    /// The overlay's own lines.
    pub(crate) fn lines(&self) -> Vec<String> {
        let s = self.summary();
        let mut out = vec![
            format!(
                "{:.0} fps   frame {:.1} ms   worst {:.1} ms",
                s.fps, s.frame_ms, s.worst_ms
            ),
            format!("editor work {:.1} ms a frame", s.work_ms),
        ];
        if let Some(bytes) = s.resident {
            out.push(format!("memory {}", megabytes(bytes)));
        }
        out
    }
}

/// `bytes` as megabytes, for the overlay.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1.0e6)
}

/// How much memory the process holds right now (its resident set), where
/// the system tells us: Linux only, for now.
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vm_rss(&status)
}

/// The `VmRSS` line of `/proc/self/status`, in bytes.
fn parse_vm_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steady_frame_rate_is_summed_up_after_each_period() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        // 60 frames a second, 4 ms of work each, for one second.
        for i in 0..=60u32 {
            let t = start + Duration::from_micros(16_667 * u64::from(i));
            stats.begin(t);
            stats.end(t + Duration::from_millis(4));
        }
        let s = stats.summary();
        assert!((s.fps - 60.0).abs() < 2.0, "{s:?}");
        assert!((s.frame_ms - 16.7).abs() < 0.2, "{s:?}");
        assert!((s.work_ms - 4.0).abs() < 0.2, "{s:?}");
    }

    #[test]
    fn one_slow_frame_shows_as_the_worst() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        let mut t = start;
        for i in 0..40 {
            t += Duration::from_millis(if i == 20 { 80 } else { 16 });
            stats.begin(t);
            stats.end(t);
        }
        assert!((stats.summary().worst_ms - 80.0).abs() < 0.5);
    }

    #[test]
    fn nothing_is_shown_before_the_first_period_ends() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        stats.begin(start);
        stats.end(start + Duration::from_millis(5));
        assert_eq!(stats.summary(), Summary::default());
    }

    #[test]
    fn resident_memory_is_read_from_the_status_file() {
        let status = "Name:\tframe\nVmPeak:\t  900 kB\nVmRSS:\t  1234 kB\nThreads:\t4\n";
        assert_eq!(parse_vm_rss(status), Some(1234 * 1024));
        assert_eq!(parse_vm_rss("Name: x\n"), None);
    }
}
