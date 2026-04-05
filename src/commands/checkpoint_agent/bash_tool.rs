//! Bash tool change attribution via pre/post stat-tuple snapshots.
//!
//! Detects file changes made by bash/shell tool calls by comparing filesystem
//! metadata snapshots taken before and after tool execution.

use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::prepare_captured_checkpoint;
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::config;
use crate::daemon::control_api::ControlRequest;
use crate::daemon::{DaemonConfig, send_control_request_with_timeout};
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
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
const MTIME_GRACE_WINDOW_NS: u128 = (_MTIME_GRACE_WINDOW_SECS as u128) * 1_000_000_000;

/// Maximum number of stale files before skipping content capture.
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
pub struct BashToolResult {
    /// The checkpoint action (unchanged from previous API).
    pub action: BashCheckpointAction,
    /// If set, a captured checkpoint was prepared and needs submission by the handler.
    pub captured_checkpoint: Option<CapturedCheckpointInfo>,
}

/// Info about a captured checkpoint prepared by the bash tool.
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
fn system_time_to_nanos(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Read file contents for captured checkpoint, skipping binary/large/unreadable files.
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
/// Watermarks returned by the daemon for a single worktree.
struct DaemonWatermarks {
    /// Per-file mtime watermarks from scoped checkpoints.
    per_file: HashMap<String, u128>,
    /// Timestamp of the last full (non-scoped) Human checkpoint, if any.
    /// `None` on cold start (daemon has never processed a full checkpoint).
    worktree: Option<u128>,
}

fn query_daemon_watermarks(repo_working_dir: &str) -> Option<DaemonWatermarks> {
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

    // The daemon returns `{ "watermarks": {...}, "worktree_watermark": <u128|null> }`.
    let data = response.data?;
    let per_file: HashMap<String, u128> = data
        .get("watermarks")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let worktree: Option<u128> = data
        .get("worktree_watermark")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Some(DaemonWatermarks { per_file, worktree })
}

/// Compare snapshot entries against daemon watermarks to find stale files.
///
/// Three-tier logic per file:
/// 1. Per-file watermark exists → stale if `mtime > watermark + GRACE`.
/// 2. No per-file watermark but worktree watermark exists → stale if
///    `mtime > worktree_watermark + GRACE`.
/// 3. Neither watermark → not included here; the caller handles cold-start via
///    `git status` (see `attempt_pre_hook_capture`).
fn find_stale_files(snapshot: &StatSnapshot, wm: &DaemonWatermarks) -> Vec<PathBuf> {
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

        match wm.per_file.get(&posix_key) {
            Some(&file_wm) => {
                // Tier 1: precise per-file watermark from a prior scoped checkpoint.
                if mtime_ns > file_wm + MTIME_GRACE_WINDOW_NS {
                    stale.push(rel_path.clone());
                }
            }
            None => {
                // Tier 2: fall back to worktree-level watermark.
                if let Some(worktree_wm) = wm.worktree {
                    if mtime_ns > worktree_wm + MTIME_GRACE_WINDOW_NS {
                        stale.push(rel_path.clone());
                    }
                }
                // Tier 3 (no worktree watermark): cold-start, handled in caller.
            }
        }
    }
    stale.sort();
    stale
}

// ---------------------------------------------------------------------------
// Pre/post hook captured-checkpoint helpers
// ---------------------------------------------------------------------------

