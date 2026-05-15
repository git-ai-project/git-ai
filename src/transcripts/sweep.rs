//! Sweep function for daemon-driven transcript ingestion.
//!
//! Called by the daemon's post-commit worker to incrementally read new transcript
//! events from all discoverable agent sessions, updating watermarks as it goes.

use std::path::Path;

use super::watermark::{Watermark, WatermarkStore};
use super::{
    discover_sessions, read_json_array_incremental, read_jsonl_incremental,
    AgentTranscriptConfig, DiscoveryStrategy, TranscriptFormat, AGENT_TRANSCRIPT_CONFIGS,
};

/// A batch of new events discovered during a sweep for a single session.
#[derive(Debug)]
pub struct TranscriptUpdate {
    /// Agent tool name (e.g., "claude", "cursor").
    pub agent: String,
    /// Session hash (16 hex chars of SHA-256 of the file path).
    pub session_id: String,
    /// Newly read events since last watermark.
    pub new_events: Vec<serde_json::Value>,
    /// Updated position (byte offset or record index) after reading.
    pub new_position: u64,
}

/// Maximum events to read per session in a single sweep pass.
const BATCH_SIZE: usize = 1000;

/// Sweep all discoverable transcript files, reading incrementally from watermarks.
///
/// This function is idempotent — calling it repeatedly with no new transcript data
/// will return an empty vec and leave watermarks unchanged.
///
/// # Arguments
/// * `git_dir` - Path to the `.git` directory (watermarks are stored under `.git/ai/transcripts/`)
///
/// # Returns
/// A vec of `TranscriptUpdate` for sessions that had new events since last sweep.
pub fn sweep_transcripts(git_dir: &Path) -> Vec<TranscriptUpdate> {
    let mut updates = Vec::new();

    for config in AGENT_TRANSCRIPT_CONFIGS {
        // Only sweep agents that use directory scanning for discovery.
        if !matches!(config.discovery, DiscoveryStrategy::ScanDirs { .. }) {
            continue;
        }

        let session_files = discover_sessions(config.tool);
        for session_path in session_files {
            let session_id = WatermarkStore::session_hash(&session_path);

            let current_position = WatermarkStore::load(git_dir, config.tool, &session_id)
                .map(|wm| wm.position)
                .unwrap_or(0);

            let batch_result = read_session(config, &session_path, current_position);

            let (events, new_position) = match batch_result {
                Ok((events, pos)) => (events, pos),
                Err(_) => continue,
            };

            if events.is_empty() {
                continue;
            }

            // Save updated watermark
            let watermark = Watermark {
                path: session_path.clone(),
                position: new_position,
                last_read: now_iso8601(),
            };

            // Best-effort save — if it fails we'll just re-read next time
            let _ = WatermarkStore::save(git_dir, config.tool, &session_id, &watermark);

            updates.push(TranscriptUpdate {
                agent: config.tool.to_string(),
                session_id,
                new_events: events,
                new_position,
            });
        }
    }

    updates
}

/// Read new events from a single session file using the appropriate strategy.
fn read_session(
    config: &AgentTranscriptConfig,
    path: &Path,
    position: u64,
) -> Result<(Vec<serde_json::Value>, u64), super::reader::TranscriptError> {
    let batch = match config.format {
        TranscriptFormat::Jsonl => read_jsonl_incremental(path, position, BATCH_SIZE)?,
        TranscriptFormat::JsonArray => read_json_array_incremental(path, position, BATCH_SIZE)?,
    };
    Ok((batch.events, batch.new_position))
}

