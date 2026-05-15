//! Persistent daemon statistics across restarts.
//!
//! Stats are saved to `~/.git-ai/daemon_stats.json` and accumulate
//! across daemon restarts for continuity tracking.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use super::stats;

/// Persisted stats that accumulate across daemon restarts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedStats {
    pub total_uptime_secs: u64,
    pub total_commits_processed: u64,
    pub total_checkpoints: u64,
    pub last_started: String,
}

/// Save the current daemon stats (merged with any previously-persisted stats)
/// to `~/.git-ai/daemon_stats.json`.
pub fn save_stats(current: &stats::DaemonStats) {
    let path = stats_file_path();

    // Load existing persisted stats and accumulate
    let mut persisted = load_stats().unwrap_or_default();
    persisted.total_uptime_secs += current.uptime_secs();
    persisted.total_commits_processed += current.commits_processed.load(Ordering::Relaxed);
    persisted.total_checkpoints += current.checkpoints_ingested.load(Ordering::Relaxed);
    // last_started is already set from when the daemon started; keep it as-is
    // (it will be updated on next start via update_last_started)

    if let Ok(json) = serde_json::to_string_pretty(&persisted) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, json);
    }
}

/// Load previously-persisted stats from disk.
pub fn load_stats() -> Option<PersistedStats> {
    let path = stats_file_path();
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Update the `last_started` field in the persisted stats file.
/// Called after the daemon starts to record the current start time.
pub fn update_last_started(timestamp: &str) {
    let path = stats_file_path();
    let mut persisted = load_stats().unwrap_or_default();
    persisted.last_started = timestamp.to_string();

    if let Ok(json) = serde_json::to_string_pretty(&persisted) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, json);
    }
}

fn stats_file_path() -> PathBuf {
    #[cfg(unix)]
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("APPDATA"))
        .unwrap_or_else(|_| "C:\\Temp".to_string());

    PathBuf::from(home)
        .join(".git-ai")
        .join("daemon_stats.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn persisted_stats_serialization_roundtrip() {
        let stats = PersistedStats {
            total_uptime_secs: 3600,
            total_commits_processed: 42,
            total_checkpoints: 100,
            last_started: "2025-01-01T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string_pretty(&stats).unwrap();
        let deserialized: PersistedStats = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.total_uptime_secs, 3600);
        assert_eq!(deserialized.total_commits_processed, 42);
        assert_eq!(deserialized.total_checkpoints, 100);
        assert_eq!(deserialized.last_started, "2025-01-01T00:00:00Z");
    }

    #[test]
    fn load_stats_returns_none_for_missing_file() {
        // Point HOME to a temp dir with no stats file
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            env::set_var("HOME", dir.path());
        }

        let result = load_stats();
        assert!(result.is_none());

        // Restore (best effort)
        unsafe {
            env::remove_var("HOME");
        }
    }

    #[test]
    fn default_persisted_stats_is_zero() {
        let stats = PersistedStats::default();
        assert_eq!(stats.total_uptime_secs, 0);
        assert_eq!(stats.total_commits_processed, 0);
        assert_eq!(stats.total_checkpoints, 0);
        assert_eq!(stats.last_started, "");
    }
}
