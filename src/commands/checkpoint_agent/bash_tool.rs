//! Bash tool change attribution via pre/post stat-tuple snapshots.
//!
//! Detects file changes made by bash/shell tool calls by comparing filesystem
//! metadata snapshots taken before and after tool execution.

use crate::daemon::control_api::ControlRequest;
use crate::daemon::{DaemonConfig, send_control_request_with_timeout};
use crate::error::GitAiError;
use crate::utils::debug_log;
use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Grace window for low-resolution filesystem detection (seconds).
const _MTIME_GRACE_WINDOW_SECS: u64 = 2;

/// Maximum time for stat-diff walk before fallback (ms).
const STAT_DIFF_TIMEOUT_MS: u64 = 5000;

/// Repo size threshold; above this, warn and fall back to git status.
const MAX_TRACKED_FILES: usize = 500_000;

/// Pre-snapshots older than this are garbage-collected (seconds).
const SNAPSHOT_STALE_SECS: u64 = 300;

/// Grace window in nanoseconds for low-resolution filesystem mtime comparison.
#[allow(dead_code)]
const MTIME_GRACE_WINDOW_NS: u128 = (_MTIME_GRACE_WINDOW_SECS as u128) * 1_000_000_000;

/// Maximum number of stale files before skipping content capture.
#[allow(dead_code)]
const MAX_STALE_FILES_FOR_CAPTURE: usize = 1000;

