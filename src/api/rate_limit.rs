use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Minimum interval between uploads: 5 minutes.
const UPLOAD_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Minimum interval between metrics pushes: 24 hours.
const METRICS_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Persisted rate limit state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RateLimitState {
    /// ISO 8601 timestamp of the last upload.
    last_upload: Option<String>,
    /// ISO 8601 timestamp of the last metrics push.
    last_metrics: Option<String>,
}

impl Default for RateLimitState {
    fn default() -> Self {
        Self {
            last_upload: None,
            last_metrics: None,
        }
    }
}

/// Simple file-backed rate limiter.
/// Prevents accidental spam but does not block critical uploads.
pub struct RateLimiter {
    path: PathBuf,
}

impl RateLimiter {
    /// Create a rate limiter backed by `~/.git-ai/rate_limit.json`.
    pub fn new() -> Option<Self> {
        let home = super::home_dir()?;
        Some(Self {
            path: home.join(".git-ai").join("rate_limit.json"),
        })
    }

    /// Create a rate limiter with a custom file path (useful for testing).
    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns `true` if more than 5 minutes have elapsed since the last upload.
    pub fn can_upload(&self) -> bool {
        self.check_interval(|s| s.last_upload.as_deref(), UPLOAD_INTERVAL)
    }

    /// Returns `true` if more than 24 hours have elapsed since the last metrics push.
    pub fn can_send_metrics(&self) -> bool {
        self.check_interval(|s| s.last_metrics.as_deref(), METRICS_INTERVAL)
    }

    fn check_interval<F>(&self, get_ts: F, interval: Duration) -> bool
    where
        F: FnOnce(&RateLimitState) -> Option<&str>,
    {
        let state = self.load_state();
        match get_ts(&state) {
            None => true,
            Some(ts) => elapsed_since(ts) >= interval,
        }
    }

    /// Record that an upload just happened.
    pub fn record_upload(&self) {
        self.update_state(|s| s.last_upload = Some(now_iso8601()));
    }

    /// Record that a metrics push just happened.
    pub fn record_metrics(&self) {
        self.update_state(|s| s.last_metrics = Some(now_iso8601()));
    }

    fn update_state<F: FnOnce(&mut RateLimitState)>(&self, f: F) {
        let mut state = self.load_state();
        f(&mut state);
        self.save_state(&state);
    }

    fn load_state(&self) -> RateLimitState {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &RateLimitState) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(serialized) = serde_json::to_string_pretty(state) {
            let _ = fs::write(&self.path, serialized);
        }
    }
}

/// Get current time as ISO 8601 string (simplified: seconds since epoch as a fixed format).
fn now_iso8601() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Store as seconds since epoch for simplicity (avoids chrono dependency)
    format!("{}", duration.as_secs())
}

/// Compute elapsed time since the given timestamp string.
fn elapsed_since(timestamp: &str) -> Duration {
    let stored_secs: u64 = timestamp.parse().unwrap_or(0);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Duration::from_secs(now_secs.saturating_sub(stored_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_can_upload_when_no_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rate_limit.json");
        let limiter = RateLimiter::with_path(path);
        assert!(limiter.can_upload());
        assert!(limiter.can_send_metrics());
    }

    #[test]
    fn test_record_upload_then_cannot_upload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rate_limit.json");
        let limiter = RateLimiter::with_path(path);

        limiter.record_upload();
        assert!(!limiter.can_upload());
    }

    #[test]
    fn test_record_metrics_then_cannot_send_metrics() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rate_limit.json");
        let limiter = RateLimiter::with_path(path);

        limiter.record_metrics();
        assert!(!limiter.can_send_metrics());
    }

    #[test]
    fn test_can_upload_after_interval_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rate_limit.json");

        // Write a state with last_upload far in the past (0 = epoch)
        let state = RateLimitState {
            last_upload: Some("0".to_string()),
            last_metrics: Some("0".to_string()),
        };
        fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        let limiter = RateLimiter::with_path(path);
        assert!(limiter.can_upload());
        assert!(limiter.can_send_metrics());
    }

    #[test]
    fn test_state_persistence() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rate_limit.json");

        let limiter = RateLimiter::with_path(path.clone());
        limiter.record_upload();

        // Create a new limiter pointing to same file — state should persist
        let limiter2 = RateLimiter::with_path(path);
        assert!(!limiter2.can_upload());
    }
}
