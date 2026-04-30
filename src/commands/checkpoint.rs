use crate::authorship::attribution_tracker::{
    Attribution, AttributionTracker, INITIAL_ATTRIBUTION_TS, LineAttribution,
};
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::{
    generate_session_id, generate_short_hash, generate_trace_id,
};
use crate::authorship::ignore::{
    IgnoreMatcher, build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::imara_diff_utils::{
    LineChangeTag, compute_line_changes, normalize_line_endings,
};
use crate::authorship::working_log::CheckpointKind;
use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};
use crate::commands::blame::{GitAiBlameOptions, OLDEST_AI_BLAME_DATE};
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::repo_storage::PersistedWorkingLog;
use crate::git::repository::Repository;
use crate::git::status::{EntryKind, StatusCode};
use crate::utils::normalize_to_posix;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

/// Per-file line statistics (in-memory only, not persisted)
#[derive(Debug, Clone, Default)]
#[doc(hidden)]
pub struct FileLineStats {
    pub additions: u32,
    pub deletions: u32,
    pub additions_sloc: u32,
    pub deletions_sloc: u32,
}

/// Latest checkpoint state needed to process a file in the next checkpoint.
#[derive(Debug, Clone)]
struct PreviousFileState {
    blob_sha: String,
    attributions: Vec<Attribution>,
}

use crate::authorship::working_log::AgentId;

/// Emit at most one `agent_usage` metric per prompt every 2.5 minutes.
/// This is half of the server-side bucketing window.
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
const AGENT_USAGE_MIN_INTERVAL_SECS: u64 = 150;

