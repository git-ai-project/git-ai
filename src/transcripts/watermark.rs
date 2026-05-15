//! Flat-file watermark storage for transcript reading positions.
//!
//! Watermarks track how far into each transcript file has been read.
//! Stored as JSON files at `.git/ai/transcripts/<agent>/<session_hash>.json`.
//! Session hash is SHA-256 of the absolute path, truncated to 16 hex chars.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Watermark representing progress through a transcript file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Watermark {
    /// Absolute path to the transcript file being tracked.
    pub path: PathBuf,
    /// Byte offset (JSONL) or record index (JSON array) — where we left off.
    pub position: u64,
    /// ISO 8601 timestamp of the last successful read.
    pub last_read: String,
}

/// Manages watermark persistence in the `.git/ai/transcripts/` directory.
pub struct WatermarkStore;

impl WatermarkStore {
    /// Compute the session hash for a given transcript path.
    /// SHA-256 of the absolute path string, truncated to 16 hex characters.
    pub fn session_hash(session_path: &Path) -> String {
        let mut hasher = Sha256::new();
        hasher.update(session_path.to_string_lossy().as_bytes());
        let result = hasher.finalize();
        hex_encode(&result[..8])
    }

    /// Directory where watermarks for a given agent are stored.
    fn watermark_dir(git_dir: &Path, agent: &str) -> PathBuf {
        git_dir.join("ai").join("transcripts").join(agent)
    }

    /// Full path to a specific watermark file.
    fn watermark_path(git_dir: &Path, agent: &str, session_id: &str) -> PathBuf {
        Self::watermark_dir(git_dir, agent).join(format!("{}.json", session_id))
    }

    /// Load a watermark for a specific agent/session. Returns `None` if not found or invalid.
    pub fn load(git_dir: &Path, agent: &str, session_id: &str) -> Option<Watermark> {
        let path = Self::watermark_path(git_dir, agent, session_id);
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save a watermark for a specific agent/session.
    /// Creates the directory structure if it does not exist.
    pub fn save(
        git_dir: &Path,
        agent: &str,
        session_id: &str,
        watermark: &Watermark,
    ) -> std::io::Result<()> {
        let dir = Self::watermark_dir(git_dir, agent);
        fs::create_dir_all(&dir)?;
        let path = Self::watermark_path(git_dir, agent, session_id);
        let content = serde_json::to_string_pretty(watermark)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(&path, content)
    }

    /// List all watermarks for a given agent.
    /// Returns pairs of (session_id, Watermark).
    pub fn list(git_dir: &Path, agent: &str) -> Vec<(String, Watermark)> {
        let dir = Self::watermark_dir(git_dir, agent);
        let Ok(entries) = fs::read_dir(&dir) else {
            return vec![];
        };

        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    return None;
                }
                let session_id = path.file_stem()?.to_str()?.to_string();
                let content = fs::read_to_string(&path).ok()?;
                let watermark: Watermark = serde_json::from_str(&content).ok()?;
                Some((session_id, watermark))
            })
            .collect()
    }
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_hash_deterministic() {
        let path = Path::new("/home/user/.claude/projects/foo/session.jsonl");
        let h1 = WatermarkStore::session_hash(path);
        let h2 = WatermarkStore::session_hash(path);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_session_hash_different_paths() {
        let a = WatermarkStore::session_hash(Path::new("/path/a.jsonl"));
        let b = WatermarkStore::session_hash(Path::new("/path/b.jsonl"));
        assert_ne!(a, b);
    }

    #[test]
    fn test_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path();

        let wm = Watermark {
            path: PathBuf::from("/home/user/transcript.jsonl"),
            position: 12345,
            last_read: "2024-01-01T00:00:00Z".to_string(),
        };

        WatermarkStore::save(git_dir, "claude", "abc123", &wm).unwrap();
        let loaded = WatermarkStore::load(git_dir, "claude", "abc123");
        assert_eq!(loaded, Some(wm));
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let tmp = TempDir::new().unwrap();
        let result = WatermarkStore::load(tmp.path(), "cursor", "nonexistent");
        assert_eq!(result, None);
    }

    #[test]
    fn test_save_creates_directories() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path();

        let wm = Watermark {
            path: PathBuf::from("/some/path.jsonl"),
            position: 0,
            last_read: "2024-06-15T12:00:00Z".to_string(),
        };

        WatermarkStore::save(git_dir, "codex", "session_x", &wm).unwrap();

        // Verify the directory was created
        assert!(git_dir.join("ai/transcripts/codex").is_dir());
        assert!(git_dir.join("ai/transcripts/codex/session_x.json").is_file());
    }

    #[test]
    fn test_list_empty() {
        let tmp = TempDir::new().unwrap();
        let results = WatermarkStore::list(tmp.path(), "cursor");
        assert!(results.is_empty());
    }

    #[test]
    fn test_list_multiple_sessions() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path();

        let wm1 = Watermark {
            path: PathBuf::from("/a.jsonl"),
            position: 100,
            last_read: "2024-01-01T00:00:00Z".to_string(),
        };
        let wm2 = Watermark {
            path: PathBuf::from("/b.jsonl"),
            position: 200,
            last_read: "2024-01-02T00:00:00Z".to_string(),
        };

        WatermarkStore::save(git_dir, "claude", "sess1", &wm1).unwrap();
        WatermarkStore::save(git_dir, "claude", "sess2", &wm2).unwrap();

        let mut results = WatermarkStore::list(git_dir, "claude");
        results.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "sess1");
        assert_eq!(results[0].1, wm1);
        assert_eq!(results[1].0, "sess2");
        assert_eq!(results[1].1, wm2);
    }

    #[test]
    fn test_save_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path();

        let wm1 = Watermark {
            path: PathBuf::from("/a.jsonl"),
            position: 100,
            last_read: "2024-01-01T00:00:00Z".to_string(),
        };
        let wm2 = Watermark {
            path: PathBuf::from("/a.jsonl"),
            position: 500,
            last_read: "2024-01-02T00:00:00Z".to_string(),
        };

        WatermarkStore::save(git_dir, "claude", "sess1", &wm1).unwrap();
        WatermarkStore::save(git_dir, "claude", "sess1", &wm2).unwrap();

        let loaded = WatermarkStore::load(git_dir, "claude", "sess1").unwrap();
        assert_eq!(loaded.position, 500);
    }
}
