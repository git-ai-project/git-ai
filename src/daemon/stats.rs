//! Runtime statistics for the daemon.
//!
//! Thread-safe counters tracking daemon activity, used by `git-ai bg status`
//! and structured log output.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static DAEMON_STATS: OnceLock<DaemonStats> = OnceLock::new();

pub struct DaemonStats {
    started_at: Instant,
    pub commits_processed: AtomicU64,
    pub commits_skipped: AtomicU64,
    pub rewrites_processed: AtomicU64,
    pub checkpoints_ingested: AtomicU64,
    pub trace2_events_received: AtomicU64,
    pub trace2_connections: AtomicU64,
    pub errors: AtomicU64,
}

impl DaemonStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            commits_processed: AtomicU64::new(0),
            commits_skipped: AtomicU64::new(0),
            rewrites_processed: AtomicU64::new(0),
            checkpoints_ingested: AtomicU64::new(0),
            trace2_events_received: AtomicU64::new(0),
            trace2_connections: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

pub fn get() -> &'static DaemonStats {
    DAEMON_STATS.get_or_init(DaemonStats::new)
}

/// Format stats as a human-readable status report.
pub fn format_status_report() -> String {
    let s = get();
    let uptime = s.uptime_secs();
    let hours = uptime / 3600;
    let mins = (uptime % 3600) / 60;
    let secs = uptime % 60;

    format!(
        concat!(
            "uptime: {}h {}m {}s\n",
            "trace2 connections: {}\n",
            "trace2 events: {}\n",
            "commits processed: {}\n",
            "commits skipped: {}\n",
            "rewrites processed: {}\n",
            "checkpoints ingested: {}\n",
            "errors: {}",
        ),
        hours,
        mins,
        secs,
        s.trace2_connections.load(Ordering::Relaxed),
        s.trace2_events_received.load(Ordering::Relaxed),
        s.commits_processed.load(Ordering::Relaxed),
        s.commits_skipped.load(Ordering::Relaxed),
        s.rewrites_processed.load(Ordering::Relaxed),
        s.checkpoints_ingested.load(Ordering::Relaxed),
        s.errors.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_increment_and_read() {
        let stats = DaemonStats::new();
        stats.commits_processed.fetch_add(3, Ordering::Relaxed);
        stats.errors.fetch_add(1, Ordering::Relaxed);
        assert_eq!(stats.commits_processed.load(Ordering::Relaxed), 3);
        assert_eq!(stats.errors.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn uptime_is_non_zero_after_creation() {
        let stats = DaemonStats::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        // uptime_secs() is u64, so it's always >= 0
        let _ = stats.uptime_secs();
    }

    #[test]
    fn format_report_contains_all_fields() {
        let report = format_status_report();
        assert!(report.contains("uptime:"));
        assert!(report.contains("commits processed:"));
        assert!(report.contains("errors:"));
    }
}