/// Maximum file size for content capture (10 MB).
const MAX_CAPTURE_FILE_SIZE: u64 = 10 * 1024 * 1024; // used by capture_file_contents

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Metadata fingerprint for a single file, collected via `lstat()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatEntry {
    pub exists: bool,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
    pub size: u64,
    pub mode: u32,
    pub file_type: StatFileType,
}

/// File type enumeration (symlink-aware, no following).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatFileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

impl StatEntry {
    /// Build a `StatEntry` from `std::fs::Metadata` (from `symlink_metadata` / `lstat`).
    pub fn from_metadata(meta: &fs::Metadata) -> Self {
        let file_type = if meta.file_type().is_symlink() {
            StatFileType::Symlink
        } else if meta.file_type().is_dir() {
            StatFileType::Directory
        } else if meta.file_type().is_file() {
            StatFileType::Regular
        } else {
            StatFileType::Other
        };

        let mtime = meta.modified().ok();
        let size = meta.len();
        let mode = Self::extract_mode(meta);
        let ctime = Self::extract_ctime(meta);

        StatEntry {
            exists: true,
            mtime,
            ctime,
            size,
            mode,
            file_type,
        }
    }

    #[cfg(unix)]
    fn extract_mode(meta: &fs::Metadata) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }

    #[cfg(not(unix))]
    fn extract_mode(meta: &fs::Metadata) -> u32 {
        if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }

    #[cfg(unix)]
    fn extract_ctime(meta: &fs::Metadata) -> Option<SystemTime> {
        use std::os::unix::fs::MetadataExt;
        let ctime_secs = meta.ctime();
        let ctime_nsecs = meta.ctime_nsec() as u32;
        if ctime_secs >= 0 {
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(ctime_secs as u64, ctime_nsecs))
        } else {
            None
        }
    }

    #[cfg(not(unix))]
    fn extract_ctime(meta: &fs::Metadata) -> Option<SystemTime> {
        // On Windows, use creation time as a proxy for ctime
        meta.created().ok()
    }
}

/// A complete filesystem snapshot: stat-tuples keyed by normalized path.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatSnapshot {
    /// File metadata keyed by normalized relative path.
    pub entries: HashMap<PathBuf, StatEntry>,
    /// Git-tracked files at snapshot time (normalized relative paths).
    pub tracked_files: HashSet<PathBuf>,
    /// Serialized gitignore rules (we store the repo root for rebuild).
    #[serde(skip)]
    pub gitignore: Option<Gitignore>,
    /// When this snapshot was taken.
    #[serde(skip)]
    pub taken_at: Option<Instant>,
    /// Unique invocation key: "{session_id}:{tool_use_id}".
    pub invocation_key: String,
    /// Repo root path (for serialization round-trip of gitignore).
    pub repo_root: PathBuf,
}

/// Result of diffing two snapshots.
#[derive(Debug, Default)]
pub struct StatDiffResult {
    pub created: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

impl StatDiffResult {
    /// All changed paths (created + modified + deleted) as Strings.
    pub fn all_changed_paths(&self) -> Vec<String> {
        self.created
            .iter()
            .chain(self.modified.iter())
            .chain(self.deleted.iter())
            .map(|p| crate::utils::normalize_to_posix(&p.to_string_lossy()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }
}

/// What the bash tool handler decided to do.
pub enum BashCheckpointAction {
    /// Take a pre-snapshot (PreToolUse).
    TakePreSnapshot,
    /// Files changed — emit a checkpoint with these paths.
    Checkpoint(Vec<String>),
    /// Stat-diff ran but found nothing.
    NoChanges,
    /// An error occurred; fall back to git status.
    Fallback,
}

/// Which hook event triggered the bash tool handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

/// Result from `handle_bash_tool` combining the action with optional captured checkpoint info.
#[allow(dead_code)]
pub struct BashToolResult {
    /// The checkpoint action (unchanged from previous API).
    pub action: BashCheckpointAction,
    /// If set, a captured checkpoint was prepared and needs submission by the handler.
    pub captured_checkpoint: Option<CapturedCheckpointInfo>,
}

/// Info about a captured checkpoint prepared by the bash tool.
#[allow(dead_code)]
pub struct CapturedCheckpointInfo {
    pub capture_id: String,
    pub repo_working_dir: String,
}

/// Per-agent tool classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// A known file-edit tool (Write, Edit, etc.) — handled by existing preset logic.
    FileEdit,
    /// A bash/shell tool — handled by the stat-diff system.
    Bash,
    /// Unrecognized tool — skip checkpoint.
    Skip,
}

// ---------------------------------------------------------------------------
// Tool classification per agent (Section 8.2 of PRD)
// ---------------------------------------------------------------------------

/// Classify a tool name for a given agent.
pub fn classify_tool(agent: Agent, tool_name: &str) -> ToolClass {
    match agent {
        Agent::Claude => match tool_name {
            "Write" | "Edit" | "MultiEdit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Gemini => match tool_name {
            "write_file" | "replace" => ToolClass::FileEdit,
            "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::ContinueCli => match tool_name {
            "edit" => ToolClass::FileEdit,
            "terminal" | "local_shell_call" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Droid => match tool_name {
            "ApplyPatch" | "Edit" | "Write" | "Create" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Amp => match tool_name {
            "Write" | "Edit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::OpenCode => match tool_name {
            "edit" | "write" => ToolClass::FileEdit,
            "bash" | "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
    }
}

/// Supported AI agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Gemini,
    ContinueCli,
    Droid,
    Amp,
    OpenCode,
}

// ---------------------------------------------------------------------------
// Path normalization
// ---------------------------------------------------------------------------

/// Normalize a path for use as HashMap key.
/// On case-insensitive filesystems (macOS, Windows), case-fold to lowercase.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn normalize_path(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn normalize_path(p: &Path) -> PathBuf {
    p.to_path_buf()
}

// ---------------------------------------------------------------------------
// Path filtering (two-tier: git index + frozen .gitignore)
// ---------------------------------------------------------------------------

/// Load the set of git-tracked files from the index.
pub fn load_tracked_files(repo_root: &Path) -> Result<HashSet<PathBuf>, GitAiError> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repo_root)
        .output()
        .map_err(GitAiError::IoError)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let tracked: HashSet<PathBuf> = output
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let path_str = String::from_utf8_lossy(s);
            normalize_path(Path::new(path_str.as_ref()))
        })
        .collect();

    Ok(tracked)
}

/// Build frozen `.gitignore` rules from the repo root at a point in time.
pub fn build_gitignore(repo_root: &Path) -> Result<Gitignore, GitAiError> {
    let mut builder = GitignoreBuilder::new(repo_root);

    // Recursively collect .gitignore files from the repo tree.
    // Depth-limited and time-limited to avoid excessive traversal.
    const MAX_GITIGNORE_DEPTH: usize = 10;

    /// Well-known directory names that are almost always gitignored.
    /// Skipping these avoids descending into very large ignored trees
    /// (e.g. `node_modules/`) when we cannot yet match against the
    /// partially-built gitignore ruleset.
    const SKIP_DIR_NAMES: &[&str] = &[
        "node_modules",
        "target",
        ".build",
        "vendor",
        "__pycache__",
        ".venv",
        "dist",
        "build",
    ];

    fn collect_gitignores(
        builder: &mut GitignoreBuilder,
        dir: &Path,
        depth: usize,
        deadline: Instant,
    ) {
        if depth >= MAX_GITIGNORE_DEPTH || Instant::now() > deadline {
            return;
        }

        let gitignore_path = dir.join(".gitignore");
        if gitignore_path.exists()
            && let Some(err) = builder.add(&gitignore_path)
        {
            debug_log(&format!(
                "Warning: failed to parse {}: {}",
                gitignore_path.display(),
                err
            ));
        }

        // Recurse into subdirectories to find nested .gitignore files
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && !path.ends_with(".git") {
                    // Skip well-known large ignored directory trees.
                    if let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && SKIP_DIR_NAMES.contains(&name)
                    {
                        continue;
                    }
                    collect_gitignores(builder, &path, depth + 1, deadline);
                }
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    collect_gitignores(&mut builder, repo_root, 0, deadline);

    builder
        .build()
        .map_err(|e| GitAiError::Generic(format!("Failed to build gitignore rules: {}", e)))
}

/// Check whether a newly created (untracked) file should be included.
/// Returns true if the file is NOT ignored by .gitignore rules.
pub fn should_include_new_file(gitignore: &Gitignore, path: &Path, is_dir: bool) -> bool {
    let matched = gitignore.matched(path, is_dir);
    !matched.is_ignore()
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Take a stat snapshot of the repo working tree.
///
/// Collects `lstat()` metadata for all tracked files plus new untracked files
/// that pass gitignore filtering.
pub fn snapshot(
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
) -> Result<StatSnapshot, GitAiError> {
    let start = Instant::now();
    let invocation_key = format!("{}:{}", session_id, tool_use_id);

    // Load git-tracked files (Tier 1)
    let tracked_files = load_tracked_files(repo_root)?;

    if tracked_files.len() > MAX_TRACKED_FILES {
        debug_log(&format!(
            "Repo has {} tracked files (> {}), falling back to git status",
            tracked_files.len(),
            MAX_TRACKED_FILES
        ));
        return Err(GitAiError::Generic(format!(
            "Repo exceeds {} tracked files; use git status fallback",
            MAX_TRACKED_FILES
        )));
    }

    // Freeze .gitignore rules (Tier 2)
    let gitignore = build_gitignore(repo_root)?;

    let mut entries = HashMap::new();

    // Use the ignore crate walker for efficient traversal.
    // Enable git_ignore so the walker prunes ignored directories (node_modules/,
    // target/, etc.) during traversal rather than visiting all their files only
    // to filter them out later. The frozen gitignore from build_gitignore() is
    // still used separately in diff() for Tier 2 filtering of new files.
    let walker = WalkBuilder::new(repo_root)
        .hidden(false) // Don't skip hidden files
        .git_ignore(true) // Prune ignored directories during traversal
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            // Skip .git directory itself
            entry.file_name() != ".git"
        })
        .build();

    for result in walker {
        // Check timeout
        if start.elapsed().as_millis() > STAT_DIFF_TIMEOUT_MS as u128 {
            debug_log("Stat-diff timeout exceeded; returning partial snapshot");
            break;
        }

        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                debug_log(&format!("Walker error: {}", e));
                continue;
            }
        };

        let abs_path = entry.path();

        // Skip directories themselves (we only stat files).
        // Use entry.file_type() (lstat semantics) instead of abs_path.is_dir()
        // to avoid following symlinks — a symlink to a directory should be
        // snapshotted as a symlink entry, not skipped.
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or_else(|| abs_path.is_dir())
        {
            continue;
        }

        // Compute relative path from repo root
        let rel_path = match abs_path.strip_prefix(repo_root) {
            Ok(p) => p,
            Err(_) => continue, // Outside repo root
        };

        let normalized = normalize_path(rel_path);

        // Tier 1: always include tracked files
        // Tier 2: include new untracked files that pass gitignore
        let is_tracked = tracked_files.contains(&normalized);
        if !is_tracked && !should_include_new_file(&gitignore, rel_path, false) {
            continue;
        }

        // Collect stat via lstat (symlink_metadata)
        match fs::symlink_metadata(abs_path) {
            Ok(meta) => {
                entries.insert(normalized, StatEntry::from_metadata(&meta));
            }
            Err(e) => {
                debug_log(&format!("Failed to stat {}: {}", abs_path.display(), e));
                // ENOENT is fine (deleted during walk), others are warnings
            }
        }
    }

    // Second pass: ensure all git-tracked files are included even if the
    // walker's gitignore pruning skipped them (e.g. a tracked *.log file
    // that matches a .gitignore pattern). This preserves the Tier 1 guarantee
    // that tracked files are always in the snapshot.
    for tracked in &tracked_files {
        let normalized = normalize_path(tracked);
        if let std::collections::hash_map::Entry::Vacant(entry) = entries.entry(normalized) {
            let abs_path = repo_root.join(tracked);
            if let Ok(meta) = fs::symlink_metadata(&abs_path) {
                entry.insert(StatEntry::from_metadata(&meta));
            }
        }
    }

    let duration = start.elapsed();
    debug_log(&format!(
        "Snapshot: {} files scanned in {}ms",
        entries.len(),
        duration.as_millis()
    ));

    Ok(StatSnapshot {
        entries,
        tracked_files,
        gitignore: Some(gitignore),
        taken_at: Some(Instant::now()),
        invocation_key,
        repo_root: repo_root.to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Diff two snapshots to find created, modified, and deleted files.
///
/// Uses the pre-snapshot's frozen gitignore rules for Tier 2 filtering
/// of newly created files.
pub fn diff(pre: &StatSnapshot, post: &StatSnapshot) -> StatDiffResult {
    let mut result = StatDiffResult::default();

    let pre_keys: HashSet<&PathBuf> = pre.entries.keys().collect();
    let post_keys: HashSet<&PathBuf> = post.entries.keys().collect();

    // Created: in post but not pre
    for path in post_keys.difference(&pre_keys) {
        // For new files not in the tracked set, apply frozen gitignore
        let is_tracked = pre.tracked_files.contains(*path);
        if !is_tracked
            && let Some(ref gitignore) = pre.gitignore
            && !should_include_new_file(gitignore, path, false)
        {
            continue;
        }
        result.created.push((*path).clone());
    }

    // Deleted: in pre but not post
    for path in pre_keys.difference(&post_keys) {
        result.deleted.push((*path).clone());
    }

    // Modified: in both but stat-tuple differs
    for path in pre_keys.intersection(&post_keys) {
        let pre_entry = &pre.entries[*path];
        let post_entry = &post.entries[*path];
        if pre_entry != post_entry {
            result.modified.push((*path).clone());
        }
    }

    // Sort for deterministic output
    result.created.sort();
    result.modified.sort();
    result.deleted.sort();

    result
}

// ---------------------------------------------------------------------------
// Snapshot caching (file-based persistence)
// ---------------------------------------------------------------------------

/// Get the directory for storing bash snapshots.
fn snapshot_cache_dir(repo_root: &Path) -> Result<PathBuf, GitAiError> {
    // Find .git directory (handles worktrees)
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(repo_root)
        .output()
        .map_err(GitAiError::IoError)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(
            "Failed to find .git directory".to_string(),
        ));
    }

    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let git_dir_path = if Path::new(&git_dir).is_absolute() {
        PathBuf::from(&git_dir)
    } else {
        repo_root.join(&git_dir)
    };

    let cache_dir = git_dir_path.join("ai").join("bash_snapshots");
    fs::create_dir_all(&cache_dir).map_err(GitAiError::IoError)?;

    Ok(cache_dir)
}

/// Save a pre-snapshot to the cache.
pub fn save_snapshot(snapshot: &StatSnapshot) -> Result<(), GitAiError> {
    let cache_dir = snapshot_cache_dir(&snapshot.repo_root)?;
    let filename = sanitize_key(&snapshot.invocation_key);
    let path = cache_dir.join(format!("{}.json", filename));

    let data = serde_json::to_vec(snapshot).map_err(GitAiError::JsonError)?;

    fs::write(&path, data).map_err(GitAiError::IoError)?;

    debug_log(&format!(
        "Saved pre-snapshot: {} ({} entries)",
        path.display(),
        snapshot.entries.len()
    ));

    Ok(())
}

/// Load a pre-snapshot from the cache and remove it (consume).
pub fn load_and_consume_snapshot(
    repo_root: &Path,
    invocation_key: &str,
) -> Result<Option<StatSnapshot>, GitAiError> {
    let cache_dir = snapshot_cache_dir(repo_root)?;
    let filename = sanitize_key(invocation_key);
    let path = cache_dir.join(format!("{}.json", filename));

    if !path.exists() {
        return Ok(None);
    }

    let data = fs::read(&path).map_err(GitAiError::IoError)?;
    let snapshot: StatSnapshot = serde_json::from_slice(&data).map_err(GitAiError::JsonError)?;

    // Consume: remove the file after loading
    let _ = fs::remove_file(&path);

    debug_log(&format!(
        "Loaded pre-snapshot: {} ({} entries)",
        path.display(),
        snapshot.entries.len()
    ));

    Ok(Some(snapshot))
}

/// Clean up stale snapshots older than SNAPSHOT_STALE_SECS.
pub fn cleanup_stale_snapshots(repo_root: &Path) -> Result<(), GitAiError> {
    let cache_dir = snapshot_cache_dir(repo_root)?;

    if let Ok(entries) = fs::read_dir(&cache_dir) {
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(meta) = fs::metadata(&path)
                && let Ok(modified) = meta.modified()
                && let Ok(age) = now.duration_since(modified)
                && age.as_secs() > SNAPSHOT_STALE_SECS
            {
                debug_log(&format!("Cleaning stale snapshot: {}", path.display()));
                let _ = fs::remove_file(&path);
            }
        }
    }

    Ok(())
}

/// Sanitize an invocation key for use as a filename.
fn sanitize_key(key: &str) -> String {
    key.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
}

// ---------------------------------------------------------------------------
// Git status fallback
// ---------------------------------------------------------------------------

/// Fall back to `git status --porcelain=v2` to detect changed files.
/// Used when the pre-snapshot is lost (process restart) or on very large repos.
pub fn git_status_fallback(repo_root: &Path) -> Result<Vec<String>, GitAiError> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .current_dir(repo_root)
        .output()
        .map_err(GitAiError::IoError)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let mut changed_files = Vec::new();
    let parts: Vec<&[u8]> = output.stdout.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < parts.len() {
        let part = parts[i];
        if part.is_empty() {
            i += 1;
            continue;
        }

        let line = String::from_utf8_lossy(part);

        if line.starts_with("1 ") || line.starts_with("u ") {
            // Ordinary entry: 8 fields before path; unmerged: 10 fields before path
            let n = if line.starts_with("u ") { 11 } else { 9 };
            let fields: Vec<&str> = line.splitn(n, ' ').collect();
            if let Some(path) = fields.last() {
                changed_files.push(crate::utils::normalize_to_posix(path));
            }
        } else if line.starts_with("2 ") {
            // Rename/copy: 9 fields before new path, then NUL-delimited original path
            let fields: Vec<&str> = line.splitn(10, ' ').collect();
            if let Some(path) = fields.last() {
                changed_files.push(crate::utils::normalize_to_posix(path));
            }
            // Also include the original path (next NUL-delimited entry)
            if i + 1 < parts.len() {
                let orig = String::from_utf8_lossy(parts[i + 1]);
                if !orig.is_empty() {
                    changed_files.push(crate::utils::normalize_to_posix(&orig));
                }
            }
            i += 1;
        } else if let Some(path) = line.strip_prefix("? ") {
            // Untracked: path follows "? "
            changed_files.push(crate::utils::normalize_to_posix(path));
        }

        i += 1;
    }

    Ok(changed_files)
}

// ---------------------------------------------------------------------------
// Captured-checkpoint helpers
// ---------------------------------------------------------------------------

/// Convert a `SystemTime` to nanoseconds since UNIX epoch for watermark comparison.
#[allow(dead_code)]
fn system_time_to_nanos(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Read file contents for captured checkpoint, skipping binary/large/unreadable files.
#[allow(dead_code)]
fn capture_file_contents(repo_root: &Path, file_paths: &[PathBuf]) -> HashMap<String, String> {
    let mut contents = HashMap::new();
    for rel_path in file_paths {
        let abs_path = repo_root.join(rel_path);
        match fs::metadata(&abs_path) {
            Ok(meta) if meta.len() > MAX_CAPTURE_FILE_SIZE => {
                debug_log(&format!(
                    "Skipping large file for capture: {} ({} bytes)",
                    rel_path.display(),
                    meta.len()
                ));
                continue;
            }
            Err(e) => {
                debug_log(&format!(
                    "Skipping unreadable file for capture: {}: {}",
                    rel_path.display(),
                    e
                ));
                continue;
            }
            _ => {}
        }
        match fs::read_to_string(&abs_path) {
            Ok(content) => {
                let key = crate::utils::normalize_to_posix(&rel_path.to_string_lossy());
                contents.insert(key, content);
            }
            Err(e) => {
                debug_log(&format!(
                    "Skipping non-UTF8/unreadable file for capture: {}: {}",
                    rel_path.display(),
                    e
                ));
            }
        }
    }
    contents
}

// ---------------------------------------------------------------------------
// Daemon watermark query + stale file detection
// ---------------------------------------------------------------------------

/// Query the daemon for per-file mtime watermarks for a given repository.
///
/// Returns `None` on any failure (daemon not running, socket error, parse
/// error, etc.) for graceful degradation — the caller simply skips the
/// captured-checkpoint path when watermarks are unavailable.
#[allow(dead_code)]
fn query_daemon_watermarks(repo_working_dir: &str) -> Option<HashMap<String, u128>> {
    let config = DaemonConfig::from_env_or_default_paths().ok()?;
    let request = ControlRequest::SnapshotWatermarks {
        repo_working_dir: repo_working_dir.to_string(),
    };
    let response = send_control_request_with_timeout(
        &config.control_socket_path,
        &request,
        Duration::from_millis(500),
    )
    .ok()?;

    if !response.ok {
        debug_log(&format!(
            "Daemon watermark query returned error: {}",
            response.error.as_deref().unwrap_or("unknown")
        ));
        return None;
    }

    // The daemon returns `{ "watermarks": { "<path>": <u128>, ... } }`.
    let data = response.data?;
    let watermarks_value = data.get("watermarks")?;
    let map: HashMap<String, u128> = serde_json::from_value(watermarks_value.clone()).ok()?;
    Some(map)
}

/// Compare snapshot entries against daemon watermarks to find stale files.
///
/// A file is "stale" (modified since last checkpoint) when its `mtime`
/// exceeds the watermark for that path by more than `MTIME_GRACE_WINDOW_NS`.
/// Files not present in the watermark map are considered stale if they exist
/// in the snapshot.
#[allow(dead_code)]
fn find_stale_files(snapshot: &StatSnapshot, watermarks: &HashMap<String, u128>) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    for (rel_path, entry) in &snapshot.entries {
        if !entry.exists {
            continue;
        }
        let Some(mtime) = entry.mtime else {
            continue;
        };
        let mtime_ns = system_time_to_nanos(mtime);
        let posix_key = crate::utils::normalize_to_posix(&rel_path.to_string_lossy());

        match watermarks.get(&posix_key) {
            Some(&watermark_ns) => {
                if mtime_ns > watermark_ns + MTIME_GRACE_WINDOW_NS {
                    stale.push(rel_path.clone());
                }
            }
            None => {
                // No watermark means this file has never been checkpointed —
                // treat as stale so the pre-hook captures it.
                stale.push(rel_path.clone());
            }
        }
    }
    stale.sort();
    stale
}

// ---------------------------------------------------------------------------
// handle_bash_tool() — main orchestration
// ---------------------------------------------------------------------------

/// Handle a bash tool invocation.
///
/// On `PreToolUse`: takes a pre-snapshot and stores it.
/// On `PostToolUse`: takes a post-snapshot, diffs against the stored pre-snapshot,
/// and returns the list of changed files.
pub fn handle_bash_tool(
    hook_event: HookEvent,
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
) -> Result<BashToolResult, GitAiError> {
    let invocation_key = format!("{}:{}", session_id, tool_use_id);

    match hook_event {
        HookEvent::PreToolUse => {
            // Clean up stale snapshots
            let _ = cleanup_stale_snapshots(repo_root);

            // Take and store pre-snapshot
            match snapshot(repo_root, session_id, tool_use_id) {
                Ok(snap) => {
                    save_snapshot(&snap)?;
                    debug_log(&format!(
                        "Pre-snapshot stored for invocation {}",
                        invocation_key
                    ));
                    Ok(BashToolResult {
                        action: BashCheckpointAction::TakePreSnapshot,
                        captured_checkpoint: None,
                    })
                }
                Err(e) => {
                    debug_log(&format!(
                        "Pre-snapshot failed: {}; will use fallback on post",
                        e
                    ));
                    // Don't fail the tool call; post-hook will use git status fallback
                    Ok(BashToolResult {
                        action: BashCheckpointAction::TakePreSnapshot,
                        captured_checkpoint: None,
                    })
                }
            }
        }
        HookEvent::PostToolUse => {
            // Try to load the pre-snapshot
            let pre_snapshot = load_and_consume_snapshot(repo_root, &invocation_key)?;

            match pre_snapshot {
                Some(mut pre) => {
                    // Take post-snapshot
                    match snapshot(repo_root, session_id, tool_use_id) {
                        Ok(post) => {
                            // Rebuild gitignore from the pre-snapshot's repo root for filtering
                            if pre.gitignore.is_none() {
                                pre.gitignore = build_gitignore(&pre.repo_root).ok();
                            }

                            let diff_result = diff(&pre, &post);

                            if diff_result.is_empty() {
                                debug_log(&format!(
                                    "Bash tool {}: no changes detected",
                                    invocation_key
                                ));
                                Ok(BashToolResult {
                                    action: BashCheckpointAction::NoChanges,
                                    captured_checkpoint: None,
                                })
                            } else {
                                let paths = diff_result.all_changed_paths();
                                debug_log(&format!(
                                    "Bash tool {}: {} files changed ({} created, {} modified, {} deleted)",
                                    invocation_key,
                                    paths.len(),
                                    diff_result.created.len(),
                                    diff_result.modified.len(),
                                    diff_result.deleted.len(),
                                ));
                                Ok(BashToolResult {
                                    action: BashCheckpointAction::Checkpoint(paths),
                                    captured_checkpoint: None,
                                })
                            }
                        }
                        Err(e) => {
                            debug_log(&format!(
                                "Post-snapshot failed: {}; falling back to git status",
                                e
                            ));
                            // Fall back to git status
                            match git_status_fallback(repo_root) {
                                Ok(paths) if paths.is_empty() => Ok(BashToolResult {
                                    action: BashCheckpointAction::NoChanges,
                                    captured_checkpoint: None,
                                }),
                                Ok(paths) => Ok(BashToolResult {
                                    action: BashCheckpointAction::Checkpoint(paths),
                                    captured_checkpoint: None,
                                }),
                                Err(_) => Ok(BashToolResult {
                                    action: BashCheckpointAction::Fallback,
                                    captured_checkpoint: None,
                                }),
                            }
                        }
                    }
                }
                None => {
                    // Pre-snapshot lost (process restart, etc.) — use git status fallback
                    debug_log(&format!(
                        "Pre-snapshot not found for {}; using git status fallback",
                        invocation_key
                    ));
                    match git_status_fallback(repo_root) {
                        Ok(paths) if paths.is_empty() => Ok(BashToolResult {
                            action: BashCheckpointAction::NoChanges,
                            captured_checkpoint: None,
                        }),
                        Ok(paths) => Ok(BashToolResult {
                            action: BashCheckpointAction::Checkpoint(paths),
                            captured_checkpoint: None,
                        }),
                        Err(_) => Ok(BashToolResult {
                            action: BashCheckpointAction::Fallback,
                            captured_checkpoint: None,
                        }),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_stat_entry_from_metadata() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), "hello world").unwrap();
        let meta = fs::symlink_metadata(tmp.path()).unwrap();
        let entry = StatEntry::from_metadata(&meta);

        assert!(entry.exists);
        assert!(entry.mtime.is_some());
        assert_eq!(entry.size, 11);
        assert_eq!(entry.file_type, StatFileType::Regular);
    }

    #[test]
    fn test_stat_entry_equality() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), "hello").unwrap();
        let meta = fs::symlink_metadata(tmp.path()).unwrap();
        let entry1 = StatEntry::from_metadata(&meta);
        let entry2 = StatEntry::from_metadata(&meta);
        assert_eq!(entry1, entry2);
    }

    #[test]
    fn test_stat_entry_modification_detected() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::write(tmp.path(), "hello").unwrap();
        let meta1 = fs::symlink_metadata(tmp.path()).unwrap();
        let entry1 = StatEntry::from_metadata(&meta1);

        // Modify the file
        std::thread::sleep(Duration::from_millis(50));
        fs::write(tmp.path(), "hello world").unwrap();
        let meta2 = fs::symlink_metadata(tmp.path()).unwrap();
        let entry2 = StatEntry::from_metadata(&meta2);

        assert_ne!(entry1, entry2);
        assert_ne!(entry1.size, entry2.size);
    }

    #[test]
    fn test_normalize_path_consistency() {
        let path = Path::new("src/main.rs");
        let normalized = normalize_path(path);
        let normalized2 = normalize_path(path);
        assert_eq!(normalized, normalized2);
    }

    #[test]
    fn test_diff_empty_snapshots() {
        let pre = StatSnapshot {
            entries: HashMap::new(),
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };
        let post = StatSnapshot {
            entries: HashMap::new(),
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let result = diff(&pre, &post);
        assert!(result.is_empty());
    }

    #[test]
    fn test_diff_detects_creation() {
        let pre = StatSnapshot {
            entries: HashMap::new(),
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let mut post_entries = HashMap::new();
        post_entries.insert(
            normalize_path(Path::new("new_file.txt")),
            StatEntry {
                exists: true,
                mtime: Some(SystemTime::now()),
                ctime: Some(SystemTime::now()),
                size: 100,
                mode: 0o644,
                file_type: StatFileType::Regular,
            },
        );

        let post = StatSnapshot {
            entries: post_entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let result = diff(&pre, &post);
        assert_eq!(result.created.len(), 1);
        assert!(result.modified.is_empty());
        assert!(result.deleted.is_empty());
    }

    #[test]
    fn test_diff_detects_deletion() {
        let mut pre_entries = HashMap::new();
        let path = normalize_path(Path::new("deleted.txt"));
        pre_entries.insert(
            path.clone(),
            StatEntry {
                exists: true,
                mtime: Some(SystemTime::now()),
                ctime: Some(SystemTime::now()),
                size: 50,
                mode: 0o644,
                file_type: StatFileType::Regular,
            },
        );

        let pre = StatSnapshot {
            entries: pre_entries,
            tracked_files: {
                let mut s = HashSet::new();
                s.insert(path);
                s
            },
            gitignore: None,
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let post = StatSnapshot {
            entries: HashMap::new(),
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let result = diff(&pre, &post);
        assert!(result.created.is_empty());
        assert!(result.modified.is_empty());
        assert_eq!(result.deleted.len(), 1);
    }

    #[test]
    fn test_diff_detects_modification() {
        let path = normalize_path(Path::new("modified.txt"));
        let now = SystemTime::now();
        let later = now + Duration::from_secs(1);

        let mut pre_entries = HashMap::new();
        pre_entries.insert(
            path.clone(),
            StatEntry {
                exists: true,
                mtime: Some(now),
                ctime: Some(now),
                size: 50,
                mode: 0o644,
                file_type: StatFileType::Regular,
            },
        );

        let mut post_entries = HashMap::new();
        post_entries.insert(
            path.clone(),
            StatEntry {
                exists: true,
                mtime: Some(later),
                ctime: Some(later),
                size: 75,
                mode: 0o644,
                file_type: StatFileType::Regular,
            },
        );

        let pre = StatSnapshot {
            entries: pre_entries,
            tracked_files: {
                let mut s = HashSet::new();
                s.insert(path);
                s
            },
            gitignore: None,
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let post = StatSnapshot {
            entries: post_entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
        };

        let result = diff(&pre, &post);
        assert!(result.created.is_empty());
        assert_eq!(result.modified.len(), 1);
        assert!(result.deleted.is_empty());
    }

    #[test]
    fn test_tool_classification_claude() {
        assert_eq!(classify_tool(Agent::Claude, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Claude, "Edit"), ToolClass::FileEdit);
        assert_eq!(
            classify_tool(Agent::Claude, "MultiEdit"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Claude, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Claude, "Read"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Claude, "unknown"), ToolClass::Skip);
    }

    #[test]
    fn test_tool_classification_all_agents() {
        // Gemini
        assert_eq!(
            classify_tool(Agent::Gemini, "write_file"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Gemini, "shell"), ToolClass::Bash);

        // Continue CLI
        assert_eq!(
            classify_tool(Agent::ContinueCli, "edit"),
            ToolClass::FileEdit
        );
        assert_eq!(
            classify_tool(Agent::ContinueCli, "terminal"),
            ToolClass::Bash
        );
        assert_eq!(
            classify_tool(Agent::ContinueCli, "local_shell_call"),
            ToolClass::Bash
        );

        // Droid
        assert_eq!(
            classify_tool(Agent::Droid, "ApplyPatch"),
            ToolClass::FileEdit
        );
        assert_eq!(classify_tool(Agent::Droid, "Bash"), ToolClass::Bash);

        // Amp
        assert_eq!(classify_tool(Agent::Amp, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Amp, "Bash"), ToolClass::Bash);

        // OpenCode
        assert_eq!(classify_tool(Agent::OpenCode, "edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::OpenCode, "bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::OpenCode, "shell"), ToolClass::Bash);
    }

    #[test]
    fn test_sanitize_key() {
        assert_eq!(sanitize_key("session:tool"), "session_tool");
        assert_eq!(sanitize_key("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_key("normal_key"), "normal_key");
    }

    #[test]
    fn test_stat_diff_result_all_changed_paths() {
        let result = StatDiffResult {
            created: vec![PathBuf::from("new.txt")],
            modified: vec![PathBuf::from("changed.txt")],
            deleted: vec![PathBuf::from("removed.txt")],
        };
        let paths = result.all_changed_paths();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&"new.txt".to_string()));
        assert!(paths.contains(&"changed.txt".to_string()));
        assert!(paths.contains(&"removed.txt".to_string()));
    }
}
