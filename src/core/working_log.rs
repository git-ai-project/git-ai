use super::attribution::{Attribution, LineAttribution};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(windows)]
#[repr(C)]
struct WinapiOverlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: usize,
}

#[cfg(windows)]
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;
#[cfg(windows)]
#[allow(dead_code)]
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;

#[cfg(windows)]
unsafe extern "system" {
    fn LockFileEx(
        hFile: *mut std::ffi::c_void,
        dwFlags: u32,
        dwReserved: u32,
        nNumberOfBytesToLockLow: u32,
        nNumberOfBytesToLockHigh: u32,
        lpOverlapped: *mut WinapiOverlapped,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointKind {
    Human,
    AiAgent,
    KnownHuman,
}

impl CheckpointKind {
    pub fn is_ai(self) -> bool {
        matches!(self, CheckpointKind::AiAgent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentId {
    pub tool: String,
    pub id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingLogEntry {
    pub file: String,
    #[serde(default)]
    pub blob_sha: String,
    #[serde(default)]
    pub attributions: Vec<Attribution>,
    #[serde(default)]
    pub line_attributions: Vec<LineAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub kind: CheckpointKind,
    pub author: String,
    pub entries: Vec<WorkingLogEntry>,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Monotonically increasing sequence number within a base commit's working log.
    /// Used to establish ordering even if checkpoints arrive out of order.
    #[serde(default)]
    pub seq: u64,
}

impl Checkpoint {
    pub fn new(kind: CheckpointKind, author: String, entries: Vec<WorkingLogEntry>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            kind,
            author,
            entries,
            timestamp,
            agent_id: None,
            trace_id: None,
            seq: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanRecord {
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRecord {
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitialAttributions {
    pub files: HashMap<String, Vec<LineAttribution>>,
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,
    #[serde(default)]
    pub humans: HashMap<String, HumanRecord>,
    #[serde(default)]
    pub prompts: HashMap<String, PromptRecord>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Validates that a string looks like a valid git SHA to prevent path traversal.
fn is_valid_base_commit(s: &str) -> bool {
    if s == "initial" {
        return true;
    }
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn working_log_dir(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    if !is_valid_base_commit(base_commit) {
        return repo_git_dir.join("ai").join("working_logs").join("invalid");
    }
    repo_git_dir
        .join("ai")
        .join("working_logs")
        .join(base_commit)
}

fn checkpoints_path(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("checkpoints.jsonl")
}

fn blobs_dir(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("blobs")
}

fn initial_path(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("INITIAL")
}

// ---------------------------------------------------------------------------
// Storage operations
// ---------------------------------------------------------------------------

/// Read all checkpoints from the JSONL file for the given base commit.
/// Returns an empty vec if the file does not exist or cannot be read.
pub fn read_checkpoints(repo_git_dir: &Path, base_commit: &str) -> Vec<Checkpoint> {
    let path = checkpoints_path(repo_git_dir, base_commit);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut checkpoints = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Checkpoint>(&line) {
            Ok(cp) => checkpoints.push(cp),
            Err(_) => continue,
        }
    }

    checkpoints
}

/// Acquire an advisory file lock for the given base commit's working log.
/// Returns the lock file handle (lock is held for the lifetime of the handle).
/// On failure, returns `None` (best-effort).
#[cfg(unix)]
fn lock_working_log(repo_git_dir: &Path, base_commit: &str) -> Option<fs::File> {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);
    let lock_path = dir.join(".lock");
    let file = fs::File::create(&lock_path).ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret == 0 { Some(file) } else { None }
}

#[cfg(windows)]
fn lock_working_log(repo_git_dir: &Path, base_commit: &str) -> Option<fs::File> {
    use std::os::windows::io::AsRawHandle;

    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);
    let lock_path = dir.join(".lock");

    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;

    let handle = file.as_raw_handle();
    let mut overlapped: WinapiOverlapped = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        LockFileEx(
            handle as _,
            LOCKFILE_EXCLUSIVE_LOCK,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ret != 0 { Some(file) } else { None }
}

#[cfg(not(any(unix, windows)))]
fn lock_working_log(_repo_git_dir: &Path, _base_commit: &str) -> Option<fs::File> {
    None
}

/// Read and increment the sequence counter for the given base commit.
/// The counter is stored in `.git/ai/working_logs/<base_commit>/.seq`.
/// Caller should hold the working log lock before calling this.
fn next_seq(repo_git_dir: &Path, base_commit: &str) -> u64 {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let seq_path = dir.join(".seq");

    let current: u64 = fs::read_to_string(&seq_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let next = current + 1;
    let _ = fs::write(&seq_path, next.to_string());
    next
}

/// Append a single checkpoint to the JSONL file.
/// Creates the directory structure if it does not exist.
/// Acquires an advisory file lock to serialize concurrent writes
/// and assigns a monotonically increasing sequence number.
pub fn append_checkpoint(repo_git_dir: &Path, base_commit: &str, checkpoint: &Checkpoint) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    // Acquire advisory lock (held until _lock_guard is dropped)
    let _lock_guard = lock_working_log(repo_git_dir, base_commit);

    // Assign sequence number
    let seq = next_seq(repo_git_dir, base_commit);
    let mut checkpoint = checkpoint.clone();
    checkpoint.seq = seq;

    let path = checkpoints_path(repo_git_dir, base_commit);
    let json_line = match serde_json::to_string(&checkpoint) {
        Ok(s) => s,
        Err(_) => return,
    };

    use std::io::Write;
    let mut file = match fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if writeln!(file, "{}", json_line).is_ok() {
        // Ensure data is flushed to disk for crash resilience
        let _ = file.flush();
        let _ = file.sync_data();
    }
}

/// Save arbitrary content to the blobs directory, returning its SHA-256 hex digest.
pub fn save_blob(repo_git_dir: &Path, base_commit: &str, content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let sha = format!("{:x}", hasher.finalize());

    let dir = blobs_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    let blob_path = dir.join(&sha);
    let _ = fs::write(&blob_path, content);

    sha
}

/// Read a blob by its SHA-256 hex digest. Returns `None` if it does not exist.
pub fn read_blob(repo_git_dir: &Path, base_commit: &str, sha: &str) -> Option<String> {
    let blob_path = blobs_dir(repo_git_dir, base_commit).join(sha);
    fs::read_to_string(blob_path).ok()
}

/// Read the INITIAL attributions file for the given base commit.
pub fn read_initial_attributions(
    repo_git_dir: &Path,
    base_commit: &str,
) -> Option<InitialAttributions> {
    let path = initial_path(repo_git_dir, base_commit);
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write the INITIAL attributions file for the given base commit.
/// Creates the directory structure if needed.
pub fn write_initial_attributions(
    repo_git_dir: &Path,
    base_commit: &str,
    attrs: &InitialAttributions,
) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    let path = initial_path(repo_git_dir, base_commit);
    let json = match serde_json::to_string_pretty(attrs) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = fs::write(&path, json);
}

/// Delete the entire working log directory for the given base commit.
pub fn delete_working_log(repo_git_dir: &Path, base_commit: &str) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        (tmp, git_dir)
    }

    #[test]
    fn roundtrip_checkpoint() {
        let (_tmp, git_dir) = setup();
        let base = "abc123";

        let entry = WorkingLogEntry {
            file: "src/main.rs".into(),
            blob_sha: "deadbeef".into(),
            attributions: vec![],
            line_attributions: vec![],
        };
        let cp = Checkpoint::new(CheckpointKind::AiAgent, "claude".into(), vec![entry]);

        append_checkpoint(&git_dir, base, &cp);
        append_checkpoint(&git_dir, base, &cp);

        let loaded = read_checkpoints(&git_dir, base);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].author, "claude");
        assert_eq!(loaded[0].entries[0].file, "src/main.rs");
    }

    #[test]
    fn blob_save_and_read() {
        let (_tmp, git_dir) = setup();
        let base = "def456";

        let content = b"fn main() { println!(\"hello\"); }";
        let sha = save_blob(&git_dir, base, content);

        assert_eq!(sha.len(), 64); // SHA-256 hex is 64 chars
        let loaded = read_blob(&git_dir, base, &sha).unwrap();
        assert_eq!(loaded.as_bytes(), content);
    }

    #[test]
    fn initial_attributions_roundtrip() {
        let (_tmp, git_dir) = setup();
        let base = "789abc";

        let mut files = HashMap::new();
        files.insert(
            "lib.rs".into(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 5,
                author_id: "h_alice".into(),
                overrode: None,
            }],
        );

        let attrs = InitialAttributions {
            files,
            sessions: HashMap::new(),
            humans: HashMap::new(),
            prompts: HashMap::new(),
        };

        write_initial_attributions(&git_dir, base, &attrs);
        let loaded = read_initial_attributions(&git_dir, base).unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files["lib.rs"][0].end_line, 5);
    }

    #[test]
    fn delete_removes_directory() {
        let (_tmp, git_dir) = setup();
        let base = "to_delete";

        let cp = Checkpoint::new(CheckpointKind::Human, "user".into(), vec![]);
        append_checkpoint(&git_dir, base, &cp);

        let dir = working_log_dir(&git_dir, base);
        assert!(dir.exists());

        delete_working_log(&git_dir, base);
        assert!(!dir.exists());
    }

    #[test]
    fn read_checkpoints_empty_when_missing() {
        let (_tmp, git_dir) = setup();
        let loaded = read_checkpoints(&git_dir, "nonexistent");
        assert!(loaded.is_empty());
    }

    #[test]
    fn read_blob_returns_none_when_missing() {
        let (_tmp, git_dir) = setup();
        assert!(read_blob(&git_dir, "x", "nosuchsha").is_none());
    }

    #[test]
    fn seq_numbers_are_monotonically_increasing() {
        let (_tmp, git_dir) = setup();
        let base = "seq_test";

        let entry = WorkingLogEntry {
            file: "test.rs".into(),
            blob_sha: "aaa".into(),
            attributions: vec![],
            line_attributions: vec![],
        };

        for _ in 0..5 {
            let cp = Checkpoint::new(CheckpointKind::AiAgent, "agent".into(), vec![entry.clone()]);
            append_checkpoint(&git_dir, base, &cp);
        }

        let loaded = read_checkpoints(&git_dir, base);
        assert_eq!(loaded.len(), 5);

        // Verify sequence numbers are 1, 2, 3, 4, 5
        for (i, cp) in loaded.iter().enumerate() {
            assert_eq!(
                cp.seq,
                (i + 1) as u64,
                "checkpoint {} should have seq {}",
                i,
                i + 1
            );
        }
    }

    #[test]
    fn concurrent_writes_do_not_corrupt_data() {
        use std::sync::Arc;
        use std::thread;

        let (_tmp, git_dir) = setup();
        let base = "concurrent_test";
        let git_dir = Arc::new(git_dir);

        let num_threads = 8;
        let writes_per_thread = 10;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let git_dir = Arc::clone(&git_dir);
                let base = base.to_string();
                thread::spawn(move || {
                    for i in 0..writes_per_thread {
                        let entry = WorkingLogEntry {
                            file: format!("file_t{}_i{}.rs", t, i),
                            blob_sha: format!("sha_{}_{}", t, i),
                            attributions: vec![],
                            line_attributions: vec![],
                        };
                        let cp = Checkpoint::new(
                            CheckpointKind::AiAgent,
                            format!("agent-{}", t),
                            vec![entry],
                        );
                        append_checkpoint(&git_dir, &base, &cp);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let loaded = read_checkpoints(&git_dir, base);
        let expected_count = num_threads * writes_per_thread;
        assert_eq!(
            loaded.len(),
            expected_count,
            "expected {} checkpoints, got {}",
            expected_count,
            loaded.len()
        );

        // Verify all sequence numbers are unique and form the set 1..=expected_count
        let mut seqs: Vec<u64> = loaded.iter().map(|cp| cp.seq).collect();
        seqs.sort();
        let expected_seqs: Vec<u64> = (1..=expected_count as u64).collect();
        assert_eq!(
            seqs, expected_seqs,
            "sequence numbers should be 1..={}",
            expected_count
        );
    }

    #[test]
    fn seq_counter_persists_across_calls() {
        let (_tmp, git_dir) = setup();
        let base = "persist_seq";

        let entry = WorkingLogEntry {
            file: "a.rs".into(),
            blob_sha: "x".into(),
            attributions: vec![],
            line_attributions: vec![],
        };

        // Write 3 checkpoints
        for _ in 0..3 {
            let cp = Checkpoint::new(CheckpointKind::Human, "user".into(), vec![entry.clone()]);
            append_checkpoint(&git_dir, base, &cp);
        }

        // Read the sequence file directly to verify it persisted
        let seq_path = working_log_dir(&git_dir, base).join(".seq");
        let stored_seq: u64 = fs::read_to_string(&seq_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(stored_seq, 3);

        // Write one more and verify it gets seq=4
        let cp = Checkpoint::new(CheckpointKind::Human, "user".into(), vec![entry.clone()]);
        append_checkpoint(&git_dir, base, &cp);

        let loaded = read_checkpoints(&git_dir, base);
        assert_eq!(loaded.last().unwrap().seq, 4);
    }

    #[test]
    fn sha_validation_prevents_path_traversal() {
        // Valid SHAs
        assert!(is_valid_base_commit("abc123"));
        assert!(is_valid_base_commit("0123456789abcdef"));
        assert!(is_valid_base_commit("a".repeat(40).as_str())); // SHA-1 length
        assert!(is_valid_base_commit("a".repeat(64).as_str())); // SHA-256 length

        // Invalid SHAs (should be rejected)
        assert!(!is_valid_base_commit(""));
        assert!(!is_valid_base_commit("../../../etc/passwd"));
        assert!(!is_valid_base_commit("../../etc"));
        assert!(!is_valid_base_commit("abc/def"));
        assert!(!is_valid_base_commit("abc def"));
        assert!(!is_valid_base_commit("abc\x00def"));
        assert!(!is_valid_base_commit("a".repeat(65).as_str())); // Too long
    }

    #[test]
    fn path_traversal_attempt_uses_safe_fallback() {
        let (_tmp, git_dir) = setup();
        let malicious_base = "../../etc/passwd";

        // Attempting to use a path traversal as base_commit should not escape the working_logs dir
        let path = working_log_dir(&git_dir, malicious_base);
        let path_str = path.to_string_lossy();

        // Should use the "invalid" fallback, not traverse outside
        assert!(path_str.contains("working_logs"));
        assert!(path_str.contains("invalid"));
        assert!(!path_str.contains("etc"));
        assert!(!path_str.contains("passwd"));
    }
}