/// Generate a current ISO 8601 timestamp.
/// Uses a simple approach without pulling in chrono.
fn now_iso8601() -> String {
    // Read /proc/driver/rtc or use a simple seconds-since-epoch approach.
    // For portability, we use std::time::SystemTime.
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Convert to rough UTC date-time (good enough for watermark tracking)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Simple days-since-epoch to Y-M-D (good enough, not accounting for leap seconds)
    let (year, month, day) = days_to_ymd(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let days = days as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // day of era [0, 146096]
    let yoe =
        (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn test_sweep_empty_no_sessions() {
        let tmp = TempDir::new().unwrap();
        let empty_home = tmp.path().join("empty_home");
        fs::create_dir_all(&empty_home).unwrap();
        unsafe { std::env::set_var("HOME", empty_home.to_str().unwrap()) };

        let result = sweep_transcripts(tmp.path());
        assert!(result.is_empty());

        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    #[serial]
    fn test_sweep_reads_new_events_and_saves_watermark() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join("git_dir");
        fs::create_dir_all(&git_dir).unwrap();

        // Create a fake session file and point HOME to find it
        let home = tmp.path().join("home");
        let session_dir = home.join(".claude/projects");
        fs::create_dir_all(&session_dir).unwrap();

        let session_file = session_dir.join("test-session.jsonl");
        let mut f = fs::File::create(&session_file).unwrap();
        writeln!(f, r#"{{"role":"user","text":"hello"}}"#).unwrap();
        writeln!(f, r#"{{"role":"assistant","text":"hi","model":"claude-4"}}"#).unwrap();
        f.flush().unwrap();

        // Override HOME for discover_sessions
        unsafe { std::env::set_var("HOME", home.to_str().unwrap()) };

        let updates = sweep_transcripts(&git_dir);

        // Restore HOME (best effort)
        unsafe { std::env::remove_var("HOME") };

        // Should have found events from the claude agent
        assert!(!updates.is_empty());
        let update = updates.iter().find(|u| u.agent == "claude").unwrap();
        assert_eq!(update.new_events.len(), 2);
        assert!(update.new_position > 0);

        // Watermark should have been saved
        let wm = WatermarkStore::load(&git_dir, "claude", &update.session_id);
        assert!(wm.is_some());
        let wm = wm.unwrap();
        assert_eq!(wm.position, update.new_position);
        assert_eq!(wm.path, session_file);
    }

    #[test]
    #[serial]
    fn test_sweep_idempotent() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join("git_dir");
        fs::create_dir_all(&git_dir).unwrap();

        let home = tmp.path().join("home");
        let session_dir = home.join(".codex/sessions");
        fs::create_dir_all(&session_dir).unwrap();

        let session_file = session_dir.join("session1.jsonl");
        let mut f = fs::File::create(&session_file).unwrap();
        writeln!(f, r#"{{"id":1}}"#).unwrap();
        f.flush().unwrap();

        unsafe { std::env::set_var("HOME", home.to_str().unwrap()) };

        // First sweep — should find the event
        let updates1 = sweep_transcripts(&git_dir);
        assert_eq!(updates1.len(), 1);
        assert_eq!(updates1[0].new_events.len(), 1);

        // Second sweep — nothing new
        let updates2 = sweep_transcripts(&git_dir);
        assert!(updates2.is_empty());

        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    #[serial]
    fn test_sweep_picks_up_appended_data() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join("git_dir");
        fs::create_dir_all(&git_dir).unwrap();

        let home = tmp.path().join("home");
        let session_dir = home.join(".codex/sessions");
        fs::create_dir_all(&session_dir).unwrap();

        let session_file = session_dir.join("growing.jsonl");
        {
            let mut f = fs::File::create(&session_file).unwrap();
            writeln!(f, r#"{{"id":1}}"#).unwrap();
            writeln!(f, r#"{{"id":2}}"#).unwrap();
        }

        unsafe { std::env::set_var("HOME", home.to_str().unwrap()) };

        let updates1 = sweep_transcripts(&git_dir);
        assert_eq!(updates1[0].new_events.len(), 2);

        // Append more data
        {
            use std::fs::OpenOptions;
            let mut f = OpenOptions::new().append(true).open(&session_file).unwrap();
            writeln!(f, r#"{{"id":3}}"#).unwrap();
        }

        // Second sweep picks up only new data
        let updates2 = sweep_transcripts(&git_dir);
        assert_eq!(updates2.len(), 1);
        assert_eq!(updates2[0].new_events.len(), 1);
        assert_eq!(updates2[0].new_events[0]["id"], 3);

        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    fn test_now_iso8601_format() {
        let ts = now_iso8601();
        // Should look like "2024-01-15T10:30:45Z"
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // 2024-01-01 is 19723 days from epoch
        let (y, m, d) = days_to_ymd(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }
}
