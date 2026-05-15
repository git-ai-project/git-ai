use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Per-file attribution statistics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileStats {
    pub path: String,
    pub ai_lines: u64,
    pub human_lines: u64,
    pub untracked_lines: u64,
}

/// Per-commit attribution statistics, cached to disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommitStats {
    pub commit_sha: String,
    pub ai_lines: u64,
    pub human_lines: u64,
    pub untracked_lines: u64,
    pub files: Vec<FileStats>,
    pub cached_at: String,
}

/// Flat-file stats cache sharded by the first 2 hex chars of the commit SHA.
///
/// Storage layout: `<git_dir>/ai/stats_cache/{sha[0..2]}/{sha}.json`
pub struct StatsCache;

impl StatsCache {
    /// Resolve the cache file path for a given commit SHA.
    fn cache_path(git_dir: &Path, sha: &str) -> PathBuf {
        let shard = &sha[..2];
        git_dir
            .join("ai")
            .join("stats_cache")
            .join(shard)
            .join(format!("{}.json", sha))
    }

    /// Read cached stats for a commit. Returns `None` if not cached or unreadable.
    pub fn get(git_dir: &Path, sha: &str) -> Option<CommitStats> {
        let path = Self::cache_path(git_dir, sha);
        let data = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Write stats to the cache. Creates shard directories as needed.
    pub fn put(git_dir: &Path, stats: &CommitStats) -> Result<(), String> {
        let path = Self::cache_path(git_dir, &stats.commit_sha);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create cache directory: {}", e))?;
        }
        let json = serde_json::to_string_pretty(stats)
            .map_err(|e| format!("serialization error: {}", e))?;
        fs::write(&path, json).map_err(|e| format!("failed to write cache file: {}", e))
    }

    /// Check if a cached entry exists without parsing.
    pub fn has(git_dir: &Path, sha: &str) -> bool {
        Self::cache_path(git_dir, sha).exists()
    }

    /// Delete a cached entry (e.g. after a rebase rewrites history).
    pub fn invalidate(git_dir: &Path, sha: &str) {
        let path = Self::cache_path(git_dir, sha);
        let _ = fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_temp_git_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "git-ai-cache-test-{}-{}",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_stats() -> CommitStats {
        CommitStats {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            ai_lines: 42,
            human_lines: 10,
            untracked_lines: 5,
            files: vec![
                FileStats {
                    path: "src/main.rs".to_string(),
                    ai_lines: 30,
                    human_lines: 8,
                    untracked_lines: 2,
                },
                FileStats {
                    path: "src/lib.rs".to_string(),
                    ai_lines: 12,
                    human_lines: 2,
                    untracked_lines: 3,
                },
            ],
            cached_at: "2026-05-15T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_put_and_get_roundtrip() {
        let git_dir = make_temp_git_dir("roundtrip");
        let stats = sample_stats();

        StatsCache::put(&git_dir, &stats).unwrap();
        let retrieved = StatsCache::get(&git_dir, &stats.commit_sha).unwrap();
        assert_eq!(stats, retrieved);

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_has_returns_true_when_cached() {
        let git_dir = make_temp_git_dir("has");
        let stats = sample_stats();

        assert!(!StatsCache::has(&git_dir, &stats.commit_sha));
        StatsCache::put(&git_dir, &stats).unwrap();
        assert!(StatsCache::has(&git_dir, &stats.commit_sha));

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_get_returns_none_when_missing() {
        let git_dir = make_temp_git_dir("missing");
        assert!(StatsCache::get(&git_dir, "deadbeef00000000000000000000000000000000").is_none());
        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_invalidate_removes_entry() {
        let git_dir = make_temp_git_dir("invalidate");
        let stats = sample_stats();

        StatsCache::put(&git_dir, &stats).unwrap();
        assert!(StatsCache::has(&git_dir, &stats.commit_sha));

        StatsCache::invalidate(&git_dir, &stats.commit_sha);
        assert!(!StatsCache::has(&git_dir, &stats.commit_sha));

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_sharding_logic() {
        let git_dir = make_temp_git_dir("sharding");
        let stats = sample_stats();

        StatsCache::put(&git_dir, &stats).unwrap();

        // Verify the shard directory was created using first 2 chars of SHA
        let shard_dir = git_dir.join("ai").join("stats_cache").join("ab");
        assert!(shard_dir.exists());
        assert!(shard_dir.is_dir());

        let file_path = shard_dir.join(format!("{}.json", stats.commit_sha));
        assert!(file_path.exists());

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_invalidate_nonexistent_is_noop() {
        let git_dir = make_temp_git_dir("noop");
        // Should not panic or error
        StatsCache::invalidate(&git_dir, "0000000000000000000000000000000000000000");
        let _ = fs::remove_dir_all(&git_dir);
    }
}
