//! Best-effort .git/ai directory storage telemetry for daemon heartbeats.
//!
//! Tracks known repo ai_dirs discovered through trace2 ingestion and computes
//! aggregate storage statistics (total bytes, working log counts) on demand.
//!
//! All I/O is bounded: directory traversals cap entries, skip symlinks, and
//! enforce an elapsed-time limit so the heartbeat path stays fast.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Instant;

static KNOWN_AI_DIRS: RwLock<Option<HashSet<PathBuf>>> = RwLock::new(None);

/// Maximum number of directory entries to visit per ai_dir traversal.
const MAX_ENTRIES_PER_DIR: usize = 10_000;

/// Maximum wall-clock time for the entire storage scan.
const MAX_SCAN_DURATION: std::time::Duration = std::time::Duration::from_secs(2);

/// Aggregated storage statistics across all known repos.
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub git_ai_dir_bytes: u64,
    pub working_logs_dir_bytes: u64,
    pub working_logs_count: u64,
    pub working_log_largest_bytes: u64,
}

/// Register a .git/ai directory path for storage tracking.
/// Called when the daemon discovers a repo through trace2 ingestion.
pub fn register_ai_dir(ai_dir: PathBuf) {
    if let Ok(mut guard) = KNOWN_AI_DIRS.write() {
        let set = guard.get_or_insert_with(HashSet::new);
        set.insert(ai_dir);
    }
}

/// Compute aggregate storage statistics across all known repo ai_dirs.
/// Returns `None` if no repos are registered or on lock failure.
/// Bounded: skips symlinks, caps entry count, and enforces a time limit.
pub fn scan_storage() -> Option<StorageStats> {
    let dirs: Vec<PathBuf> = {
        let guard = KNOWN_AI_DIRS.read().ok()?;
        let set = guard.as_ref()?;
        if set.is_empty() {
            return None;
        }
        set.iter().cloned().collect()
    };

    let deadline = Instant::now() + MAX_SCAN_DURATION;
    let mut total_ai_bytes: u64 = 0;
    let mut total_wl_bytes: u64 = 0;
    let mut total_wl_count: u64 = 0;
    let mut largest_wl_bytes: u64 = 0;

    for ai_dir in &dirs {
        if Instant::now() >= deadline {
            break;
        }
        if !ai_dir.is_dir() {
            continue;
        }

        let mut entries_visited: usize = 0;
        let ai_bytes =
            dir_size_bounded(ai_dir, &mut entries_visited, MAX_ENTRIES_PER_DIR, deadline);
        total_ai_bytes = total_ai_bytes.saturating_add(ai_bytes);

        let wl_dir = ai_dir.join("working_logs");
        if wl_dir.is_dir()
            && let Ok(rd) = std::fs::read_dir(&wl_dir) {
                for entry in rd.flatten() {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    if path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("old-"))
                    {
                        continue;
                    }

                    total_wl_count += 1;
                    let mut wl_entries: usize = 0;
                    let wl_size =
                        dir_size_bounded(&path, &mut wl_entries, MAX_ENTRIES_PER_DIR, deadline);
                    total_wl_bytes = total_wl_bytes.saturating_add(wl_size);
                    if wl_size > largest_wl_bytes {
                        largest_wl_bytes = wl_size;
                    }
                }
            }
    }

    Some(StorageStats {
        git_ai_dir_bytes: total_ai_bytes,
        working_logs_dir_bytes: total_wl_bytes,
        working_logs_count: total_wl_count,
        working_log_largest_bytes: largest_wl_bytes,
    })
}

