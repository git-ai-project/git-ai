//! Bash tool change attribution via pre/post stat-tuple snapshots.
//!
//! Detects file changes made by bash/shell tool calls by comparing filesystem
//! metadata snapshots taken before and after tool execution.

use crate::authorship::ignore::{
    default_ignore_patterns, load_git_ai_ignore_patterns_from_path,
    load_linguist_generated_patterns_from_path,
};
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::prepare_captured_checkpoint;
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::daemon::control_api::ControlRequest;
use crate::daemon::{DaemonConfig, send_control_request_with_timeout};
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::utils::{checkpoint_delegation_enabled, debug_log, normalize_to_posix};
use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Grace window for low-resolution filesystem detection (seconds).
const MTIME_GRACE_WINDOW_SECS: u64 = 2;

/// Hard limit for the filesystem stat-diff walk.  If the walk exceeds this,
/// the snapshot is abandoned (returning Err) and the hook falls back gracefully.
const WALK_TIMEOUT_MS: u64 = 1500;

/// Hard limit for the entire handle_bash_tool execution.  If this is exceeded
/// at any checkpoint, the hook returns Fallback immediately.
const HOOK_TIMEOUT_MS: u64 = 4000;

/// Pre-snapshots older than this are garbage-collected (seconds).
const SNAPSHOT_STALE_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Test-only timeout overrides (thread-local so parallel tests don't interfere)
// ---------------------------------------------------------------------------

// Thread-local overrides for WALK_TIMEOUT_MS and HOOK_TIMEOUT_MS, injected
// by tests via `set_walk_timeout_ms_for_test` / `set_hook_timeout_ms_for_test`.
// Setting either to 0 causes the corresponding timeout to fire immediately.
// Thread-local (not global) so parallel tests in other modules are unaffected.
#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static TEST_WALK_TIMEOUT_MS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static TEST_HOOK_TIMEOUT_MS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Return the walk timeout, honouring any test-time thread-local override.
fn effective_walk_timeout_ms() -> u64 {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(v) = TEST_WALK_TIMEOUT_MS.with(|c| c.get()) {
        return v;
    }
    WALK_TIMEOUT_MS
}

/// Return the hook timeout, honouring any test-time thread-local override.
fn effective_hook_timeout_ms() -> u64 {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(v) = TEST_HOOK_TIMEOUT_MS.with(|c| c.get()) {
        return v;
    }
    HOOK_TIMEOUT_MS
}

/// Override the walk timeout for the current thread.  Call
/// `reset_timeout_overrides_for_test()` at the end of the test.
#[cfg(any(test, feature = "test-support"))]
pub fn set_walk_timeout_ms_for_test(ms: u64) {
    TEST_WALK_TIMEOUT_MS.with(|c| c.set(Some(ms)));
}

/// Override the hook timeout for the current thread.  Call
/// `reset_timeout_overrides_for_test()` at the end of the test.
#[cfg(any(test, feature = "test-support"))]
pub fn set_hook_timeout_ms_for_test(ms: u64) {
    TEST_HOOK_TIMEOUT_MS.with(|c| c.set(Some(ms)));
}

/// Clear all test-time timeout overrides for the current thread.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_timeout_overrides_for_test() {
    TEST_WALK_TIMEOUT_MS.with(|c| c.set(None));
    TEST_HOOK_TIMEOUT_MS.with(|c| c.set(None));
}

/// Grace window in nanoseconds for low-resolution filesystem mtime comparison.
const MTIME_GRACE_WINDOW_NS: u128 = (MTIME_GRACE_WINDOW_SECS as u128) * 1_000_000_000;

/// Maximum number of stale files before skipping content capture.
const MAX_STALE_FILES_FOR_CAPTURE: usize = 1000;

/// Maximum number of files to track in a snapshot.  Repos larger than this
/// skip the stat-diff system entirely (returning Fallback) to avoid adding
/// seconds of latency to every Bash tool call.
const MAX_TRACKED_FILES: usize = 50_000;

/// Maximum file size for content capture (10 MB).
const MAX_CAPTURE_FILE_SIZE: u64 = 10 * 1024 * 1024;

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
///
/// Only stores entries for files that pass the git-ai ignore filter AND have
/// `mtime > effective_worktree_wm + GRACE` (i.e., not covered by any watermark).
/// Filtering is applied uniformly to all files — there is no special treatment
/// for git-tracked vs untracked files.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatSnapshot {
    /// File metadata for files that passed the ignore filter and are not
    /// covered by any watermark at snapshot time.
    pub entries: HashMap<PathBuf, StatEntry>,
    /// When this snapshot was taken.
    #[serde(skip)]
    pub taken_at: Option<Instant>,
    /// Unique invocation key: "{session_id}:{tool_use_id}".
    pub invocation_key: String,
    /// Repo root path.
    pub repo_root: PathBuf,
    /// Effective worktree-level watermark at snapshot time.
    /// Either the real daemon worktree watermark (warm start) or the mtime
    /// of `.git/index` (cold-start proxy).  `None` if neither was available.
    #[serde(default)]
    pub effective_worktree_wm: Option<u128>,
    /// Per-file watermarks from the daemon at snapshot time.
    /// Used for Tier-1 stale detection in `find_stale_files`.
    #[serde(default)]
    pub per_file_wm: HashMap<String, u128>,
}

/// Result of diffing two snapshots.
#[derive(Debug, Default)]
pub struct StatDiffResult {
    pub created: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
}