/// Attempt to prepare a captured checkpoint during the pre-hook.
///
/// Queries daemon watermarks, identifies stale files (modified since the last
/// checkpoint), captures their contents, and prepares a captured checkpoint
/// with `CheckpointKind::Human` and `will_edit_filepaths`.
///
/// Returns `None` on any failure or when no stale files are found, allowing
/// the caller to proceed without a captured checkpoint.
fn attempt_pre_hook_capture(
    snap: &StatSnapshot,
    repo_root: &Path,
) -> Option<CapturedCheckpointInfo> {
    if !captured_checkpoint_delegate_enabled() {
        debug_log("Pre-hook capture: async checkpoint delegation not enabled, skipping capture");
        return None;
    }

    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // 1. Query daemon watermarks (graceful degradation on failure).
    let wm = query_daemon_watermarks(&repo_working_dir)?;

    // 2. Find stale files (tiers 1 & 2: per-file and worktree watermarks).
    let mut stale_files = find_stale_files(snap, &wm);

    // 3. Cold-start: no worktree watermark means we have no baseline for files
    //    that have never appeared in a scoped checkpoint. Use `git status` to
    //    find only actually-dirty files instead of scanning all tracked files.
    if wm.worktree.is_none() {
        match git_status_fallback(repo_root) {
            Ok(dirty_paths) => {
                for path_str in dirty_paths {
                    let posix_key = crate::utils::normalize_to_posix(&path_str);
                    // Skip if already covered by a per-file watermark comparison.
                    if wm.per_file.contains_key(&posix_key) {
                        continue;
                    }
                    // Normalize to match the case-folded keys already in stale_files
                    // (from find_stale_files which uses normalize_path). On
                    // case-insensitive FS (macOS/Windows), git status returns
                    // original-case paths that would otherwise fail the contains check.
                    let p = normalize_path(Path::new(&path_str));
                    if !stale_files.contains(&p) {
                        stale_files.push(p);
                    }
                }
                stale_files.sort();
            }
            Err(e) => {
                debug_log(&format!(
                    "Pre-hook capture: git status fallback failed: {}",
                    e
                ));
            }
        }
    }

    if stale_files.is_empty() {
        debug_log("Pre-hook capture: no stale files found, skipping");
        return None;
    }
    if stale_files.len() > MAX_STALE_FILES_FOR_CAPTURE {
        debug_log(&format!(
            "Pre-hook capture: {} stale files exceeds limit of {}, skipping",
            stale_files.len(),
            MAX_STALE_FILES_FOR_CAPTURE,
        ));
        return None;
    }

    // 4. Capture file contents for the stale files.
    let contents = capture_file_contents(repo_root, &stale_files);

    // 5. Open the repository.
    let repo = match find_repository_in_path(&repo_working_dir) {
        Ok(r) => r,
        Err(e) => {
            debug_log(&format!("Pre-hook capture: failed to open repo: {}", e));
            return None;
        }
    };

    // 6. Build stale paths as posix-normalized strings.
    let stale_paths: Vec<String> = stale_files
        .iter()
        .map(|p| crate::utils::normalize_to_posix(&p.to_string_lossy()))
        .collect();

    // 7. Build a synthetic AgentRunResult for the captured checkpoint.
    let agent_run_result = AgentRunResult {
        agent_id: AgentId {
            tool: "bash-tool".to_string(),
            id: "pre-hook".to_string(),
            model: String::new(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::Human,
        transcript: None,
        repo_working_dir: Some(repo_working_dir.clone()),
        edited_filepaths: None,
        will_edit_filepaths: Some(stale_paths),
        dirty_files: Some(contents),
        captured_checkpoint_id: None,
    };

    // 8. Prepare the captured checkpoint.
    match prepare_captured_checkpoint(
        &repo,
        "bash-tool", // author
        CheckpointKind::Human,
        Some(&agent_run_result),
        false, // is_pre_commit
        None,  // base_commit_override
    ) {
        Ok(Some(capture)) => {
            debug_log(&format!(
                "Pre-hook captured checkpoint prepared: {} ({} files)",
                capture.capture_id, capture.file_count,
            ));
            Some(CapturedCheckpointInfo {
                capture_id: capture.capture_id,
                repo_working_dir: capture.repo_working_dir,
            })
        }
        Ok(None) => {
            debug_log("Pre-hook capture: prepare_captured_checkpoint returned None");
            None
        }
        Err(e) => {
            debug_log(&format!(
                "Pre-hook capture: prepare_captured_checkpoint failed: {}",
                e
            ));
            None
        }
    }
}

/// Check whether async/daemon checkpoint delegation is enabled.
///
/// Mirrors the logic of `daemon_checkpoint_delegate_enabled()` in
/// `git_ai_handlers.rs` so the bash tool can skip capture work when the
/// daemon will not be available to consume the files.
fn captured_checkpoint_delegate_enabled() -> bool {
    if config::Config::get().feature_flags().async_mode {
        return true;
    }
    matches!(
        std::env::var("GIT_AI_DAEMON_CHECKPOINT_DELEGATE")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Attempt to prepare a captured checkpoint during the post-hook.
///
/// Captures the current contents of changed files and prepares a captured
/// checkpoint with `CheckpointKind::AiAgent` and `edited_filepaths`.
///
/// Returns `None` on any failure, allowing the caller to proceed without a
/// captured checkpoint (the stat-diff paths are still returned for the
/// live checkpoint path).
fn attempt_post_hook_capture(
    repo_root: &Path,
    changed_paths: &[String],
) -> Option<CapturedCheckpointInfo> {
    // Guard: only attempt capture when async/daemon checkpoint delegation is
    // enabled.  Captured checkpoint files are only consumed/cleaned-up by the
    // daemon, so without this check they would accumulate indefinitely.
    // This mirrors the daemon_checkpoint_delegate_enabled() check used by the
    // handler in run_checkpoint_via_daemon_or_local.
    if !captured_checkpoint_delegate_enabled() {
        debug_log("Post-hook capture: async checkpoint delegation not enabled, skipping capture");
        return None;
    }

    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // 1. Split changed paths into existing and deleted. Deleted files have no
    //    capturable post-content; recording them as empty blobs in the captured
    //    checkpoint would misrepresent deletions as "file was emptied". The live
    //    checkpoint path (BashCheckpointAction::Checkpoint) already carries all
    //    changed paths including deletions for attribution tracking.
    let (existing_paths, _deleted_paths): (Vec<&String>, Vec<&String>) = changed_paths
        .iter()
        .partition(|p| repo_root.join(p.as_str()).exists());

    if existing_paths.is_empty() {
        debug_log("Post-hook capture: no existing changed files to capture, skipping");
        return None;
    }

    // 2. Convert to PathBuf and capture file contents (existing files only).
    let path_bufs: Vec<PathBuf> = existing_paths
        .iter()
        .map(|p| PathBuf::from(p.as_str()))
        .collect();
    let contents = capture_file_contents(repo_root, &path_bufs);

    // 3. Open the repository.
    let repo = match find_repository_in_path(&repo_working_dir) {
        Ok(r) => r,
        Err(e) => {
            debug_log(&format!("Post-hook capture: failed to open repo: {}", e));
            return None;
        }
    };

    // 4. Build a synthetic AgentRunResult for the captured checkpoint.
    // Only include existing files — deleted files have no post-content to capture.
    let existing_path_strings: Vec<String> = existing_paths.iter().map(|p| p.to_string()).collect();
    let agent_run_result = AgentRunResult {
        agent_id: AgentId {
            tool: "bash-tool".to_string(),
            id: "post-hook".to_string(),
            model: String::new(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::AiAgent,
        transcript: None,
        repo_working_dir: Some(repo_working_dir.clone()),
        edited_filepaths: Some(existing_path_strings),
        will_edit_filepaths: None,
        dirty_files: Some(contents),
        captured_checkpoint_id: None,
    };

    // 5. Prepare the captured checkpoint.
    match prepare_captured_checkpoint(
        &repo,
        "bash-tool", // author
        CheckpointKind::AiAgent,
        Some(&agent_run_result),
        false, // is_pre_commit
        None,  // base_commit_override
    ) {
        Ok(Some(capture)) => {
            debug_log(&format!(
                "Post-hook captured checkpoint prepared: {} ({} files)",
                capture.capture_id, capture.file_count,
            ));
            Some(CapturedCheckpointInfo {
                capture_id: capture.capture_id,
                repo_working_dir: capture.repo_working_dir,
            })
        }
        Ok(None) => {
            debug_log("Post-hook capture: prepare_captured_checkpoint returned None");
            None
        }
        Err(e) => {
            debug_log(&format!(
                "Post-hook capture: prepare_captured_checkpoint failed: {}",
                e
            ));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Invocation-ID sidecar helpers
// ---------------------------------------------------------------------------

/// Sidecar file path used to correlate pre and post hooks for agents that do
/// not provide a unique per-call `tool_use_id` (e.g. Gemini CLI, ContinueCli).
fn bash_sidecar_path(repo_root: &Path, session_id: &str) -> Option<PathBuf> {
    snapshot_cache_dir(repo_root)
        .ok()
        .map(|d| d.join(format!("last_id_{}.txt", sanitize_key(session_id))))
}

/// Resolve the effective `tool_use_id` for a bash tool invocation.
///
/// When the caller passes the generic fallback `"bash"` (meaning the agent did
/// not supply a unique per-call identifier), we generate a fresh UUID at
/// pre-hook time and persist it in a small sidecar file, then read it back at
/// post-hook time. This prevents snapshot key collisions when two bash calls
/// share the same session.
///
/// Returns the resolved ID (either the original or a sidecar-backed UUID).
fn resolve_bash_tool_use_id(
    hook_event: HookEvent,
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
) -> String {
    const FALLBACK: &str = "bash";
    if tool_use_id != FALLBACK {
        return tool_use_id.to_string();
    }

    // The caller passed the generic "bash" fallback — use the sidecar mechanism.
    match hook_event {
        HookEvent::PreToolUse => {
            let id = uuid::Uuid::new_v4().to_string();
            if let Some(path) = bash_sidecar_path(repo_root, session_id) {
                if let Err(e) = fs::write(&path, &id) {
                    debug_log(&format!(
                        "bash sidecar write failed ({}): {}",
                        path.display(),
                        e
                    ));
                }
            }
            id
        }
        HookEvent::PostToolUse => {
            let path = bash_sidecar_path(repo_root, session_id);
            if let Some(ref p) = path
                && let Ok(id) = fs::read_to_string(p)
            {
                let id = id.trim().to_string();
                // Consume the sidecar so it doesn't linger after the post-hook.
                let _ = fs::remove_file(p);
                if !id.is_empty() {
                    return id;
                }
            }
            // No sidecar found — caller passed "bash" for both hooks without a
            // matching pre-hook; fall back to the literal key (best effort).
            debug_log("bash sidecar not found for post-hook; using 'bash' as fallback key");
            FALLBACK.to_string()
        }
    }
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
    // Resolve the effective tool_use_id — generates/reads a sidecar UUID when
    // the caller passes the generic "bash" fallback (no per-call ID from agent).
    let tool_use_id = resolve_bash_tool_use_id(hook_event, repo_root, session_id, tool_use_id);
    let tool_use_id = tool_use_id.as_str();
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

                    // Attempt watermark-based pre-hook content capture.
                    let captured_checkpoint = attempt_pre_hook_capture(&snap, repo_root);

                    Ok(BashToolResult {
                        action: BashCheckpointAction::TakePreSnapshot,
                        captured_checkpoint,
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

                                // Attempt post-hook content capture for async checkpoint.
                                let captured_checkpoint =
                                    attempt_post_hook_capture(repo_root, &paths);

                                Ok(BashToolResult {
                                    action: BashCheckpointAction::Checkpoint(paths),
                                    captured_checkpoint,
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

    // -----------------------------------------------------------------------
    // system_time_to_nanos tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_system_time_to_nanos() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        assert_eq!(system_time_to_nanos(t), 1_000_000_000);
    }

    #[test]
    fn test_system_time_to_nanos_epoch() {
        assert_eq!(system_time_to_nanos(SystemTime::UNIX_EPOCH), 0);
    }

    // -----------------------------------------------------------------------
    // find_stale_files tests
    // -----------------------------------------------------------------------

    /// Helper: build a minimal `StatSnapshot` with the given entries.
    fn make_snapshot(entries: HashMap<PathBuf, StatEntry>) -> StatSnapshot {
        StatSnapshot {
            entries,
            tracked_files: HashSet::new(),
            gitignore: None,
            taken_at: None,
            invocation_key: "test:stale".to_string(),
            repo_root: PathBuf::from("/tmp"),
        }
    }

    /// Helper: build a `StatEntry` for a regular file with the given mtime.
    fn make_entry(mtime_secs: u64, exists: bool) -> StatEntry {
        let mtime = if exists {
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs))
        } else {
            None
        };
        StatEntry {
            exists,
            mtime,
            ctime: mtime, // ctime not used by find_stale_files
            size: 100,
            mode: 0o644,
            file_type: StatFileType::Regular,
        }
    }

    fn make_daemon_watermarks(
        per_file: HashMap<String, u128>,
        worktree: Option<u128>,
    ) -> DaemonWatermarks {
        DaemonWatermarks { per_file, worktree }
    }

    #[test]
    fn test_find_stale_files_cold_start_excludes_unwatermarked_files() {
        // On cold start (no per-file and no worktree watermark), files with no
        // watermark are NOT returned by find_stale_files — they are handled via
        // git status in attempt_pre_hook_capture instead.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot(entries);
        let wm = make_daemon_watermarks(HashMap::new(), None);

        let stale = find_stale_files(&snapshot, &wm);
        assert!(
            stale.is_empty(),
            "cold-start: unwatermarked files not returned; git status handles them"
        );
    }

    #[test]
    fn test_find_stale_files_uses_worktree_watermark_as_fallback() {
        // File has no per-file watermark, but worktree watermark exists at 90s.
        // File mtime is 100s → beyond grace window → stale.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot(entries);
        let wm = make_daemon_watermarks(HashMap::new(), Some(Duration::from_secs(90).as_nanos()));

        let stale = find_stale_files(&snapshot, &wm);
        assert_eq!(
            stale.len(),
            1,
            "file modified after worktree watermark is stale"
        );
    }

    #[test]
    fn test_find_stale_files_worktree_watermark_within_grace() {
        // File mtime=100s, worktree watermark=99s → within 2s grace → NOT stale.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot(entries);
        let wm = make_daemon_watermarks(HashMap::new(), Some(Duration::from_secs(99).as_nanos()));

        let stale = find_stale_files(&snapshot, &wm);
        assert!(
            stale.is_empty(),
            "file within grace of worktree watermark is not stale"
        );
    }

    #[test]
    fn test_find_stale_files_per_file_wins_over_worktree() {
        // Per-file watermark (95s) is older than worktree watermark (98s).
        // File mtime=100s → 5s beyond per-file watermark → stale.
        // (Even though it would also be stale via worktree watermark, the
        // per-file path is taken.)
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));
        let snapshot = make_snapshot(entries);

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(95).as_nanos());
        let wm = make_daemon_watermarks(per_file, Some(Duration::from_secs(98).as_nanos()));

        let stale = find_stale_files(&snapshot, &wm);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn test_find_stale_files_within_grace_window() {
        // File with mtime=100s, per-file watermark at 99s.
        // Difference is 1s which is within the 2s grace window -> NOT stale.
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));
        let snapshot = make_snapshot(entries);

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(99).as_nanos());
        let wm = make_daemon_watermarks(per_file, None);

        let stale = find_stale_files(&snapshot, &wm);
        assert!(
            stale.is_empty(),
            "file within grace window should not be stale"
        );
    }

    #[test]
    fn test_find_stale_files_beyond_grace_window() {
        // File with mtime=100s, per-file watermark at 95s.
        // Difference is 5s which exceeds the 2s grace window -> stale.
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));
        let snapshot = make_snapshot(entries);

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(95).as_nanos());
        let wm = make_daemon_watermarks(per_file, None);

        let stale = find_stale_files(&snapshot, &wm);
        assert_eq!(stale.len(), 1, "file beyond grace window should be stale");
    }

    #[test]
    fn test_find_stale_files_nonexistent_skipped() {
        // File with exists=false should not appear in stale list regardless of watermarks.
        let mut entries = HashMap::new();
        entries.insert(normalize_path(Path::new("gone.rs")), make_entry(100, false));
        let snapshot = make_snapshot(entries);
        let wm = make_daemon_watermarks(HashMap::new(), Some(0));

        let stale = find_stale_files(&snapshot, &wm);
        assert!(stale.is_empty(), "nonexistent file should not be stale");
    }

    // -----------------------------------------------------------------------
    // capture_file_contents tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_capture_file_contents_reads_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        fs::write(&file_path, "hello world").unwrap();

        let contents = capture_file_contents(dir.path(), &[PathBuf::from("hello.txt")]);
        assert_eq!(contents.get("hello.txt").unwrap(), "hello world",);
    }

    #[test]
    fn test_capture_file_contents_skips_missing() {
        let dir = tempfile::tempdir().unwrap();
        let contents = capture_file_contents(dir.path(), &[PathBuf::from("nonexistent.txt")]);
        assert!(contents.is_empty());
    }

    /// Verify that the sidecar mechanism correlates pre and post hooks when no unique
    /// tool_use_id is provided (the "bash" fallback case).
    ///
    /// Two sequential pre-hooks must produce distinct invocation keys so their
    /// snapshots do not collide.
    #[test]
    fn test_bash_sidecar_generates_unique_ids_per_pre_hook() {
        let dir = tempfile::tempdir().unwrap();
        // Initialise a real git repo so snapshot_cache_dir works.
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let session_id = "test-session-abc";

        // First pre-hook: should generate UUID1
        let id1 = resolve_bash_tool_use_id(HookEvent::PreToolUse, dir.path(), session_id, "bash");

        // Second pre-hook (while first post-hook hasn't fired yet): should generate UUID2
        let id2 = resolve_bash_tool_use_id(HookEvent::PreToolUse, dir.path(), session_id, "bash");

        assert_ne!(id1, id2, "sequential pre-hooks must produce different IDs");
        assert_ne!(id1, "bash");
        assert_ne!(id2, "bash");

        // Post-hook reads back the LAST written sidecar (id2)
        let id_post =
            resolve_bash_tool_use_id(HookEvent::PostToolUse, dir.path(), session_id, "bash");
        assert_eq!(
            id_post, id2,
            "post-hook should recover the last pre-hook ID"
        );

        // Sidecar is consumed; second post-hook falls back to "bash"
        let id_post2 =
            resolve_bash_tool_use_id(HookEvent::PostToolUse, dir.path(), session_id, "bash");
        assert_eq!(
            id_post2, "bash",
            "after sidecar consumed, falls back to 'bash'"
        );
    }

    #[test]
    fn test_bash_sidecar_not_triggered_when_real_id_provided() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let real_id = "toolu_real_unique_id_12345";
        let resolved =
            resolve_bash_tool_use_id(HookEvent::PreToolUse, dir.path(), "session", real_id);
        assert_eq!(resolved, real_id, "real IDs pass through unchanged");

        // No sidecar file should have been created
        let sidecar = bash_sidecar_path(dir.path(), "session");
        if let Some(path) = sidecar {
            assert!(!path.exists(), "sidecar must not be created for real IDs");
        }
    }

    /// Verify that attempt_post_hook_capture does not pass deleted files as edited_filepaths.
    /// Deleted files have no post-content to capture; passing them would store empty blobs.
    #[test]
    fn test_post_hook_capture_excludes_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("modified.txt");
        let deleted = "deleted.txt"; // does not exist on disk

        fs::write(&existing, b"new content").unwrap();

        // Simulate changed_paths from diff (includes both modified and deleted)
        let changed_paths = vec!["modified.txt".to_string(), deleted.to_string()];

        let (existing_paths, deleted_paths): (Vec<&String>, Vec<&String>) = changed_paths
            .iter()
            .partition(|p| dir.path().join(p.as_str()).exists());

        assert_eq!(existing_paths.len(), 1, "one existing file");
        assert_eq!(existing_paths[0], "modified.txt");
        assert_eq!(deleted_paths.len(), 1, "one deleted file");
        assert_eq!(deleted_paths[0], "deleted.txt");

        // Captured checkpoint edited_filepaths must only contain existing files
        let captured_edited: Vec<String> = existing_paths.iter().map(|p| p.to_string()).collect();
        assert!(!captured_edited.contains(&deleted.to_string()));
        assert!(captured_edited.contains(&"modified.txt".to_string()));
    }

    /// Verify that cold-start dedup normalizes paths from git_status before comparing
    /// against stale_files entries (which use normalize_path / case-folded keys).
    ///
    /// On case-insensitive filesystems (macOS/Windows), `git status` returns original-case
    /// paths (e.g. "src/Foo.rs") while `find_stale_files` inserts normalized paths
    /// (e.g. "src/foo.rs" on macOS). Without normalization the contains() check misses
    /// the duplicate and the file ends up in stale_files twice.
    #[test]
    fn test_cold_start_dedup_normalizes_case_before_contains_check() {
        // Simulate a stale_files list already populated with a normalize_path() entry.
        // normalize_path lowercases on case-insensitive targets; to make this test
        // deterministic on all platforms we directly use the normalized form.
        let normalized = normalize_path(Path::new("src/Foo.rs"));
        let mut stale_files: Vec<PathBuf> = vec![normalized.clone()];

        // Simulate git status returning the same file with its original case.
        let git_status_path = "src/Foo.rs".to_string();

        // Apply the same normalization the production code now uses.
        let p = normalize_path(Path::new(&git_status_path));
        if !stale_files.contains(&p) {
            stale_files.push(p);
        }

        // The file must appear exactly once regardless of case differences.
        assert_eq!(
            stale_files.len(),
            1,
            "duplicate after cold-start dedup: stale_files = {:?}",
            stale_files
        );
    }
}