/// Recursively compute directory size, bounded by max entries and deadline.
/// Skips symlinks.
fn dir_size_bounded(
    dir: &Path,
    entries_visited: &mut usize,
    max_entries: usize,
    deadline: Instant,
) -> u64 {
    let mut total: u64 = 0;
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    for entry in rd.flatten() {
        if *entries_visited >= max_entries || Instant::now() >= deadline {
            break;
        }
        *entries_visited += 1;

        let path = entry.path();

        // Skip symlinks
        if path
            .symlink_metadata()
            .map_or(true, |m| m.file_type().is_symlink())
        {
            continue;
        }

        if path.is_file() {
            if let Ok(meta) = path.metadata() {
                total = total.saturating_add(meta.len());
            }
        } else if path.is_dir() {
            total = total.saturating_add(dir_size_bounded(
                &path,
                entries_visited,
                max_entries,
                deadline,
            ));
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn reset_known_dirs() {
        if let Ok(mut guard) = KNOWN_AI_DIRS.write() {
            *guard = None;
        }
    }

    #[test]
    fn scan_storage_returns_none_when_no_dirs_registered() {
        reset_known_dirs();
        assert!(scan_storage().is_none());
    }

    #[test]
    fn scan_storage_returns_none_for_nonexistent_dir() {
        reset_known_dirs();
        register_ai_dir(PathBuf::from("/tmp/nonexistent-ai-dir-test-12345"));
        // Should return Some with zeros since the dir doesn't exist
        let stats = scan_storage();
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.git_ai_dir_bytes, 0);
    }

    #[test]
    fn scan_storage_computes_basic_stats() {
        reset_known_dirs();

        let tmp = tempfile::tempdir().unwrap();
        let ai_dir = tmp.path().join("ai");
        fs::create_dir_all(ai_dir.join("working_logs").join("abc123")).unwrap();
        fs::write(
            ai_dir
                .join("working_logs")
                .join("abc123")
                .join("checkpoints.jsonl"),
            "test data here!",
        )
        .unwrap();
        fs::write(ai_dir.join("config.json"), "{}").unwrap();

        register_ai_dir(ai_dir);

        let stats = scan_storage().unwrap();
        assert!(stats.git_ai_dir_bytes > 0);
        assert!(stats.working_logs_dir_bytes > 0);
        assert_eq!(stats.working_logs_count, 1);
        assert!(stats.working_log_largest_bytes > 0);
    }

    #[test]
    fn scan_storage_skips_old_working_logs() {
        reset_known_dirs();

        let tmp = tempfile::tempdir().unwrap();
        let ai_dir = tmp.path().join("ai");
        let wl = ai_dir.join("working_logs");
        fs::create_dir_all(wl.join("abc123")).unwrap();
        fs::write(wl.join("abc123").join("data.json"), "active").unwrap();
        fs::create_dir_all(wl.join("old-def456")).unwrap();
        fs::write(wl.join("old-def456").join("data.json"), "archived").unwrap();

        register_ai_dir(ai_dir);

        let stats = scan_storage().unwrap();
        assert_eq!(stats.working_logs_count, 1);
    }

    #[test]
    fn scan_storage_finds_largest_working_log() {
        reset_known_dirs();

        let tmp = tempfile::tempdir().unwrap();
        let ai_dir = tmp.path().join("ai");
        let wl = ai_dir.join("working_logs");
        fs::create_dir_all(wl.join("small")).unwrap();
        fs::write(wl.join("small").join("data"), "x").unwrap();
        fs::create_dir_all(wl.join("large")).unwrap();
        fs::write(wl.join("large").join("data"), "x".repeat(1000)).unwrap();

        register_ai_dir(ai_dir);

        let stats = scan_storage().unwrap();
        assert_eq!(stats.working_logs_count, 2);
        assert!(stats.working_log_largest_bytes >= 1000);
    }

    #[test]
    fn dir_size_bounded_respects_entry_limit() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..20 {
            fs::write(tmp.path().join(format!("file_{i}.txt")), "hello").unwrap();
        }

        let mut visited = 0;
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let _ = dir_size_bounded(tmp.path(), &mut visited, 5, deadline);
        assert!(visited <= 5);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_bounded_skips_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("real.txt"), "data").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real.txt"), tmp.path().join("link.txt"))
            .unwrap();

        let mut visited = 0;
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        let size = dir_size_bounded(tmp.path(), &mut visited, 100, deadline);
        // Only the real file's size (4 bytes), not counted twice
        assert_eq!(size, 4);
    }
}