#[cfg(not(any(test, feature = "test-support")))]
const KNOWN_HUMAN_MIN_SECS_AFTER_AI: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedPathRole {
    Edited,
    WillEdit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum PreparedCheckpointFileSource {
    DirtyFileContent { content: String },
    BlobRef { blob_name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedCheckpointFile {
    pub path: String,
    pub source: PreparedCheckpointFileSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedCheckpointManifest {
    pub repo_working_dir: String,
    pub base_commit: String,
    pub captured_at_ms: u128,
    pub kind: CheckpointKind,
    pub author: String,
    pub is_pre_commit: bool,
    pub explicit_path_role: PreparedPathRole,
    pub explicit_paths: Vec<String>,
    pub files: Vec<PreparedCheckpointFile>,
    #[serde(default)]
    pub checkpoint_request: Option<CheckpointRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCheckpointCapture {
    pub capture_id: String,
    pub repo_working_dir: String,
    pub file_count: usize,
}

#[derive(Debug, Clone)]
struct ResolvedCheckpointExecution {
    base_commit: String,
    ts: u128,
    files: Vec<String>,
    dirty_files: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum BaseOverrideResolutionPolicy {
    AllowFallback,
    RequireExplicitSnapshot,
}


/// Build EventAttributes with repo metadata.
/// Reused for both AgentUsage and Checkpoint events.
fn build_checkpoint_attrs(
    repo: &Repository,
    base_commit: &str,
    agent_id: Option<&AgentId>,
) -> crate::metrics::EventAttributes {
    // Extract session_id from agent_id if available
    let session_id = agent_id
        .as_ref()
        .map(|aid| generate_session_id(&aid.id, &aid.tool))
        .unwrap_or_default();

    let mut attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .session_id(session_id)
        .base_commit_sha(base_commit);

    // Add AI-specific attributes
    if let Some(agent_id) = agent_id {
        let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
        attrs = attrs
            .tool(&agent_id.tool)
            .model(&agent_id.model)
            .prompt_id(prompt_id)
            .external_prompt_id(&agent_id.id);
    }

    // Attach custom attributes using Config::fresh() to support runtime config updates
    attrs = attrs.custom_attributes_map(crate::config::Config::fresh().custom_attributes());

    // Add repo URL
    if let Ok(Some(remote_name)) = repo.get_default_remote()
        && let Ok(remotes) = repo.remotes_with_urls()
        && let Some((_, url)) = remotes.into_iter().find(|(n, _)| n == &remote_name)
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        attrs = attrs.repo_url(normalized);
    }

    // Add branch
    if let Ok(head_ref) = repo.head()
        && let Ok(short_branch) = head_ref.shorthand()
    {
        attrs = attrs.branch(short_branch);
    }

    attrs
}

/// Persistent local rate limit keyed by prompt ID hash.
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) fn should_emit_agent_usage(agent_id: &AgentId) -> bool {
    let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
    let now_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let Ok(db) = crate::metrics::db::MetricsDatabase::global() else {
        return true;
    };
    let Ok(mut db_lock) = db.lock() else {
        return true;
    };

    db_lock
        .should_emit_agent_usage(&prompt_id, now_ts, AGENT_USAGE_MIN_INTERVAL_SECS)
        .unwrap_or(true)
}

/// Always returns false in test mode — no metrics DB access needed.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn should_emit_agent_usage(_agent_id: &AgentId) -> bool {
    false
}

pub fn explicit_capture_target_paths(
    _kind: CheckpointKind,
    checkpoint_request: Option<&CheckpointRequest>,
) -> Option<(PreparedPathRole, Vec<String>)> {
    let result = checkpoint_request?;
    if result.file_paths.is_empty() {
        return None;
    }

    let paths: Vec<String> = result
        .file_paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    if paths.is_empty() {
        None
    } else {
        Some((result.path_role, paths))
    }
}

fn resolve_base_commit(repo: &Repository, base_commit_override: Option<&str>) -> String {
    base_commit_override
        .filter(|base| !base.trim().is_empty())
        .map(|base| base.to_string())
        .unwrap_or_else(|| match repo.head() {
            Ok(head) => match head.target() {
                Ok(oid) => oid,
                Err(_) => "initial".to_string(),
            },
            Err(_) => "initial".to_string(),
        })
}

fn async_checkpoint_internal_dir() -> Result<PathBuf, GitAiError> {
    if let Ok(home) = std::env::var("GIT_AI_DAEMON_HOME")
        && !home.trim().is_empty()
    {
        return Ok(PathBuf::from(home).join(".git-ai").join("internal"));
    }

    crate::config::internal_dir_path().ok_or_else(|| {
        GitAiError::Generic("Unable to determine ~/.git-ai/internal path".to_string())
    })
}

fn async_checkpoint_storage_dir() -> Result<PathBuf, GitAiError> {
    Ok(async_checkpoint_internal_dir()?.join("async-checkpoint-blobs"))
}

fn async_checkpoint_capture_dir(capture_id: &str) -> Result<PathBuf, GitAiError> {
    Ok(async_checkpoint_storage_dir()?.join(capture_id))
}

fn async_checkpoint_manifest_path(capture_id: &str) -> Result<PathBuf, GitAiError> {
    Ok(async_checkpoint_capture_dir(capture_id)?.join("manifest.json"))
}

#[doc(hidden)]
pub fn cleanup_failed_captured_checkpoint_prepare(
    capture_dir: &std::path::Path,
    capture_id: &str,
    error: &GitAiError,
) {
    if let Err(cleanup_error) = fs::remove_dir_all(capture_dir)
        && cleanup_error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(
            "failed cleaning up incomplete captured checkpoint {} at {} after error {}: {}",
            capture_id,
            capture_dir.display(),
            error,
            cleanup_error
        );
    }
}

fn new_async_checkpoint_capture_id() -> String {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("capture-{}-{}", std::process::id(), now_ns)
}

pub fn delete_captured_checkpoint(capture_id: &str) -> Result<(), GitAiError> {
    let capture_dir = async_checkpoint_capture_dir(capture_id)?;
    if capture_dir.exists() {
        fs::remove_dir_all(capture_dir)?;
    }
    Ok(())
}

pub fn prune_stale_captured_checkpoints(max_age: StdDuration) -> Result<(), GitAiError> {
    let storage_dir = match async_checkpoint_storage_dir() {
        Ok(path) => path,
        Err(_) => return Ok(()),
    };
    if !storage_dir.exists() {
        return Ok(());
    }

    let cutoff = SystemTime::now()
        .checked_sub(max_age)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in fs::read_dir(&storage_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified <= cutoff {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    is_pre_commit: bool,
) -> Result<(usize, usize, usize), GitAiError> {
    run_with_base_commit_override(
        repo,
        author,
        kind,
        quiet,
        checkpoint_request,
        is_pre_commit,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_with_base_commit_override(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
) -> Result<(usize, usize, usize), GitAiError> {
    run_with_base_commit_override_with_policy(
        repo,
        author,
        kind,
        quiet,
        checkpoint_request,
        is_pre_commit,
        base_commit_override,
        BaseOverrideResolutionPolicy::AllowFallback,
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn run_with_base_commit_override_with_policy(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
    base_override_resolution_policy: BaseOverrideResolutionPolicy,
) -> Result<(usize, usize, usize), GitAiError> {
    let checkpoint_start = Instant::now();
    tracing::debug!("[BENCHMARK] Starting checkpoint run");
    let resolved = resolve_live_checkpoint_execution(
        repo,
        kind,
        checkpoint_request.as_ref(),
        is_pre_commit,
        base_commit_override,
        base_override_resolution_policy,
    )?;
    let Some(resolved) = resolved else {
        tracing::debug!(
            "[BENCHMARK] Total checkpoint run took {:?}",
            checkpoint_start.elapsed()
        );
        return Ok((0, 0, 0));
    };

    execute_resolved_checkpoint(
        repo,
        author,
        kind,
        quiet,
        checkpoint_request,
        is_pre_commit,
        resolved,
        checkpoint_start,
    )
}

fn filtered_pathspecs_for_checkpoint_request(
    repo: &Repository,
    kind: CheckpointKind,
    checkpoint_request: Option<&CheckpointRequest>,
) -> Option<Vec<String>> {
    let (_, paths) = explicit_capture_target_paths(kind, checkpoint_request)?;
    let repo_workdir = repo.workdir().ok()?;

    let filtered = paths
        .into_iter()
        .filter_map(|path| {
            let path_buf = if std::path::Path::new(&path).is_absolute() {
                std::path::PathBuf::from(&path)
            } else {
                repo_workdir.join(&path)
            };

            if repo.path_is_in_workdir(&path_buf) {
                if std::path::Path::new(&path).is_absolute() {
                    if let Ok(relative) = path_buf.strip_prefix(&repo_workdir) {
                        Some(normalize_to_posix(&relative.to_string_lossy()))
                    } else {
                        let canonical_workdir = repo_workdir.canonicalize().ok()?;
                        let canonical_path = path_buf.canonicalize().ok()?;
                        canonical_path
                            .strip_prefix(&canonical_workdir)
                            .ok()
                            .map(|relative| normalize_to_posix(&relative.to_string_lossy()))
                    }
                } else {
                    Some(normalize_to_posix(&path))
                }
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

fn resolve_base_override_dirty_file_execution(
    base_commit: &str,
    ts: u128,
    edited_filepaths: &[String],
    dirty_files: &HashMap<String, String>,
    ignore_matcher: &IgnoreMatcher,
) -> Result<Option<ResolvedCheckpointExecution>, GitAiError> {
    let normalized_dirty_files = dirty_files
        .iter()
        .map(|(path, content)| (normalize_to_posix(path), content.clone()))
        .collect::<HashMap<_, _>>();
    let mut files = Vec::new();
    let mut resolved_dirty_files = HashMap::new();
    let mut missing_paths = Vec::new();

    for path in edited_filepaths {
        if should_ignore_file_with_matcher(path, ignore_matcher) {
            continue;
        }
        let Some(content) = normalized_dirty_files.get(path).cloned() else {
            missing_paths.push(path.clone());
            continue;
        };
        files.push(path.clone());
        resolved_dirty_files.insert(path.clone(), content);
    }

    if !missing_paths.is_empty() {
        return Err(GitAiError::Generic(format!(
            "base override requires dirty snapshot entries for explicit file(s): {}",
            missing_paths.join(", ")
        )));
    }

    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ResolvedCheckpointExecution {
            base_commit: base_commit.to_string(),
            ts,
            files,
            dirty_files: resolved_dirty_files,
        }))
    }
}

fn explicit_dirty_file_content_if_text(
    working_log: &PersistedWorkingLog,
    file_path: &str,
) -> Option<String> {
    working_log
        .dirty_files
        .as_ref()
        .and_then(|files| files.get(file_path))
        .filter(|content| !content.chars().any(|c| c == '\0'))
        .cloned()
}

#[allow(clippy::too_many_arguments)]
fn resolve_explicit_path_execution(
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    base_commit: &str,
    ts: u128,
    explicit_paths: &[String],
    ignore_matcher: &IgnoreMatcher,
    kind: CheckpointKind,
    is_pre_commit: bool,
) -> Result<Option<ResolvedCheckpointExecution>, GitAiError> {
    let repo_workdir = repo.workdir()?;
    let mut candidate_paths = Vec::new();
    let mut seen = HashSet::new();

    for path in explicit_paths {
        let normalized_path = normalize_to_posix(path);
        if !seen.insert(normalized_path.clone()) {
            continue;
        }
        if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
            continue;
        }

        let path_buf = if std::path::Path::new(&normalized_path).is_absolute() {
            PathBuf::from(&normalized_path)
        } else {
            repo_workdir.join(&normalized_path)
        };
        if !repo.path_is_in_workdir(&path_buf) {
            continue;
        }

        candidate_paths.push(normalized_path);
    }

    if candidate_paths.is_empty() {
        return Ok(None);
    }

    let status_pathspecs = candidate_paths.iter().cloned().collect::<HashSet<_>>();
    let explicit_statuses = repo
        .status(Some(&status_pathspecs), false)?
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<HashMap<_, _>>();
    let preserve_unchanged_explicit_paths = kind == CheckpointKind::Human && is_pre_commit;

    let mut files = Vec::new();
    let mut resolved_dirty_files = HashMap::new();

    for normalized_path in candidate_paths {
        // Status output uses NFC paths; the normalized_path may be NFD on some
        // filesystems, so look up with NFC to handle the mismatch.
        let nfc_key: String = normalized_path.nfc().collect();
        let status_entry = explicit_statuses.get(&nfc_key);
        if matches!(status_entry, Some(entry) if entry.kind == EntryKind::Unmerged) {
            continue;
        }

        let explicit_dirty_content =
            explicit_dirty_file_content_if_text(working_log, &normalized_path);
        if status_entry.is_none()
            && explicit_dirty_content.is_none()
            && !preserve_unchanged_explicit_paths
        {
            continue;
        }

        if let Some(content) = explicit_dirty_content {
            resolved_dirty_files.insert(normalized_path.clone(), content);
            files.push(normalized_path);
            continue;
        }

        let is_deleted = matches!(
            status_entry,
            Some(entry)
                if entry.staged == StatusCode::Deleted || entry.unstaged == StatusCode::Deleted
        );

        if is_text_file(working_log, &normalized_path)
            || (is_deleted && is_text_file_in_head(repo, &normalized_path))
        {
            files.push(normalized_path);
        }
    }

    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ResolvedCheckpointExecution {
            base_commit: base_commit.to_string(),
            ts,
            files,
            dirty_files: resolved_dirty_files,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_live_checkpoint_execution(
    repo: &Repository,
    kind: CheckpointKind,
    checkpoint_request: Option<&CheckpointRequest>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
    base_override_resolution_policy: BaseOverrideResolutionPolicy,
) -> Result<Option<ResolvedCheckpointExecution>, GitAiError> {
    let base_commit = resolve_base_commit(repo, base_commit_override);

    if repo.workdir().is_err() {
        eprintln!("Cannot run checkpoint on bare repositories");
        return Err(GitAiError::Generic(
            "Cannot run checkpoint on bare repositories".to_string(),
        ));
    }

    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let ignore_matcher = build_ignore_matcher(&ignore_patterns);

    let storage_start = Instant::now();
    let repo_storage = repo.storage.clone();
    let mut working_log = repo_storage.working_log_for_base_commit(&base_commit)?;
    tracing::debug!(
        "[BENCHMARK] Storage initialization took {:?}",
        storage_start.elapsed()
    );

    if is_pre_commit && base_commit_override.is_none() {
        let has_no_ai_edits = working_log
            .all_ai_touched_files()
            .map(|files| files.is_empty())
            .unwrap_or(true);
        let has_initial_attributions = !working_log.read_initial_attributions().files.is_empty();
        let has_explicit_ai_agent_context = kind.is_ai() && checkpoint_request.is_some();

        if has_no_ai_edits
            && !has_initial_attributions
            && !Config::get().get_feature_flags().inter_commit_move
            && !has_explicit_ai_agent_context
        {
            tracing::debug!("No AI edits in pre-commit checkpoint, skipping");
            return Ok(None);
        }
    }

    if let Some(dirty_files) = checkpoint_request.and_then(|result| result.dirty_files.as_ref()) {
        let string_dirty = dirty_files
            .iter()
            .map(|(k, v)| (k.to_string_lossy().to_string(), v.clone()))
            .collect();
        working_log.set_dirty_files(Some(string_dirty));
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let has_explicit_target_paths =
        explicit_capture_target_paths(kind, checkpoint_request).is_some();
    let pathspec_start = Instant::now();
    let filtered_pathspec =
        filtered_pathspecs_for_checkpoint_request(repo, kind, checkpoint_request);
    tracing::debug!(
        "[BENCHMARK] Pathspec filtering took {:?}",
        pathspec_start.elapsed()
    );

    // Base-override replays already provide the exact file list and content snapshot that
    // should be checkpointed. Re-running git status here turns daemon commit replay into a
    // full worktree scan on every commit, which is especially expensive on macOS runners.
    if base_commit_override.is_some() {
        let dirty_files_for_override = checkpoint_request
            .and_then(|result| result.dirty_files.as_ref())
            .map(|df| {
                df.iter()
                    .map(|(k, v)| (k.to_string_lossy().to_string(), v.clone()))
                    .collect::<HashMap<_, _>>()
            });
        match (
            filtered_pathspec.as_ref(),
            dirty_files_for_override.as_ref(),
        ) {
            (Some(explicit_paths), Some(dirty_files)) => {
                match resolve_base_override_dirty_file_execution(
                    &base_commit,
                    ts,
                    explicit_paths,
                    dirty_files,
                    &ignore_matcher,
                ) {
                    Ok(Some(resolved)) => {
                        tracing::debug!(
                            "[BENCHMARK] Reusing {} explicit dirty file(s) for base override checkpoint",
                            resolved.files.len()
                        );
                        return Ok(Some(resolved));
                    }
                    Ok(None) => {
                        if base_override_resolution_policy
                            == BaseOverrideResolutionPolicy::RequireExplicitSnapshot
                        {
                            return Ok(None);
                        }
                    }
                    Err(e) => {
                        if base_override_resolution_policy
                            == BaseOverrideResolutionPolicy::RequireExplicitSnapshot
                        {
                            return Err(e);
                        }
                    }
                }
            }
            _ if base_override_resolution_policy
                == BaseOverrideResolutionPolicy::RequireExplicitSnapshot =>
            {
                return Err(GitAiError::Generic(
                    "base override replay requires explicit in-repository target paths and a matching dirty snapshot".to_string(),
                ));
            }
            _ => {}
        }
    }

    if has_explicit_target_paths {
        return if let Some(explicit_paths) = filtered_pathspec.as_ref() {
            resolve_explicit_path_execution(
                repo,
                &working_log,
                &base_commit,
                ts,
                explicit_paths,
                &ignore_matcher,
                kind,
                is_pre_commit,
            )
        } else {
            Ok(None)
        };
    }

    let files_start = Instant::now();
    let files = get_all_tracked_files(
        repo,
        &base_commit,
        &working_log,
        filtered_pathspec.as_ref(),
        is_pre_commit,
        is_pre_commit && filtered_pathspec.is_some(),
        &ignore_matcher,
    )?;
    tracing::debug!(
        "[BENCHMARK] get_all_tracked_files found {} files, took {:?}",
        files.len(),
        files_start.elapsed()
    );

    let dirty_files = files
        .iter()
        .filter_map(|file_path| {
            working_log
                .dirty_files
                .as_ref()
                .and_then(|map| map.get(file_path).cloned())
                .map(|content| (file_path.clone(), content))
        })
        .collect::<HashMap<_, _>>();

    Ok(Some(ResolvedCheckpointExecution {
        base_commit,
        ts,
        files,
        dirty_files,
    }))
}

#[allow(clippy::too_many_arguments)]
fn execute_resolved_checkpoint(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    is_pre_commit: bool,
    resolved: ResolvedCheckpointExecution,
    checkpoint_start: Instant,
) -> Result<(usize, usize, usize), GitAiError> {
    let mut working_log = repo
        .storage
        .working_log_for_base_commit(&resolved.base_commit)?;
    if !resolved.dirty_files.is_empty() {
        working_log.set_dirty_files(Some(resolved.dirty_files.clone()));
    }

    let read_checkpoints_start = Instant::now();
    let mut checkpoints = working_log.read_all_checkpoints()?;
    tracing::debug!(
        "[BENCHMARK] Reading {} checkpoints took {:?}",
        checkpoints.len(),
        read_checkpoints_start.elapsed()
    );

    // Reject KnownHuman checkpoints that arrive within KNOWN_HUMAN_MIN_SECS_AFTER_AI
    // seconds of an AI checkpoint on any of the same files. These are likely spurious
    // IDE save events triggered by the AI completing its edit, not genuine human keystrokes.
    // Only compiled in non-test builds where the constant is non-zero; under --all-targets
    // clippy would otherwise flag the comparisons as always-false for u64.
    #[cfg(not(any(test, feature = "test-support")))]
    if kind == CheckpointKind::KnownHuman {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let too_soon = checkpoints.iter().rev().any(|cp| {
            cp.kind.is_ai()
                && now_secs.saturating_sub(cp.timestamp) < KNOWN_HUMAN_MIN_SECS_AFTER_AI
                && cp.entries.iter().any(|e| resolved.files.contains(&e.file))
        });
        if too_soon {
            tracing::debug!(
                "[KnownHuman] Rejected: fired within {}s of an AI checkpoint on the same file",
                KNOWN_HUMAN_MIN_SECS_AFTER_AI
            );
            return Ok((0, 0, 0));
        }
    }

    let save_states_start = Instant::now();
    let file_content_hashes = save_current_file_states(&working_log, &resolved.files)?;
    tracing::debug!(
        "[BENCHMARK] save_current_file_states for {} files took {:?}",
        resolved.files.len(),
        save_states_start.elapsed()
    );

    let hash_compute_start = Instant::now();
    let mut ordered_hashes: Vec<_> = file_content_hashes.iter().collect();
    ordered_hashes.sort_by_key(|(file_path, _)| *file_path);

    let mut combined_hasher = Sha256::new();
    for (file_path, hash) in ordered_hashes {
        combined_hasher.update(file_path.as_bytes());
        combined_hasher.update(hash.as_bytes());
    }
    let combined_hash = format!("{:x}", combined_hasher.finalize());
    tracing::debug!(
        "[BENCHMARK] Hash computation took {:?}",
        hash_compute_start.elapsed()
    );

    // Generate trace_id for this checkpoint - links all metrics events from this checkpoint together
    // This allows server-side analysis of multi-file edits, tool invocations, and checkpoint chains
    let trace_id = generate_trace_id();

    let entries_start = Instant::now();
    let (entries, file_stats) = smol::block_on(get_checkpoint_entries(
        kind,
        author,
        repo,
        &working_log,
        &resolved.files,
        &file_content_hashes,
        &checkpoints,
        checkpoint_request.as_ref(),
        resolved.ts,
        is_pre_commit,
        Some(resolved.base_commit.as_str()),
        trace_id.clone(),
    ))?;
    tracing::debug!(
        "[BENCHMARK] get_checkpoint_entries generated {} entries, took {:?}",
        entries.len(),
        entries_start.elapsed()
    );

    if !entries.is_empty() {
        let checkpoint_create_start = Instant::now();
        let mut checkpoint = Checkpoint::new(
            kind,
            combined_hash.clone(),
            author.to_string(),
            entries.clone(),
        );
        checkpoint.timestamp = (resolved.ts / 1000) as u64;
        checkpoint.line_stats = compute_line_stats(&file_stats)?;
        checkpoint.trace_id = Some(trace_id.clone());

        if kind.is_ai() {
            if let Some(cr) = &checkpoint_request {
                checkpoint.agent_id = cr.agent_id.clone();
                checkpoint.agent_metadata = if cr.metadata.is_empty() {
                    None
                } else {
                    Some(cr.metadata.clone())
                };
            }
        } else if kind == CheckpointKind::KnownHuman
            && let Some(cr) = &checkpoint_request
            && !cr.metadata.is_empty()
        {
            let editor = cr.metadata.get("kh_editor").cloned().unwrap_or_default();
            let editor_version = cr
                .metadata
                .get("kh_editor_version")
                .cloned()
                .unwrap_or_default();
            let extension_version = cr
                .metadata
                .get("kh_extension_version")
                .cloned()
                .unwrap_or_default();
            if !editor.is_empty() {
                use crate::authorship::working_log::KnownHumanMetadata;
                checkpoint.known_human_metadata = Some(KnownHumanMetadata {
                    editor,
                    editor_version,
                    extension_version,
                });
            }
        }
        tracing::debug!(
            "[BENCHMARK] Checkpoint creation took {:?}",
            checkpoint_create_start.elapsed()
        );

        let append_start = Instant::now();
        working_log.append_checkpoint(&checkpoint)?;
        tracing::debug!(
            "[BENCHMARK] Appending checkpoint to working log took {:?}",
            append_start.elapsed()
        );
        checkpoints.push(checkpoint.clone());

        let mut attrs =
            build_checkpoint_attrs(repo, &resolved.base_commit, checkpoint.agent_id.as_ref());

        // Add trace_id to attributes - links all checkpoint events together
        if let Some(ref tid) = checkpoint.trace_id {
            attrs = attrs.trace_id(tid);
        }

        // Extract tool_use_id from metadata if available
        // tool_use_id tracks specific tool invocations (e.g., bash tool calls from AI agents)
        // Allows linking checkpoint events to the exact tool use that triggered them
        let tool_use_id = checkpoint_request
            .as_ref()
            .and_then(|cr| cr.metadata.get("tool_use_id"))
            .map(|s| s.as_str());

        for (entry, file_stat) in entries.iter().zip(file_stats.iter()) {
            let mut values = crate::metrics::CheckpointValues::new()
                .checkpoint_ts(checkpoint.timestamp)
                .kind(checkpoint.kind.to_str().to_string())
                .file_path(entry.file.clone())
                .lines_added(file_stat.additions)
                .lines_deleted(file_stat.deletions)
                .lines_added_sloc(file_stat.additions_sloc)
                .lines_deleted_sloc(file_stat.deletions_sloc);

            // Add tool_use_id if available
            if let Some(tuid) = tool_use_id {
                values = values.tool_use_id(tuid);
            }

            let file_attrs = attrs.clone().author(&checkpoint.author);
            crate::metrics::record(values, file_attrs);
        }
    }

    let agent_tool = if kind.is_ai()
        && let Some(cr) = &checkpoint_request
        && let Some(aid) = &cr.agent_id
    {
        Some(aid.tool.as_str())
    } else {
        None
    };

    let label = if entries.len() > 1 {
        "checkpoint"
    } else {
        "commit"
    };

    if !quiet {
        let log_author = agent_tool.unwrap_or(author);
        let files_with_entries = entries.len();
        let total_uncommitted_files = resolved.files.len();

        if files_with_entries == total_uncommitted_files {
            eprintln!(
                "{} {} changed {} file(s) that have changed since the last {}",
                kind.to_str(),
                log_author,
                files_with_entries,
                label
            );
        } else {
            eprintln!(
                "{} {} changed {} of the {} file(s) that have changed since the last {} ({} already checkpointed)",
                kind.to_str(),
                log_author,
                files_with_entries,
                total_uncommitted_files,
                label,
                total_uncommitted_files - files_with_entries
            );
        }
    }

    tracing::debug!(
        "[BENCHMARK] Total checkpoint run took {:?}",
        checkpoint_start.elapsed()
    );
    Ok((entries.len(), resolved.files.len(), checkpoints.len()))
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_captured_checkpoint(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    checkpoint_request: Option<&CheckpointRequest>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
) -> Result<Option<PreparedCheckpointCapture>, GitAiError> {
    let Some((explicit_path_role, _)) = explicit_capture_target_paths(kind, checkpoint_request)
    else {
        return Err(GitAiError::Generic(
            "captured checkpoint requires explicit edited_filepaths or will_edit_filepaths"
                .to_string(),
        ));
    };

    let Some(resolved) = resolve_live_checkpoint_execution(
        repo,
        kind,
        checkpoint_request,
        is_pre_commit,
        base_commit_override,
        BaseOverrideResolutionPolicy::AllowFallback,
    )?
    else {
        return Ok(None);
    };

    if resolved.files.is_empty() {
        return Ok(None);
    }

    let explicit_paths = filtered_pathspecs_for_checkpoint_request(repo, kind, checkpoint_request)
        .ok_or_else(|| {
            GitAiError::Generic(
                "captured checkpoint requires explicit in-repository target paths".to_string(),
            )
        })?;

    let capture_id = new_async_checkpoint_capture_id();
    let capture_dir = async_checkpoint_capture_dir(&capture_id)?;
    let manifest_result = (|| -> Result<PreparedCheckpointManifest, GitAiError> {
        fs::create_dir_all(&capture_dir)?;
        fs::create_dir_all(capture_dir.join("blobs"))?;

        let live_working_log = repo
            .storage
            .working_log_for_base_commit(&resolved.base_commit)?;
        let mut files = Vec::with_capacity(resolved.files.len());
        for file_path in &resolved.files {
            let source = if let Some(content) = resolved.dirty_files.get(file_path).cloned() {
                PreparedCheckpointFileSource::DirtyFileContent { content }
            } else {
                let content = live_working_log.read_current_file_content(file_path)?;
                let mut hasher = Sha256::new();
                hasher.update(content.as_bytes());
                let blob_name = format!("{:x}", hasher.finalize());
                fs::write(capture_dir.join("blobs").join(&blob_name), content)?;
                PreparedCheckpointFileSource::BlobRef { blob_name }
            };
            files.push(PreparedCheckpointFile {
                path: file_path.clone(),
                source,
            });
        }

        let mut stored_checkpoint_request = checkpoint_request.cloned();
        if let Some(cr) = stored_checkpoint_request.as_mut() {
            cr.dirty_files = None;
        }

        let manifest = PreparedCheckpointManifest {
            repo_working_dir: repo
                .workdir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
            base_commit: resolved.base_commit.clone(),
            captured_at_ms: resolved.ts,
            kind,
            author: author.to_string(),
            is_pre_commit,
            explicit_path_role,
            explicit_paths,
            files,
            checkpoint_request: stored_checkpoint_request,
        };
        fs::write(
            async_checkpoint_manifest_path(&capture_id)?,
            serde_json::to_vec(&manifest)?,
        )?;
        Ok(manifest)
    })();

    let manifest = match manifest_result {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_failed_captured_checkpoint_prepare(&capture_dir, &capture_id, &error);
            return Err(error);
        }
    };

    Ok(Some(PreparedCheckpointCapture {
        capture_id,
        repo_working_dir: manifest.repo_working_dir,
        file_count: manifest.files.len(),
    }))
}

/// Patch the `checkpoint_request` stored in a captured checkpoint manifest so that
/// it carries the real agent identity, transcript, and metadata instead of the
/// synthetic placeholder written at bash-tool capture time.
pub(crate) fn update_captured_checkpoint_agent_context(
    capture_id: &str,
    author: &str,
    checkpoint_request: Option<&CheckpointRequest>,
) -> Result<(), GitAiError> {
    let manifest_path = async_checkpoint_manifest_path(capture_id)?;
    let mut manifest: PreparedCheckpointManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).map_err(|error| {
            GitAiError::Generic(format!(
                "failed reading captured checkpoint manifest {}: {}",
                manifest_path.display(),
                error
            ))
        })?)?;

    // Replace the synthetic "bash-tool" author with the real git user name.
    manifest.author = author.to_string();

    // Merge real agent context while preserving capture-specific fields
    // (file_paths, path_role, dirty_files) from the original.
    if let Some(real) = checkpoint_request {
        let mut updated = real.clone();
        if let Some(existing) = &manifest.checkpoint_request {
            updated.file_paths = existing.file_paths.clone();
            updated.path_role = existing.path_role;
        }
        updated.dirty_files = None;
        updated.captured_checkpoint_id = None;
        manifest.checkpoint_request = Some(updated);
    }

    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    Ok(())
}

pub(crate) fn load_captured_checkpoint_manifest(
    capture_id: &str,
) -> Result<PreparedCheckpointManifest, GitAiError> {
    let manifest_path = async_checkpoint_manifest_path(capture_id)?;
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| {
        GitAiError::Generic(format!(
            "failed reading captured checkpoint manifest {}: {}",
            manifest_path.display(),
            error
        ))
    })?;
    Ok(serde_json::from_str(&manifest)?)
}

fn validate_captured_checkpoint_manifest_repo(
    repo: &Repository,
    manifest: &PreparedCheckpointManifest,
) -> Result<(), GitAiError> {
    let manifest_repo_workdir = PathBuf::from(&manifest.repo_working_dir);
    let canonical_manifest_workdir = manifest_repo_workdir
        .canonicalize()
        .unwrap_or(manifest_repo_workdir);
    let repo_workdir = repo.workdir()?;
    let canonical_repo_workdir = repo_workdir.canonicalize().unwrap_or(repo_workdir);

    if canonical_manifest_workdir != canonical_repo_workdir {
        return Err(GitAiError::Generic(format!(
            "captured checkpoint manifest repo mismatch: manifest {} does not match repo {}",
            canonical_manifest_workdir.display(),
            canonical_repo_workdir.display()
        )));
    }

    Ok(())
}

pub fn execute_captured_checkpoint(
    repo: &Repository,
    capture_id: &str,
) -> Result<(usize, usize, usize), GitAiError> {
    let checkpoint_start = Instant::now();
    tracing::debug!("[BENCHMARK] Starting captured checkpoint replay");

    let manifest = load_captured_checkpoint_manifest(capture_id)?;
    validate_captured_checkpoint_manifest_repo(repo, &manifest)?;
    let mut dirty_files = HashMap::new();
    let capture_dir = async_checkpoint_capture_dir(capture_id)?;

    for file in &manifest.files {
        let content = match &file.source {
            PreparedCheckpointFileSource::DirtyFileContent { content } => content.clone(),
            PreparedCheckpointFileSource::BlobRef { blob_name } => {
                let blob_path = capture_dir.join("blobs").join(blob_name);
                fs::read_to_string(&blob_path).map_err(|error| {
                    GitAiError::Generic(format!(
                        "failed reading captured checkpoint blob {} for {}: {}",
                        blob_path.display(),
                        file.path,
                        error
                    ))
                })?
            }
        };
        dirty_files.insert(file.path.clone(), content);
    }

    let resolved = ResolvedCheckpointExecution {
        base_commit: manifest.base_commit.clone(),
        ts: manifest.captured_at_ms,
        files: manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect(),
        dirty_files,
    };

    execute_resolved_checkpoint(
        repo,
        &manifest.author,
        manifest.kind,
        true,
        manifest.checkpoint_request,
        manifest.is_pre_commit,
        resolved,
        checkpoint_start,
    )
}

// Gets tracked changes AND
fn get_status_of_files(
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    edited_filepaths: HashSet<String>,
    skip_untracked: bool,
    ignore_matcher: &IgnoreMatcher,
) -> Result<Vec<String>, GitAiError> {
    let mut files = Vec::new();

    // Use porcelain v2 format to get status

    let edited_filepaths_option = if edited_filepaths.is_empty() {
        None
    } else {
        Some(&edited_filepaths)
    };

    let status_start = Instant::now();
    let statuses = repo.status(edited_filepaths_option, skip_untracked)?;
    tracing::debug!(
        "[BENCHMARK]   git status call took {:?}",
        status_start.elapsed()
    );

    for entry in statuses {
        // Skip ignored files
        if entry.kind == EntryKind::Ignored {
            continue;
        }

        if should_ignore_file_with_matcher(&entry.path, ignore_matcher) {
            continue;
        }

        // Skip unmerged/conflicted files - we'll track them once the conflict is resolved
        if entry.kind == EntryKind::Unmerged {
            continue;
        }

        // Include files that have any change (staged or unstaged) or are untracked
        let has_change = entry.staged != StatusCode::Unmodified
            || entry.unstaged != StatusCode::Unmodified
            || entry.kind == EntryKind::Untracked;

        if has_change {
            // For deleted files, check if they were text files in HEAD
            let is_deleted =
                entry.staged == StatusCode::Deleted || entry.unstaged == StatusCode::Deleted;

            let is_text = if is_deleted {
                is_text_file_in_head(repo, &entry.path)
            } else {
                is_text_file(working_log, &entry.path)
            };

            if is_text {
                files.push(entry.path.clone());
            }
        }
    }

    Ok(files)
}

/// Get all files that should be tracked, including those from previous checkpoints and INITIAL attributions
///
fn get_all_tracked_files(
    repo: &Repository,
    _base_commit: &str,
    working_log: &PersistedWorkingLog,
    edited_filepaths: Option<&Vec<String>>,
    is_pre_commit: bool,
    preserve_explicit_pre_commit_paths: bool,
    ignore_matcher: &IgnoreMatcher,
) -> Result<Vec<String>, GitAiError> {
    let explicit_pre_commit_paths: HashSet<String> = edited_filepaths
        .map(|paths| {
            paths
                .iter()
                .map(|path| normalize_to_posix(path))
                .filter(|path| !should_ignore_file_with_matcher(path, ignore_matcher))
                .collect()
        })
        .unwrap_or_default();
    let mut files = explicit_pre_commit_paths.clone();

    // Helper closure to check if a path is within the repository
    // This prevents crashes when files outside the repo were tracked (e.g., opened in IDE but not in repo)
    // Use ok() to gracefully handle cases where workdir() fails (e.g., bare repos, test scripts that use mock_ai, etc)
    let repo_workdir = repo.workdir().ok();
    let is_path_in_repo = |path: &str| -> bool {
        // If we couldn't get workdir, skip filtering (allow all paths through)
        let Some(ref workdir) = repo_workdir else {
            return true;
        };
        let path_buf = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            workdir.join(path)
        };
        repo.path_is_in_workdir(&path_buf)
    };

    let initial_read_start = Instant::now();
    for file in working_log.read_initial_attributions().files.keys() {
        // Normalize path separators to forward slashes
        let normalized_path = normalize_to_posix(file);
        // Filter out paths outside the repository to prevent git command failures
        if !is_path_in_repo(&normalized_path) {
            tracing::debug!(
                "Skipping INITIAL file outside repository: {}",
                normalized_path
            );
            continue;
        }
        if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
            continue;
        }
        if is_text_file(working_log, &normalized_path) {
            files.insert(normalized_path);
        }
    }
    tracing::debug!(
        "[BENCHMARK]   Reading INITIAL attributions in get_all_tracked_files took {:?}",
        initial_read_start.elapsed()
    );

    let checkpoints_read_start = Instant::now();
    if let Ok(working_log_data) = working_log.read_all_checkpoints() {
        for checkpoint in &working_log_data {
            for entry in &checkpoint.entries {
                // Normalize path separators to forward slashes
                let normalized_path = normalize_to_posix(&entry.file);
                // Filter out paths outside the repository to prevent git command failures
                if !is_path_in_repo(&normalized_path) {
                    tracing::debug!(
                        "Skipping checkpoint file outside repository: {}",
                        normalized_path
                    );
                    continue;
                }
                if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
                    continue;
                }
                if !files.contains(&normalized_path) {
                    // Check if it's a text file before adding
                    if is_text_file(working_log, &normalized_path) {
                        files.insert(normalized_path);
                    }
                }
            }
        }
    }
    tracing::debug!(
        "[BENCHMARK]   Reading checkpoints in get_all_tracked_files took {:?}",
        checkpoints_read_start.elapsed()
    );

    let has_ai_checkpoints = if let Ok(working_log_data) = working_log.read_all_checkpoints() {
        working_log_data.iter().any(|checkpoint| {
            checkpoint.kind == CheckpointKind::AiAgent || checkpoint.kind == CheckpointKind::AiTab
        })
    } else {
        false
    };

    let status_files_start = Instant::now();
    let mut results_for_tracked_files = if is_pre_commit && !has_ai_checkpoints {
        get_status_of_files(repo, working_log, files, true, ignore_matcher)?
    } else {
        get_status_of_files(repo, working_log, files, false, ignore_matcher)?
    };
    tracing::debug!(
        "[BENCHMARK]   get_status_of_files in get_all_tracked_files took {:?}",
        status_files_start.elapsed()
    );

    // Ensure to always include all dirty files
    if let Some(ref dirty_files) = working_log.dirty_files {
        for file_path in dirty_files.keys() {
            // Normalize path separators to forward slashes
            let normalized_path = normalize_to_posix(file_path);
            // Filter out paths outside the repository to prevent git command failures
            if !is_path_in_repo(&normalized_path) {
                tracing::debug!(
                    "Skipping dirty file outside repository: {}",
                    normalized_path
                );
                continue;
            }
            if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
                continue;
            }
            // Only add if not already in the files list
            if !results_for_tracked_files.contains(&normalized_path) {
                // Check if it's a text file before adding
                if is_text_file(working_log, &normalized_path) {
                    results_for_tracked_files.push(normalized_path);
                }
            }
        }
    }

    if preserve_explicit_pre_commit_paths {
        for normalized_path in explicit_pre_commit_paths {
            if !is_path_in_repo(&normalized_path) {
                continue;
            }
            if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
                continue;
            }
            if results_for_tracked_files.contains(&normalized_path) {
                continue;
            }
            if is_text_file(working_log, &normalized_path)
                || is_text_file_in_head(repo, &normalized_path)
            {
                results_for_tracked_files.push(normalized_path);
            }
        }
    }

    Ok(results_for_tracked_files)
}

fn save_current_file_states(
    working_log: &PersistedWorkingLog,
    files: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let _read_start = Instant::now();

    // Extract only the data we need (no cloning the entire working_log)
    let blobs_dir = working_log.dir.join("blobs");
    let repo_workdir = working_log.repo_workdir.clone();
    let dirty_files = working_log.dirty_files.clone();

    // Process files concurrently with a semaphore limiting to 8 at a time
    let file_content_hashes = smol::block_on(async {
        let semaphore = Arc::new(smol::lock::Semaphore::new(8));
        let blobs_dir = Arc::new(blobs_dir);
        let repo_workdir = Arc::new(repo_workdir);
        let dirty_files = Arc::new(dirty_files);

        let futures = files.iter().map(|file_path| {
            let file_path = file_path.clone();
            let blobs_dir = Arc::clone(&blobs_dir);
            let repo_workdir = Arc::clone(&repo_workdir);
            let dirty_files = Arc::clone(&dirty_files);
            let semaphore = Arc::clone(&semaphore);

            async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await;

                // Read file content - check dirty_files first, then filesystem
                let content = if let Some(ref dirty_map) = *dirty_files {
                    dirty_map.get(&file_path).cloned()
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    // Construct absolute path
                    let abs_path = if std::path::Path::new(&file_path).is_absolute() {
                        file_path.clone()
                    } else {
                        repo_workdir.join(&file_path).to_string_lossy().to_string()
                    };
                    // Read from filesystem
                    std::fs::read_to_string(&abs_path).unwrap_or_default()
                });

                // Create SHA256 hash of the content
                let mut hasher = Sha256::new();
                hasher.update(content.as_bytes());
                let sha = format!("{:x}", hasher.finalize());

                // Ensure blobs directory exists
                std::fs::create_dir_all(&*blobs_dir)?;

                // Write content to blob file
                let blob_path = blobs_dir.join(&sha);
                std::fs::write(blob_path, content)?;

                Ok::<(String, String), GitAiError>((file_path, sha))
            }
        });

        // Collect results from all concurrent operations
        let results: Vec<Result<(String, String), GitAiError>> =
            stream::iter(futures).buffer_unordered(8).collect().await;

        // Convert results into HashMap
        let mut file_content_hashes = HashMap::new();
        for result in results {
            let (file_path, content_hash) = result?;
            file_content_hashes.insert(file_path, content_hash);
        }

        Ok::<HashMap<String, String>, GitAiError>(file_content_hashes)
    })?;

    Ok(file_content_hashes)
}

fn get_previous_content_from_head(
    repo: &Repository,
    file_path: &str,
    head_tree_id: &Option<String>,
) -> String {
    if let Some(tree_id) = head_tree_id.as_ref() {
        let head_tree = repo.find_tree(tree_id.clone()).ok();
        if let Some(tree) = head_tree {
            match tree.get_path(std::path::Path::new(file_path)) {
                Ok(entry) => {
                    if let Ok(blob) = repo.find_blob(entry.id()) {
                        let blob_content = blob.content().unwrap_or_default();
                        String::from_utf8_lossy(&blob_content).to_string()
                    } else {
                        String::new()
                    }
                }
                Err(_) => String::new(),
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

/// Compare file contents ignoring CRLF/LF differences.
fn content_eq_normalized(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    normalize_line_endings(a) == normalize_line_endings(b)
}

#[doc(hidden)]
pub fn is_ai_author_id(author_id: &str) -> bool {
    author_id != "human" && !author_id.starts_with("h_")
}

fn working_log_entry_has_non_human_attribution(entry: &WorkingLogEntry) -> bool {
    entry
        .line_attributions
        .iter()
        .any(|attr| is_ai_author_id(&attr.author_id))
        || entry
            .attributions
            .iter()
            .any(|attr| is_ai_author_id(&attr.author_id))
}

fn build_previous_file_state_maps(
    previous_checkpoints: &[Checkpoint],
    initial_attributions: &HashMap<String, Vec<LineAttribution>>,
) -> (HashMap<String, PreviousFileState>, HashSet<String>) {
    let mut previous_file_state_by_file: HashMap<String, PreviousFileState> = HashMap::new();
    let mut ai_touched_files: HashSet<String> = initial_attributions.keys().cloned().collect();

    // Keep only the latest entry for each file.
    for checkpoint in previous_checkpoints {
        for entry in &checkpoint.entries {
            previous_file_state_by_file.insert(
                entry.file.clone(),
                PreviousFileState {
                    blob_sha: entry.blob_sha.clone(),
                    attributions: entry.attributions.clone(),
                },
            );

            if checkpoint.kind.is_ai() || working_log_entry_has_non_human_attribution(entry) {
                ai_touched_files.insert(entry.file.clone());
            }
        }
    }

    (previous_file_state_by_file, ai_touched_files)
}

#[allow(clippy::too_many_arguments)]
fn get_checkpoint_entry_for_file(
    file_path: String,
    kind: CheckpointKind,
    is_pre_commit: bool,
    repo: Repository,
    working_log: PersistedWorkingLog,
    previous_file_state_by_file: Arc<HashMap<String, PreviousFileState>>,
    ai_touched_files: Arc<HashSet<String>>,
    file_content_hash: String,
    author_id: Arc<String>,
    head_commit_sha: Arc<Option<String>>,
    head_tree_id: Arc<Option<String>>,
    initial_attributions: Arc<HashMap<String, Vec<LineAttribution>>>,
    initial_snapshot_contents: Arc<HashMap<String, String>>,
    ts: u128,
) -> Result<Option<(WorkingLogEntry, FileLineStats)>, GitAiError> {
    let feature_flag_inter_commit_move = Config::get().get_feature_flags().inter_commit_move;

    let file_start = Instant::now();
    let initial_attrs_for_file = initial_attributions
        .get(&file_path)
        .cloned()
        .unwrap_or_default();
    let initial_snapshot_content = initial_snapshot_contents.get(&file_path).cloned();

    let previous_state = previous_file_state_by_file.get(&file_path).cloned();
    let has_prior_ai_edits = ai_touched_files.contains(&file_path);

    // Pre-commit fast path:
    // If this file has no prior AI attribution and no INITIAL attribution,
    // we can skip it entirely. Human-only files do not affect AI authorship.
    if is_pre_commit && !kind.is_ai() && !has_prior_ai_edits && initial_attrs_for_file.is_empty() {
        return Ok(None);
    }

    let current_content = working_log
        .read_current_file_content(&file_path)
        .unwrap_or_default();

    // Non-pre-commit fast path:
    // Preserve existing `git-ai checkpoint` behavior for human-only files by writing an
    // attribution-empty entry while still capturing line stats.
    // KnownHuman checkpoints must bypass this path so they record h_<hash> attributions
    // that later AI checkpoints can use to identify human-written lines.
    if kind == CheckpointKind::Human && !has_prior_ai_edits && initial_attrs_for_file.is_empty() {
        let previous_content = if let Some(state) = previous_state.as_ref() {
            working_log
                .get_file_version(&state.blob_sha)
                .unwrap_or_default()
        } else {
            get_previous_content_from_head(&repo, &file_path, head_tree_id.as_ref())
        };

        if content_eq_normalized(&current_content, &previous_content) {
            return Ok(None);
        }

        let stats = compute_file_line_stats(&previous_content, &current_content);
        let entry = WorkingLogEntry::new(file_path, file_content_hash, Vec::new(), Vec::new());
        return Ok(Some((entry, stats)));
    }

    let from_checkpoint = previous_state.as_ref().map(|state| {
        (
            working_log
                .get_file_version(&state.blob_sha)
                .unwrap_or_default(),
            state.attributions.clone(),
        )
    });

    let is_from_checkpoint = from_checkpoint.is_some();
    let (previous_content, prev_attributions) = if let Some((content, attrs)) = from_checkpoint {
        // File exists in a previous checkpoint - use that
        (content, attrs)
    } else {
        // File doesn't exist in any previous checkpoint - need to initialize from git + INITIAL
        let previous_content =
            get_previous_content_from_head(&repo, &file_path, head_tree_id.as_ref());

        // Skip if no changes, UNLESS we have INITIAL attributions for this file
        // (in which case we need to create an entry to record those attributions)
        if content_eq_normalized(&current_content, &previous_content)
            && initial_attrs_for_file.is_empty()
        {
            return Ok(None);
        }

        // Build a set of lines covered by INITIAL attributions
        let mut initial_covered_lines: HashSet<u32> = HashSet::new();
        for attr in &initial_attrs_for_file {
            for line in attr.start_line..=attr.end_line {
                initial_covered_lines.insert(line);
            }
        }

        // Start with INITIAL attributions (they win)
        let mut prev_line_attributions = initial_attrs_for_file.clone();
        let mut blamed_lines: HashSet<u32> = HashSet::new();

        // Get blame for lines not in INITIAL
        let blame_start = Instant::now();
        let mut ai_blame_opts = GitAiBlameOptions::default();
        #[allow(clippy::field_reassign_with_default)]
        {
            ai_blame_opts.no_output = true;
            ai_blame_opts.return_human_authors_as_human = true;
            ai_blame_opts.use_prompt_hashes_as_names = true;
            ai_blame_opts.newest_commit = head_commit_sha.as_ref().clone();
            ai_blame_opts.oldest_date = Some(*OLDEST_AI_BLAME_DATE);
        }
        let ai_blame = if feature_flag_inter_commit_move {
            repo.blame(&file_path, &ai_blame_opts).ok()
        } else {
            // When skipping blame, default all lines to "human"
            let total_lines = previous_content.lines().count() as u32;
            let mut line_authors: HashMap<u32, String> = HashMap::new();
            for line_num in 1..=total_lines {
                line_authors.insert(line_num, CheckpointKind::Human.to_str());
            }
            let prompt_records: HashMap<String, PromptRecord> = HashMap::new();
            Some((line_authors, prompt_records))
        };

        tracing::debug!(
            "[BENCHMARK] Blame for {} took {:?}",
            file_path,
            blame_start.elapsed()
        );

        // Add blame results for lines NOT covered by INITIAL
        if let Some((blames, _)) = ai_blame {
            for (line, author) in blames {
                blamed_lines.insert(line);
                // Skip if INITIAL already has this line
                if initial_covered_lines.contains(&line) {
                    continue;
                }

                // Skip human-authored lines - they should remain human
                if author == CheckpointKind::Human.to_str() {
                    continue;
                }

                prev_line_attributions.push(LineAttribution {
                    start_line: line,
                    end_line: line,
                    author_id: author.clone(),
                    overrode: None,
                });
            }
        }

        // For AI checkpoints, attribute any lines NOT in INITIAL and NOT returned by ai_blame
        if kind.is_ai() {
            let total_lines = current_content.lines().count() as u32;
            for line_num in 1..=total_lines {
                if !initial_covered_lines.contains(&line_num) && !blamed_lines.contains(&line_num) {
                    prev_line_attributions.push(LineAttribution {
                        start_line: line_num,
                        end_line: line_num,
                        author_id: author_id.as_ref().clone(),
                        overrode: None,
                    });
                }
            }
        }

        // INITIAL line numbers refer to the file state at the moment INITIAL was written.
        // Snapshot-aware INITIAL storage preserves that exact content; older INITIAL files
        // fall back to the legacy "current content" behavior.
        let content_for_line_conversion = if !initial_attrs_for_file.is_empty() {
            initial_snapshot_content
                .as_deref()
                .unwrap_or(&current_content)
        } else {
            &previous_content
        };

        // Convert any line attributions to character attributions
        let prev_attributions =
            crate::authorship::attribution_tracker::line_attributions_to_attributions(
                &prev_line_attributions,
                content_for_line_conversion,
                INITIAL_ATTRIBUTION_TS,
            );

        // When INITIAL has a persisted snapshot, use that as the previous content so later
        // edits after a restore/squash are tracked correctly. Older INITIAL files fall back
        // to the legacy current-content behavior.
        let adjusted_previous = if !initial_attrs_for_file.is_empty() {
            initial_snapshot_content.unwrap_or_else(|| current_content.clone())
        } else {
            previous_content
        };

        (adjusted_previous, prev_attributions)
    };

    // Skip if no changes (but we already checked this earlier, accounting for INITIAL attributions)
    // For files from previous checkpoints, check if content has changed
    if is_from_checkpoint && content_eq_normalized(&current_content, &previous_content) {
        if current_content == previous_content {
            // Byte-identical — truly no change.
            return Ok(None);
        }
        // Content differs only in line endings (CRLF ↔ LF). Update the stored blob
        // to the current content so future diffs compare LF-vs-LF. Without this,
        // the stale CRLF blob causes capture_diff_slices to see every line as changed,
        // and AI checkpoints (force_split=true) would re-attribute all lines to AI.
        // Remap attributions through line-number space to adjust byte offsets.
        let line_attributions =
            crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
                &prev_attributions,
                &previous_content,
                kind.is_ai(),
            );
        let remapped_attributions =
            crate::authorship::attribution_tracker::line_attributions_to_attributions(
                &line_attributions,
                &current_content,
                ts,
            );
        let entry = WorkingLogEntry::new(
            file_path,
            file_content_hash,
            remapped_attributions,
            line_attributions,
        );
        return Ok(Some((entry, FileLineStats::default())));
    }

    let (entry, stats) = make_entry_for_file(FileEntryInput {
        file_path: &file_path,
        blob_sha: &file_content_hash,
        author_id: author_id.as_ref(),
        is_ai_checkpoint: kind.is_ai(),
        previous_content: &previous_content,
        previous_attributions: &prev_attributions,
        content: &current_content,
        ts,
    })?;
    tracing::debug!(
        "[BENCHMARK] Processing file {} took {:?}",
        file_path,
        file_start.elapsed()
    );
    Ok(Some((entry, stats)))
}

#[allow(clippy::too_many_arguments)]
async fn get_checkpoint_entries(
    kind: CheckpointKind,
    author: &str,
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    files: &[String],
    file_content_hashes: &HashMap<String, String>,
    previous_checkpoints: &[Checkpoint],
    checkpoint_request: Option<&CheckpointRequest>,
    ts: u128,
    is_pre_commit: bool,
    head_commit_override: Option<&str>,
    trace_id: String,
) -> Result<(Vec<WorkingLogEntry>, Vec<FileLineStats>), GitAiError> {
    let entries_fn_start = Instant::now();

    // Read INITIAL attributions from working log (empty if file doesn't exist)
    let initial_read_start = Instant::now();
    let initial_data = working_log.read_initial_attributions();
    let initial_snapshot_contents: HashMap<String, String> = initial_data
        .files
        .keys()
        .filter_map(|file_path| {
            working_log
                .initial_file_content_from(&initial_data, file_path)
                .map(|content| (file_path.clone(), content))
        })
        .collect();
    let initial_attributions = initial_data.files;
    tracing::debug!(
        "[BENCHMARK] Reading initial attributions took {:?}",
        initial_read_start.elapsed()
    );

    let precompute_start = Instant::now();
    let (previous_file_state_by_file, ai_touched_files) =
        build_previous_file_state_maps(previous_checkpoints, &initial_attributions);
    tracing::debug!(
        "[BENCHMARK] Precomputing previous state maps took {:?}",
        precompute_start.elapsed()
    );

    // Determine author_id based on checkpoint kind and agent_id
    let author_id = match kind {
        CheckpointKind::Human => kind.to_str(), // "human" — stripped, never attested
        CheckpointKind::KnownHuman => {
            crate::authorship::authorship_log_serialization::generate_human_short_hash(author)
        }
        _ => {
            // AI kinds: compose session_id::trace_id
            checkpoint_request
                .and_then(|result| result.agent_id.as_ref())
                .map(|aid| {
                    let session_id = generate_session_id(&aid.id, &aid.tool);
                    format!("{}::{}", session_id, trace_id)
                })
                .unwrap_or_else(|| kind.to_str())
        }
    };

    // Get HEAD commit info for git operations
    let head_commit = head_commit_override
        .map(str::trim)
        .filter(|sha| !sha.is_empty() && *sha != "initial")
        .and_then(|sha| repo.find_commit(sha.to_string()).ok())
        .or_else(|| {
            repo.head()
                .ok()
                .and_then(|h| h.target().ok())
                .and_then(|oid| repo.find_commit(oid).ok())
        });
    let head_commit_sha = head_commit.as_ref().map(|c| c.id().to_string());
    let head_tree_id = head_commit
        .as_ref()
        .and_then(|c| c.tree().ok())
        .map(|t| t.id().to_string());

    const MAX_CONCURRENT: usize = 30;

    // Create a semaphore to limit concurrent tasks
    let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));

    // Move other repeated allocations outside the loop
    let previous_file_state_by_file = Arc::new(previous_file_state_by_file);
    let ai_touched_files = Arc::new(ai_touched_files);
    let author_id = Arc::new(author_id);
    let head_commit_sha = Arc::new(head_commit_sha);
    let head_tree_id = Arc::new(head_tree_id);
    let initial_attributions = Arc::new(initial_attributions);
    let initial_snapshot_contents = Arc::new(initial_snapshot_contents);

    // Spawn tasks for each file
    let spawn_start = Instant::now();
    let mut tasks = Vec::new();

    for file_path in files {
        let file_path = file_path.clone();
        let repo = repo.clone();
        let working_log = working_log.clone();
        let previous_file_state_by_file = Arc::clone(&previous_file_state_by_file);
        let ai_touched_files = Arc::clone(&ai_touched_files);
        let author_id = Arc::clone(&author_id);
        let head_commit_sha = Arc::clone(&head_commit_sha);
        let head_tree_id = Arc::clone(&head_tree_id);
        let blob_sha = file_content_hashes
            .get(&file_path)
            .cloned()
            .unwrap_or_default();
        let initial_attributions = Arc::clone(&initial_attributions);
        let initial_snapshot_contents = Arc::clone(&initial_snapshot_contents);
        let semaphore = Arc::clone(&semaphore);

        let task = smol::spawn(async move {
            // Acquire semaphore permit to limit concurrency
            let _permit = semaphore.acquire().await;

            // Wrap all the blocking git operations in smol::unblock
            smol::unblock(move || {
                get_checkpoint_entry_for_file(
                    file_path,
                    kind,
                    is_pre_commit,
                    repo,
                    working_log,
                    previous_file_state_by_file,
                    ai_touched_files,
                    blob_sha,
                    author_id.clone(),
                    head_commit_sha.clone(),
                    head_tree_id.clone(),
                    initial_attributions.clone(),
                    initial_snapshot_contents.clone(),
                    ts,
                )
            })
            .await
        });

        tasks.push(task);
    }
    tracing::debug!(
        "[BENCHMARK] Spawning {} tasks took {:?}",
        tasks.len(),
        spawn_start.elapsed()
    );

    // Await all tasks concurrently
    let await_start = Instant::now();
    let results = futures::future::join_all(tasks).await;
    tracing::debug!(
        "[BENCHMARK] Awaiting {} tasks took {:?}",
        results.len(),
        await_start.elapsed()
    );

    // Process results
    let process_start = Instant::now();
    let results_count = results.len();
    let mut entries = Vec::new();
    let mut file_stats = Vec::new();
    for result in results {
        match result {
            Ok(Some((entry, stats))) => {
                entries.push(entry);
                file_stats.push(stats);
            }
            Ok(None) => {} // File had no changes
            Err(e) => return Err(e),
        }
    }
    tracing::debug!(
        "[BENCHMARK] Processing {} results took {:?}",
        results_count,
        process_start.elapsed()
    );
    tracing::debug!(
        "[BENCHMARK] get_checkpoint_entries function total took {:?}",
        entries_fn_start.elapsed()
    );

    Ok((entries, file_stats))
}

struct FileEntryInput<'a> {
    file_path: &'a str,
    blob_sha: &'a str,
    author_id: &'a str,
    is_ai_checkpoint: bool,
    previous_content: &'a str,
    previous_attributions: &'a [Attribution],
    content: &'a str,
    ts: u128,
}

fn make_entry_for_file(
    input: FileEntryInput<'_>,
) -> Result<(WorkingLogEntry, FileLineStats), GitAiError> {
    let FileEntryInput {
        file_path,
        blob_sha,
        author_id,
        is_ai_checkpoint,
        previous_content,
        previous_attributions,
        content,
        ts,
    } = input;

    let tracker = AttributionTracker::new();

    let fill_start = Instant::now();
    let filled_in_prev_attributions = tracker.attribute_unattributed_ranges(
        previous_content,
        previous_attributions,
        &CheckpointKind::Human.to_str(),
        ts - 1,
    );
    tracing::debug!(
        "[BENCHMARK]   attribute_unattributed_ranges for {} took {:?}",
        file_path,
        fill_start.elapsed()
    );

    let update_start = Instant::now();
    let new_attributions = tracker.update_attributions_for_checkpoint(
        previous_content,
        content,
        &filled_in_prev_attributions,
        author_id,
        ts,
        is_ai_checkpoint,
    )?;
    tracing::debug!(
        "[BENCHMARK]   update_attributions for {} took {:?}",
        file_path,
        update_start.elapsed()
    );

    // TODO Consider discarding any "uncontentious" attributions for the human author. Any human attributions that do not share a line with any other author's attributions can be discarded.
    // let filtered_attributions = crate::authorship::attribution_tracker::discard_uncontentious_attributions_for_author(&new_attributions, &CheckpointKind::Human.to_str());

    let line_attr_start = Instant::now();
    let line_attributions =
        crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
            &new_attributions,
            content,
            is_ai_checkpoint,
        );
    tracing::debug!(
        "[BENCHMARK]   attributions_to_line_attributions for {} took {:?}",
        file_path,
        line_attr_start.elapsed()
    );

    // Compute line stats while we already have both contents in memory
    let stats_start = Instant::now();
    let line_stats = compute_file_line_stats(previous_content, content);
    tracing::debug!(
        "[BENCHMARK]   compute_file_line_stats for {} took {:?}",
        file_path,
        stats_start.elapsed()
    );

    let entry = WorkingLogEntry::new(
        file_path.to_string(),
        blob_sha.to_string(),
        new_attributions,
        line_attributions,
    );

    Ok((entry, line_stats))
}