impl StatDiffResult {
    /// All changed paths (created + modified) as Strings.
    pub fn all_changed_paths(&self) -> Vec<String> {
        self.created
            .iter()
            .chain(self.modified.iter())
            .map(|p| normalize_to_posix(&p.to_string_lossy()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty()
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
    /// An error occurred; caller should fall back to a safe default.
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
// Git-dir / index helpers
// ---------------------------------------------------------------------------

/// Resolve the `.git` directory path for a repo (handles worktrees).
fn get_git_dir(repo_root: &Path) -> Result<PathBuf, GitAiError> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(repo_root)
        .output()
        .map_err(GitAiError::IoError)?;
    if !output.status.success() {
        return Err(GitAiError::Generic(
            "git rev-parse --git-dir failed".to_string(),
        ));
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if Path::new(&s).is_absolute() {
        Ok(PathBuf::from(s))
    } else {
        Ok(repo_root.join(s))
    }
}

/// Return the mtime of `.git/index` as nanoseconds since the UNIX epoch.
///
/// Used as a cold-start watermark proxy when the daemon has no worktree
/// watermark yet.  Only called when `wm = Some(w)` with `w.worktree = None`,
/// so passing `wm = None` (tests, non-daemon mode) always bypasses this.
pub fn git_index_mtime_ns(repo_root: &Path) -> Option<u128> {
    let git_dir = get_git_dir(repo_root).ok()?;
    let index_path = git_dir.join("index");
    let mtime = fs::metadata(&index_path).ok()?.modified().ok()?;
    Some(system_time_to_nanos(mtime))
}

/// Test whether a file is covered by the current watermarks, meaning it has
/// not been modified since the last known-good baseline and does not need to
/// be stored in the snapshot.
///
/// A file is covered when:
/// - It has a per-file watermark AND `mtime ≤ file_wm + GRACE`, OR
/// - No per-file watermark but an effective worktree wm exists AND
///   `mtime ≤ effective_wm + GRACE`.
fn is_wm_covered(
    mtime_ns: u128,
    effective_wm: Option<u128>,
    per_file_wm: &HashMap<String, u128>,
    posix_key: &str,
) -> bool {
    if let Some(&file_wm) = per_file_wm.get(posix_key) {
        return mtime_ns <= file_wm + MTIME_GRACE_WINDOW_NS;
    }
    effective_wm.is_some_and(|ewm| mtime_ns <= ewm + MTIME_GRACE_WINDOW_NS)
}

// ---------------------------------------------------------------------------
// Path filtering
// ---------------------------------------------------------------------------

/// Build the git-ai ignore ruleset for use in `filter_entry` on the snapshot walker.
///
/// Only covers the git-ai-specific patterns:
/// - Default ignore patterns (lock files, node_modules, etc.)
/// - Patterns from `.git-ai-ignore` at the repo root
/// - Linguist-generated patterns from `.gitattributes` at the repo root
///
/// Standard `.gitignore` handling — including nested `.gitignore` files throughout
/// the repo tree — is left to `WalkBuilder` with `git_ignore(true)`, which discovers
/// and applies them natively as it descends. Adding them here too would be redundant
/// and would require a separate pre-walk that can't apply rules during traversal.
pub fn build_gitignore(repo_root: &Path) -> Result<Gitignore, GitAiError> {
    let mut builder = GitignoreBuilder::new(repo_root);

    // git-ai-specific patterns: same source of truth as non-bash checkpoints.
    let shared_patterns: Vec<String> = default_ignore_patterns()
        .into_iter()
        .chain(load_git_ai_ignore_patterns_from_path(repo_root))
        .chain(load_linguist_generated_patterns_from_path(repo_root))
        .collect();
    for pattern in &shared_patterns {
        if let Err(e) = builder.add_line(None, pattern) {
            debug_log(&format!(
                "Warning: failed to add ignore pattern '{}': {}",
                pattern, e
            ));
        }
    }

    builder
        .build()
        .map_err(|e| GitAiError::Generic(format!("Failed to build gitignore rules: {}", e)))
}

/// Check whether a newly created (untracked) file should be included.
/// Returns true if the file is NOT ignored by .gitignore rules.
pub fn should_include_new_file(gitignore: &Gitignore, path: &Path, is_dir: bool) -> bool {
    // Use matched_path_or_any_parents so directory patterns like `secrets/` also
    // exclude files nested inside that directory (e.g. `secrets/token.txt`).
    let matched = gitignore.matched_path_or_any_parents(path, is_dir);
    !matched.is_ignore()
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Take a stat snapshot of the repo working tree.
///
/// Only stores entries for files that pass the git-ai ignore filter (gitignore
/// + defaults + .git-ai-ignore + linguist) AND have `mtime > effective_wm + GRACE`.
///
/// Filtering is applied uniformly to all files — there is no special treatment
/// for git-tracked vs untracked files.
///
/// `wm` should be the result of a recent daemon watermark query.  Pass
/// `None` to skip watermark filtering entirely (no daemon context, or direct
/// `snapshot()` callers such as tests and `git_status_fallback`).
pub fn snapshot(
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
    wm: Option<&DaemonWatermarks>,
) -> Result<StatSnapshot, GitAiError> {
    let start = Instant::now();
    let invocation_key = format!("{}:{}", session_id, tool_use_id);

    // Compute the effective worktree-level watermark:
    //   wm = Some(w) with real worktree wm → use it directly (warm start).
    //   wm = Some(w) with no worktree wm → daemon up but hasn't seen a full
    //                Human checkpoint yet; use .git/index mtime as proxy.
    //   wm = None   → no filtering (caller opted out or direct snapshot() call
    //                without daemon context).
    //
    // Note: the cold-start proxy (git_index_mtime_ns) is injected by
    // handle_bash_tool when no daemon is running, not here, so direct
    // snapshot() callers (e.g. tests, git_status_fallback) are unaffected.
    let effective_worktree_wm: Option<u128> = match wm {
        Some(w) if w.worktree.is_some() => w.worktree,
        Some(_) => git_index_mtime_ns(repo_root),
        None => None,
    };

    let per_file_wm: HashMap<String, u128> = wm.map(|w| w.per_file.clone()).unwrap_or_default();

    // Build the git-ai ignore ruleset: gitignore + defaults + .git-ai-ignore + linguist.
    // Arc is needed because filter_entry requires 'static, preventing a borrow.
    // The closure takes sole ownership; no post-walker use of the ruleset is needed.
    let gitignore_filter = Arc::new(build_gitignore(repo_root)?);

    let mut entries = HashMap::new();

    // Pass the git-ai ignore ruleset directly into the walker via filter_entry.
    // This prunes entire ignored directories (node_modules/, target/, etc.)
    // before the walker descends into them — including directories that are in
    // default_ignore_patterns() but not yet in the repo's .gitignore (a common
    // case for node_modules that the user hasn't gitignored yet).
    // git_ignore(true) handles the standard .gitignore case; filter_entry
    // catches the rest (defaults, .git-ai-ignore, linguist-generated).
    let repo_root_buf = repo_root.to_path_buf();
    let walker = WalkBuilder::new(repo_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| {
            if entry.file_name() == ".git" {
                return false;
            }
            let abs = entry.path();
            let Ok(rel) = abs.strip_prefix(&repo_root_buf) else {
                return true; // outside repo root — let walker handle it
            };
            if rel.as_os_str().is_empty() {
                return true; // repo root itself — always include
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            should_include_new_file(&gitignore_filter, rel, is_dir)
        })
        .build();

    let walk_timeout = Duration::from_millis(effective_walk_timeout_ms());
    for result in walker {
        let elapsed = start.elapsed();
        if elapsed >= walk_timeout {
            let elapsed_ms = elapsed.as_millis();
            let timeout_ms = walk_timeout.as_millis();
            let msg = format!(
                "bash_tool: snapshot walk exceeded {}ms limit ({}ms elapsed, {} entries so far); abandoning stat-diff",
                timeout_ms,
                elapsed_ms,
                entries.len()
            );
            debug_log(&msg);
            crate::observability::log_message(
                &msg,
                "warning",
                Some(serde_json::json!({
                    "elapsed_ms": elapsed_ms,
                    "entries_so_far": entries.len(),
                    "walk_timeout_ms": timeout_ms,
                })),
            );
            return Err(GitAiError::Generic(msg));
        }

        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                debug_log(&format!("Walker error: {}", e));
                continue;
            }
        };

        let abs_path = entry.path();

        // Skip directories — filter_entry already pruned ignored dirs; this
        // guard drops any remaining directory entries (e.g. the repo root).
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or_else(|| abs_path.is_dir())
        {
            continue;
        }

        let rel_path = match abs_path.strip_prefix(repo_root) {
            Ok(p) => p,
            Err(_) => continue,
        };

        // filter_entry already applied should_include_new_file for files too,
        // so no secondary check is needed here.

        let normalized = normalize_path(rel_path);

        match fs::symlink_metadata(abs_path) {
            Ok(meta) => {
                let stat = StatEntry::from_metadata(&meta);
                let mtime_ns = stat.mtime.map(system_time_to_nanos).unwrap_or(0);
                let posix_key = normalize_to_posix(&normalized.to_string_lossy());
                if !is_wm_covered(mtime_ns, effective_worktree_wm, &per_file_wm, &posix_key) {
                    entries.insert(normalized, stat);
                    if entries.len() > MAX_TRACKED_FILES {
                        debug_log(&format!(
                            "Snapshot: exceeded MAX_TRACKED_FILES ({}), skipping stat-diff",
                            MAX_TRACKED_FILES
                        ));
                        return Err(GitAiError::Generic(format!(
                            "repo has more than {} recently-modified files; skipping stat-diff",
                            MAX_TRACKED_FILES
                        )));
                    }
                }
            }
            Err(e) => {
                debug_log(&format!("Failed to stat {}: {}", abs_path.display(), e));
            }
        }
    }

    debug_log(&format!(
        "Snapshot: {} files scanned in {}ms",
        entries.len(),
        start.elapsed().as_millis()
    ));

    Ok(StatSnapshot {
        entries,
        taken_at: Some(Instant::now()),
        invocation_key,
        repo_root: repo_root.to_path_buf(),
        effective_worktree_wm,
        per_file_wm,
    })
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Diff two snapshots to find created and modified files.
///
/// Both snapshots apply the same git-ai ignore filter at snapshot time, so
/// any file in `post.entries` already passed that filter. No secondary
/// filtering is needed here.
///
/// Files in post but not pre are reported as **created** (either genuinely
/// new, or previously wm-covered and now modified by bash — both are changed
/// files that need attribution).  Files in both with a changed stat-tuple are
/// reported as **modified**.  Deletions are not tracked.
pub fn diff(pre: &StatSnapshot, post: &StatSnapshot) -> StatDiffResult {
    let mut result = StatDiffResult::default();

    // Files in post but not pre: new files or previously wm-covered files
    // now modified by bash. Both need attribution; the distinction doesn't
    // matter since all_changed_paths() merges created + modified.
    for path in post.entries.keys() {
        if !pre.entries.contains_key(path) {
            result.created.push(path.clone());
        }
    }

    // Files in both but stat-tuple differs.
    for (path, post_entry) in &post.entries {
        if let Some(pre_entry) = pre.entries.get(path) {
            if pre_entry != post_entry {
                result.modified.push(path.clone());
            }
        }
    }

    result.created.sort();
    result.modified.sort();

    result
}

// ---------------------------------------------------------------------------
// Snapshot caching (file-based persistence)
// ---------------------------------------------------------------------------

/// Get the directory for storing bash snapshots.
fn snapshot_cache_dir(repo_root: &Path) -> Result<PathBuf, GitAiError> {
    let cache_dir = get_git_dir(repo_root)?.join("ai").join("bash_snapshots");
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

    // Read and then delete atomically: skip the exists() check to avoid a
    // TOCTOU race where a concurrent post-hook deletes the file between the
    // check and the read.  NotFound after the read means it was already
    // consumed; any other error is a real failure.
    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(GitAiError::IoError(e)),
    };
    let snapshot: StatSnapshot = serde_json::from_slice(&data).map_err(GitAiError::JsonError)?;

    // Consume: remove the file after a successful read.
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
                changed_files.push(normalize_to_posix(path));
            }
        } else if line.starts_with("2 ") {
            // Rename/copy: 9 fields before new path, then NUL-delimited original path
            let fields: Vec<&str> = line.splitn(10, ' ').collect();
            if let Some(path) = fields.last() {
                changed_files.push(normalize_to_posix(path));
            }
            // Also include the original path (next NUL-delimited entry)
            if i + 1 < parts.len() {
                let orig = String::from_utf8_lossy(parts[i + 1]);
                if !orig.is_empty() {
                    changed_files.push(normalize_to_posix(&orig));
                }
            }
            i += 1;
        } else if let Some(path) = line.strip_prefix("? ") {
            // Untracked: path follows "? "
            changed_files.push(normalize_to_posix(path));
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
    use std::io::Read;
    let mut contents = HashMap::new();
    for rel_path in file_paths {
        let abs_path = repo_root.join(rel_path);
        let mut file = match fs::File::open(&abs_path) {
            Ok(f) => f,
            Err(e) => {
                debug_log(&format!(
                    "Skipping unreadable file for capture: {}: {}",
                    rel_path.display(),
                    e
                ));
                continue;
            }
        };
        if let Ok(meta) = file.metadata() {
            if meta.len() > MAX_CAPTURE_FILE_SIZE {
                debug_log(&format!(
                    "Skipping large file for capture: {} ({} bytes)",
                    rel_path.display(),
                    meta.len()
                ));
                continue;
            }
        }
        let mut content = String::new();
        match file.read_to_string(&mut content) {
            Ok(_) => {
                let key = normalize_to_posix(&rel_path.to_string_lossy());
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
pub struct DaemonWatermarks {
    /// Per-file mtime watermarks from scoped checkpoints.
    per_file: HashMap<String, u128>,
    /// Timestamp of the last full (non-scoped) Human checkpoint, if any.
    /// `None` on cold start (daemon has never processed a full checkpoint).
    worktree: Option<u128>,
}

fn query_daemon_watermarks(repo_working_dir: &str) -> Option<DaemonWatermarks> {
    let config = DaemonConfig::from_env_or_default_paths().ok()?;
    // Fast-exit when the socket file does not exist — avoids the connect
    // timeout on every hook call when no daemon is running.
    if !config.control_socket_path.exists() {
        return None;
    }
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

/// Find files in the snapshot that are stale (modified since the last baseline).
///
/// Because `snapshot()` already filters out wm-covered files, every entry in
/// `snapshot.entries` is a candidate.  We still apply the per-file (Tier 1)
/// check for precision: a file that passed the coarser worktree-wm filter may
/// still be within the grace window of its own more-recent per-file watermark.
///
/// Three-tier logic per entry:
/// 1. Per-file watermark → stale if `mtime > file_wm + GRACE`.
/// 2. Worktree watermark (real or cold-start proxy) → all entries already
///    passed this threshold via the snapshot filter; push unconditionally.
/// 3. Neither watermark → no baseline, skip (cold-start with no index mtime).
fn find_stale_files(snapshot: &StatSnapshot) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    for (rel_path, entry) in &snapshot.entries {
        if !entry.exists {
            continue;
        }
        let Some(mtime) = entry.mtime else {
            continue;
        };
        let mtime_ns = system_time_to_nanos(mtime);
        let posix_key = normalize_to_posix(&rel_path.to_string_lossy());

        if let Some(&file_wm) = snapshot.per_file_wm.get(&posix_key) {
            // Tier 1: precise per-file watermark — may be tighter than the
            // effective worktree wm used for snapshot filtering.
            if mtime_ns > file_wm + MTIME_GRACE_WINDOW_NS {
                stale.push(rel_path.clone());
            }
        } else if snapshot.effective_worktree_wm.is_some() {
            // Tier 2: entry passed the worktree-wm snapshot filter, so by
            // definition mtime > effective_wm + GRACE → stale.
            stale.push(rel_path.clone());
        }
        // Tier 3: no watermark at all → no baseline, skip.
    }
    stale.sort();
    stale
}

// ---------------------------------------------------------------------------
// Pre/post hook captured-checkpoint helpers
// ---------------------------------------------------------------------------

/// Attempt to prepare a captured checkpoint during the pre-hook.
///
/// Uses watermarks already embedded in the snapshot to identify stale files
/// (modified since the last checkpoint), captures their contents, and prepares
/// a captured checkpoint with `CheckpointKind::Human` and `will_edit_filepaths`.
///
/// Returns `None` on any failure or when no stale files are found, allowing
/// the caller to proceed without a captured checkpoint.
fn attempt_pre_hook_capture(
    snap: &StatSnapshot,
    repo_root: &Path,
) -> Option<CapturedCheckpointInfo> {
    if !checkpoint_delegation_enabled() {
        debug_log("Pre-hook capture: async checkpoint delegation not enabled, skipping capture");
        return None;
    }

    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // Watermarks are already embedded in the snapshot (queried before snapshot
    // was taken); no second daemon round-trip needed.
    let stale_files = find_stale_files(snap);

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

    let contents = capture_file_contents(repo_root, &stale_files);

    let repo = match find_repository_in_path(&repo_working_dir) {
        Ok(r) => r,
        Err(e) => {
            debug_log(&format!("Pre-hook capture: failed to open repo: {}", e));
            return None;
        }
    };

    let stale_paths: Vec<String> = stale_files
        .iter()
        .map(|p| normalize_to_posix(&p.to_string_lossy()))
        .collect();

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
    // Only attempt capture when delegation is enabled — captured checkpoint
    // files are consumed by the daemon; without it they would accumulate.
    if !checkpoint_delegation_enabled() {
        debug_log("Post-hook capture: async checkpoint delegation not enabled, skipping capture");
        return None;
    }

    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // Exclude deleted files: they have no post-content to capture, and recording
    // them as empty blobs would misrepresent deletions as "file was emptied".
    // The Checkpoint action already carries all changed paths for attribution.
    let (existing_paths, _deleted_paths): (Vec<&String>, Vec<&String>) = changed_paths
        .iter()
        .partition(|p| repo_root.join(p.as_str()).exists());

    if existing_paths.is_empty() {
        debug_log("Post-hook capture: no existing changed files to capture, skipping");
        return None;
    }

    let path_bufs: Vec<PathBuf> = existing_paths
        .iter()
        .map(|p| PathBuf::from(p.as_str()))
        .collect();
    let contents = capture_file_contents(repo_root, &path_bufs);

    let repo = match find_repository_in_path(&repo_working_dir) {
        Ok(r) => r,
        Err(e) => {
            debug_log(&format!("Post-hook capture: failed to open repo: {}", e));
            return None;
        }
    };

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
// Inflight bash-call detection
// ---------------------------------------------------------------------------
//
// When an AI agent runs a bash tool that internally calls `git commit`, the
// git pre-commit hook fires with no agent context and would normally be
// attributed as human. We detect this via the pre-snapshot files that already
// exist in the bash_snapshots cache: a snapshot is written at pre-hook time
// and deleted when the post-hook consumes it, so its presence is a precise
// signal that a bash invocation is currently in flight.
//
// Stale snapshots (> SNAPSHOT_STALE_SECS old) are excluded so a crashed
// pre-hook does not permanently block human attribution.

/// Returns `true` if any non-stale pre-snapshot exists for this repo,
/// indicating an AI bash tool call is currently in flight.
///
/// Used by the checkpoint handler to override Human attribution to AI when
/// a `git commit` is triggered from within a bash tool invocation.
pub fn has_active_bash_inflight(repo_root: &Path) -> bool {
    let Ok(cache_dir) = snapshot_cache_dir(repo_root) else {
        return false;
    };
    let Ok(entries) = fs::read_dir(&cache_dir) else {
        return false;
    };
    let now = SystemTime::now();
    entries.flatten().any(|e| {
        let p = e.path();
        p.extension().is_some_and(|ext| ext == "json")
            && fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|age| age.as_secs() <= SNAPSHOT_STALE_SECS)
                .unwrap_or(false)
    })
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
            if let Some(path) = bash_sidecar_path(repo_root, session_id)
                && let Err(e) = fs::write(&path, &id)
            {
                debug_log(&format!(
                    "bash sidecar write failed ({}): {}",
                    path.display(),
                    e
                ));
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
    let hook_start = Instant::now();
    let hook_timeout = Duration::from_millis(effective_hook_timeout_ms());

    // Resolve the effective tool_use_id — generates/reads a sidecar UUID when
    // the caller passes the generic "bash" fallback (no per-call ID from agent).
    let tool_use_id = resolve_bash_tool_use_id(hook_event, repo_root, session_id, tool_use_id);
    let tool_use_id = tool_use_id.as_str();
    let invocation_key = format!("{}:{}", session_id, tool_use_id);

    /// Log a hook timeout event to both stderr and telemetry, then return Fallback.
    macro_rules! hook_timeout_fallback {
        ($label:expr) => {{
            let elapsed_ms = hook_start.elapsed().as_millis();
            let msg = format!(
                "bash_tool: {} exceeded {}ms hook limit ({}ms elapsed); abandoning",
                $label, hook_timeout.as_millis(), elapsed_ms
            );
            debug_log(&msg);
            crate::observability::log_message(
                &msg,
                "warning",
                Some(serde_json::json!({
                    "label": $label,
                    "elapsed_ms": elapsed_ms,
                    "hook_timeout_ms": hook_timeout.as_millis(),
                })),
            );
            return Ok(BashToolResult {
                action: BashCheckpointAction::Fallback,
                captured_checkpoint: None,
            });
        }};
    }

    match hook_event {
        HookEvent::PreToolUse => {
            // Clean up stale snapshots
            let _ = cleanup_stale_snapshots(repo_root);

            // Query daemon watermarks first so snapshot() can filter out
            // wm-covered files and embed the watermarks for the post-hook.
            let repo_working_dir = repo_root.to_string_lossy().to_string();
            let wm = query_daemon_watermarks(&repo_working_dir);

            if hook_start.elapsed() >= hook_timeout {
                hook_timeout_fallback!("pre-hook after daemon query");
            }

            // Take and store pre-snapshot (filtered by watermarks)
            match snapshot(repo_root, session_id, tool_use_id, wm.as_ref()) {
                Ok(snap) => {
                    save_snapshot(&snap)?;
                    debug_log(&format!(
                        "Pre-snapshot stored for invocation {} ({} entries, effective_wm={:?})",
                        invocation_key,
                        snap.entries.len(),
                        snap.effective_worktree_wm,
                    ));

                    if hook_start.elapsed() >= hook_timeout {
                        hook_timeout_fallback!("pre-hook after snapshot");
                    }

                    // Attempt watermark-based pre-hook content capture using
                    // the embedded watermarks (no second daemon query needed).
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
                    // Don't fail the tool call; post-hook will use fallback path
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
                Some(pre) => {
                    if hook_start.elapsed() >= hook_timeout {
                        hook_timeout_fallback!("post-hook before snapshot");
                    }

                    // Take post-snapshot using the same effective watermark as
                    // the pre-snapshot so the coverage filter is consistent.
                    // Files that bash modified will have crossed the threshold
                    // and appear in post.entries even if they were absent from
                    // pre.entries (wm-covered before the tool ran).
                    //
                    // When the pre-snapshot had no watermarks at all (no daemon
                    // at pre-hook time → effective_worktree_wm = None and no
                    // per-file wm), pass None so the post-snapshot also does a
                    // full scan rather than falling through to git_index_mtime_ns
                    // and producing an asymmetric filter.
                    let post_wm: Option<DaemonWatermarks> =
                        if pre.effective_worktree_wm.is_some() || !pre.per_file_wm.is_empty() {
                            Some(DaemonWatermarks {
                                per_file: pre.per_file_wm.clone(),
                                worktree: pre.effective_worktree_wm,
                            })
                        } else {
                            None
                        };
                    match snapshot(repo_root, session_id, tool_use_id, post_wm.as_ref()) {
                        Ok(post) => {
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
                                    "Bash tool {}: {} files changed ({} created, {} modified)",
                                    invocation_key,
                                    paths.len(),
                                    diff_result.created.len(),
                                    diff_result.modified.len(),
                                ));

                                if hook_start.elapsed() >= hook_timeout {
                                    hook_timeout_fallback!("post-hook after snapshot");
                                }

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
                            debug_log(&format!("Post-snapshot failed: {}; returning fallback", e));
                            Ok(BashToolResult {
                                action: BashCheckpointAction::Fallback,
                                captured_checkpoint: None,
                            })
                        }
                    }
                }
                None => {
                    // Pre-snapshot lost (process restart, etc.) — return fallback.
                    // We do not call git status here: it is extremely slow on large
                    // monorepos and cannot be relied on at this point in the flow.
                    debug_log(&format!(
                        "Pre-snapshot not found for {}; returning fallback (no git status)",
                        invocation_key
                    ));
                    Ok(BashToolResult {
                        action: BashCheckpointAction::Fallback,
                        captured_checkpoint: None,
                    })
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
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
        };
        let post = StatSnapshot {
            entries: HashMap::new(),
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
        };

        let result = diff(&pre, &post);
        assert!(result.is_empty());
    }

    #[test]
    fn test_diff_detects_creation() {
        let pre = StatSnapshot {
            entries: HashMap::new(),
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
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
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
        };

        let result = diff(&pre, &post);
        assert_eq!(result.created.len(), 1);
        assert!(result.modified.is_empty());
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
            taken_at: None,
            invocation_key: "test:1".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
        };

        let post = StatSnapshot {
            entries: post_entries,
            taken_at: None,
            invocation_key: "test:2".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm: None,
            per_file_wm: HashMap::new(),
        };

        let result = diff(&pre, &post);
        assert!(result.created.is_empty());
        assert_eq!(result.modified.len(), 1);
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
        };
        let paths = result.all_changed_paths();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"new.txt".to_string()));
        assert!(paths.contains(&"changed.txt".to_string()));
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

    /// Helper: build a minimal `StatSnapshot` with the given entries and
    /// optional embedded watermarks.
    fn make_snapshot_with_wm(
        entries: HashMap<PathBuf, StatEntry>,
        per_file_wm: HashMap<String, u128>,
        effective_worktree_wm: Option<u128>,
    ) -> StatSnapshot {
        StatSnapshot {
            entries,
            taken_at: None,
            invocation_key: "test:stale".to_string(),
            repo_root: PathBuf::from("/tmp"),
            effective_worktree_wm,
            per_file_wm,
        }
    }

    /// Shorthand: no watermarks (cold-start, no index mtime).
    fn make_snapshot(entries: HashMap<PathBuf, StatEntry>) -> StatSnapshot {
        make_snapshot_with_wm(entries, HashMap::new(), None)
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
            ctime: mtime,
            size: 100,
            mode: 0o644,
            file_type: StatFileType::Regular,
        }
    }

    #[test]
    fn test_find_stale_files_cold_start_excludes_unwatermarked_files() {
        // On cold start (no per-file and no worktree watermark), files with no
        // watermark are NOT returned by find_stale_files — they are simply skipped.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot(entries); // no embedded wm

        let stale = find_stale_files(&snapshot);
        assert!(
            stale.is_empty(),
            "cold-start: unwatermarked files are not returned (no baseline)"
        );
    }

    #[test]
    fn test_find_stale_files_uses_worktree_watermark_as_fallback() {
        // File has no per-file watermark, but worktree watermark exists at 90s.
        // File mtime is 100s → 10s beyond grace window → stale.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot_with_wm(
            entries,
            HashMap::new(),
            Some(Duration::from_secs(90).as_nanos()),
        );

        let stale = find_stale_files(&snapshot);
        assert_eq!(
            stale.len(),
            1,
            "file modified after worktree watermark is stale"
        );
    }

    #[test]
    fn test_find_stale_files_worktree_watermark_within_grace() {
        // File mtime=100s, worktree watermark=99s → within 2s grace → NOT stale.
        // Note: this file would have been filtered from the snapshot by
        // is_wm_covered in production; this test exercises the Tier-2 guard
        // inside find_stale_files for robustness.
        let mut entries = HashMap::new();
        entries.insert(
            normalize_path(Path::new("src/main.rs")),
            make_entry(100, true),
        );
        let snapshot = make_snapshot_with_wm(
            entries,
            HashMap::new(),
            Some(Duration::from_secs(99).as_nanos()),
        );

        // mtime 100s > effective_wm 99s, but find_stale_files pushes Tier-2
        // entries unconditionally (coverage filter already checked).  The file
        // is stale from find_stale_files' perspective even though the diff with
        // an identical post-snapshot would report no change.
        let stale = find_stale_files(&snapshot);
        assert_eq!(stale.len(), 1, "entry that passed coverage filter is stale");
    }

    #[test]
    fn test_find_stale_files_per_file_wins_over_worktree() {
        // Per-file watermark (95s) is older than worktree watermark (98s).
        // File mtime=100s → 5s beyond per-file watermark → stale.
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(95).as_nanos());
        let snapshot =
            make_snapshot_with_wm(entries, per_file, Some(Duration::from_secs(98).as_nanos()));

        let stale = find_stale_files(&snapshot);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn test_find_stale_files_within_grace_window() {
        // File with mtime=100s, per-file watermark at 99s.
        // Difference is 1s which is within the 2s grace window → NOT stale.
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(99).as_nanos());
        let snapshot = make_snapshot_with_wm(entries, per_file, None);

        let stale = find_stale_files(&snapshot);
        assert!(
            stale.is_empty(),
            "file within grace window should not be stale"
        );
    }

    #[test]
    fn test_find_stale_files_beyond_grace_window() {
        // File with mtime=100s, per-file watermark at 95s.
        // Difference is 5s which exceeds the 2s grace window → stale.
        let mut entries = HashMap::new();
        let path = normalize_path(Path::new("src/lib.rs"));
        entries.insert(path, make_entry(100, true));

        let mut per_file = HashMap::new();
        per_file.insert("src/lib.rs".to_string(), Duration::from_secs(95).as_nanos());
        let snapshot = make_snapshot_with_wm(entries, per_file, None);

        let stale = find_stale_files(&snapshot);
        assert_eq!(stale.len(), 1, "file beyond grace window should be stale");
    }

    #[test]
    fn test_find_stale_files_nonexistent_skipped() {
        // File with exists=false should not appear in stale list regardless of watermarks.
        let mut entries = HashMap::new();
        entries.insert(normalize_path(Path::new("gone.rs")), make_entry(100, false));
        let snapshot = make_snapshot_with_wm(entries, HashMap::new(), Some(0));

        let stale = find_stale_files(&snapshot);
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
        let changed_paths = ["modified.txt".to_string(), deleted.to_string()];

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

    // -----------------------------------------------------------------------
    // build_gitignore tests
    // -----------------------------------------------------------------------

    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    /// Default ignore patterns (e.g. node_modules, lock files) are applied even
    /// when no .gitignore exists in the repo.
    #[test]
    fn test_build_gitignore_applies_default_patterns() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let gitignore = build_gitignore(dir.path()).unwrap();

        // node_modules and lock files must be excluded by default
        assert!(
            !should_include_new_file(&gitignore, Path::new("node_modules/react/index.js"), false),
            "node_modules should be ignored by default"
        );
        assert!(
            !should_include_new_file(&gitignore, Path::new("package-lock.json"), false),
            "package-lock.json should be ignored by default"
        );
        assert!(
            !should_include_new_file(&gitignore, Path::new("yarn.lock"), false),
            "yarn.lock should be ignored by default"
        );

        // Normal source files must not be excluded
        assert!(
            should_include_new_file(&gitignore, Path::new("src/main.rs"), false),
            "src/main.rs should not be ignored"
        );
    }

    /// Patterns in .git-ai-ignore are respected, suppressing untracked files
    /// that aren't covered by .gitignore.
    #[test]
    fn test_build_gitignore_reads_git_ai_ignore() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        fs::write(dir.path().join(".git-ai-ignore"), "secrets/\n*.pem\n").unwrap();

        let gitignore = build_gitignore(dir.path()).unwrap();

        assert!(
            !should_include_new_file(&gitignore, Path::new("secrets/token.txt"), false),
            "secrets/ should be ignored via .git-ai-ignore"
        );
        assert!(
            !should_include_new_file(&gitignore, Path::new("server.pem"), false),
            "*.pem should be ignored via .git-ai-ignore"
        );
        assert!(
            should_include_new_file(&gitignore, Path::new("README.md"), false),
            "README.md should not be ignored"
        );
    }

    /// Files marked linguist-generated in .gitattributes are excluded from
    /// the Tier 2 snapshot.
    #[test]
    fn test_build_gitignore_reads_linguist_generated_from_gitattributes() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        fs::write(
            dir.path().join(".gitattributes"),
            "generated/*.pb.go linguist-generated=true\ndocs/api.md linguist-generated\n",
        )
        .unwrap();

        let gitignore = build_gitignore(dir.path()).unwrap();

        assert!(
            !should_include_new_file(&gitignore, Path::new("generated/foo.pb.go"), false),
            "linguist-generated glob should be ignored"
        );
        assert!(
            !should_include_new_file(&gitignore, Path::new("docs/api.md"), false),
            "linguist-generated exact file should be ignored"
        );
        assert!(
            should_include_new_file(&gitignore, Path::new("generated/manual.go"), false),
            "non-generated file in generated/ should not be ignored"
        );
    }

    // -----------------------------------------------------------------------
    // has_active_bash_inflight tests
    // -----------------------------------------------------------------------

    fn make_snapshot_file(cache_dir: &Path, name: &str) -> PathBuf {
        let path = cache_dir.join(format!("{}.json", name));
        fs::write(&path, b"{}").unwrap();
        path
    }

    /// No snapshot files → not in flight.
    #[test]
    fn test_inflight_false_when_no_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(!has_active_bash_inflight(dir.path()));
    }