/// Compute line statistics for a single file by diffing previous and current content
#[doc(hidden)]
pub fn compute_file_line_stats(previous_content: &str, current_content: &str) -> FileLineStats {
    let mut stats = FileLineStats::default();

    // Use imara_diff to count line changes (matches git's diff algorithm)
    let changes = compute_line_changes(previous_content, current_content);
    for change in changes {
        match change.tag() {
            LineChangeTag::Insert => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.additions += change.value().lines().count() as u32;
                stats.additions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Delete => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.deletions += change.value().lines().count() as u32;
                stats.deletions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Equal => {}
        }
    }

    stats
}

/// Aggregate line statistics from individual file stats
/// This avoids redundant diff computation since stats are already computed during entry creation
fn compute_line_stats(
    file_stats: &[FileLineStats],
) -> Result<crate::authorship::working_log::CheckpointLineStats, GitAiError> {
    let mut stats = crate::authorship::working_log::CheckpointLineStats::default();

    // Aggregate line stats from all files
    for file_stat in file_stats {
        stats.additions += file_stat.additions;
        stats.deletions += file_stat.deletions;
        stats.additions_sloc += file_stat.additions_sloc;
        stats.deletions_sloc += file_stat.deletions_sloc;
    }

    Ok(stats)
}

fn is_text_file(working_log: &PersistedWorkingLog, path: &str) -> bool {
    // Normalize path for dirty_files lookup
    let normalized_path = normalize_to_posix(path);
    let skip_metadata_check = working_log
        .dirty_files
        .as_ref()
        .map(|m| m.contains_key(&normalized_path))
        .unwrap_or(false);

    if !skip_metadata_check {
        if let Ok(metadata) = std::fs::metadata(working_log.to_repo_absolute_path(&normalized_path))
        {
            if !metadata.is_file() {
                return false;
            }
        } else {
            return false; // If metadata can't be read, treat as non-text
        }
    }

    working_log
        .read_current_file_content(&normalized_path)
        .map(|content| !content.chars().any(|c| c == '\0'))
        .unwrap_or(false)
}

fn is_text_file_in_head(repo: &Repository, path: &str) -> bool {
    // For deleted files, check if they were text files in HEAD
    let head_commit = match repo
        .head()
        .ok()
        .and_then(|h| h.target().ok())
        .and_then(|oid| repo.find_commit(oid).ok())
    {
        Some(commit) => commit,
        None => return false,
    };

    let head_tree = match head_commit.tree().ok() {
        Some(tree) => tree,
        None => return false,
    };

    match head_tree.get_path(std::path::Path::new(path)) {
        Ok(entry) => {
            if let Ok(blob) = repo.find_blob(entry.id()) {
                // Consider a file text if it contains no null bytes
                let blob_content = match blob.content() {
                    Ok(content) => content,
                    Err(_) => return false,
                };
                !blob_content.contains(&0)
            } else {
                false
            }
        }
        Err(_) => false,
    }
}