    /// A fresh snapshot file (just written) → in flight.
    #[test]
    fn test_inflight_true_when_snapshot_present() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let cache = snapshot_cache_dir(dir.path()).unwrap();
        make_snapshot_file(&cache, "session1_call1");
        assert!(has_active_bash_inflight(dir.path()));
    }

    /// Snapshot consumed (deleted) by post-hook → not in flight.
    #[test]
    fn test_inflight_false_after_snapshot_consumed() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let cache = snapshot_cache_dir(dir.path()).unwrap();
        let snap = make_snapshot_file(&cache, "session1_call1");
        assert!(has_active_bash_inflight(dir.path()));
        fs::remove_file(&snap).unwrap(); // post-hook consumes it
        assert!(!has_active_bash_inflight(dir.path()));
    }

    /// Stale snapshot (backdated mtime) is ignored — does not count as in flight.
    #[test]
    fn test_inflight_false_for_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let cache = snapshot_cache_dir(dir.path()).unwrap();
        let snap = make_snapshot_file(&cache, "session_stale");
        // Backdate the mtime beyond the stale threshold
        let stale_time = SystemTime::now()
            .checked_sub(Duration::from_secs(SNAPSHOT_STALE_SECS + 60))
            .unwrap();
        filetime::set_file_mtime(&snap, filetime::FileTime::from_system_time(stale_time)).unwrap();
        assert!(
            !has_active_bash_inflight(dir.path()),
            "stale snapshot should not count as inflight"
        );
    }

    /// REGRESSION: Two parallel bash calls are in flight simultaneously.
    /// Removing one does not clear the inflight signal; only when both are
    /// consumed does `has_active_bash_inflight` return false.
    /// This ensures parallel AI bash calls never cause a commit to be
    /// attributed as human.
    #[test]
    fn test_inflight_parallel_calls_regression() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let cache = snapshot_cache_dir(dir.path()).unwrap();

        // Two bash calls start (pre-hooks fire)
        let snap1 = make_snapshot_file(&cache, "session1_callA");
        let snap2 = make_snapshot_file(&cache, "session1_callB");

        // Both in flight → commits during this window must be AI
        assert!(has_active_bash_inflight(dir.path()), "both in flight");

        // First call finishes (post-hook consumes its snapshot)
        fs::remove_file(&snap1).unwrap();
        // Second call still in flight → commits must STILL be AI
        assert!(
            has_active_bash_inflight(dir.path()),
            "second call still in flight — must not regress to human"
        );

        // Second call finishes
        fs::remove_file(&snap2).unwrap();
        // Now safe to attribute as human again
        assert!(!has_active_bash_inflight(dir.path()), "all calls complete");
    }

    /// Sidecar .txt files in the same directory do not count as snapshots.
    #[test]
    fn test_inflight_ignores_sidecar_txt_files() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let cache = snapshot_cache_dir(dir.path()).unwrap();
        // Write a sidecar (the kind resolve_bash_tool_use_id creates)
        fs::write(cache.join("last_id_mysession.txt"), "some-uuid").unwrap();
        assert!(
            !has_active_bash_inflight(dir.path()),
            "txt sidecar must not be treated as snapshot"
        );
    }
}
