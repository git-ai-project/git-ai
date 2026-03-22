use crate::authorship::attribution_tracker::{
    Attribution, AttributionTracker, INITIAL_ATTRIBUTION_TS, LineAttribution,
};
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::ignore::{
    IgnoreMatcher, build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::imara_diff_utils::{LineChangeTag, compute_line_changes};
use crate::authorship::working_log::CheckpointKind;
use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};
use crate::commands::blame::{GitAiBlameOptions, OLDEST_AI_BLAME_DATE};
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::repo_storage::PersistedWorkingLog;
use crate::git::repository::Repository;
use crate::git::status::{EntryKind, StatusCode};
use crate::utils::{debug_log, normalize_to_posix};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

/// Per-file line statistics (in-memory only, not persisted)
#[derive(Debug, Clone, Default)]
struct FileLineStats {
    additions: u32,
    deletions: u32,
    additions_sloc: u32,
    deletions_sloc: u32,
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
    pub reset: bool,
    pub is_pre_commit: bool,
    pub explicit_path_role: PreparedPathRole,
    pub explicit_paths: Vec<String>,
    pub files: Vec<PreparedCheckpointFile>,
    #[serde(default)]
    pub agent_run_result: Option<AgentRunResult>,
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

/// Build EventAttributes with repo metadata.
/// Reused for both AgentUsage and Checkpoint events.
fn build_checkpoint_attrs(
    repo: &Repository,
    base_commit: &str,
    agent_id: Option<&AgentId>,
) -> crate::metrics::EventAttributes {
    let mut attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
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

    // Attach custom attributes
    attrs = attrs.custom_attributes_map(crate::config::Config::get().custom_attributes());

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
    kind: CheckpointKind,
    agent_run_result: Option<&AgentRunResult>,
) -> Option<(PreparedPathRole, Vec<String>)> {
    let result = agent_run_result?;
    let (role, paths) = if kind == CheckpointKind::Human {
        (
            PreparedPathRole::WillEdit,
            result.will_edit_filepaths.as_ref()?,
        )
    } else {
        (PreparedPathRole::Edited, result.edited_filepaths.as_ref()?)
    };

    let filtered = paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        None
    } else {
        Some((role, filtered))
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
    reset: bool,
    quiet: bool,
    agent_run_result: Option<AgentRunResult>,
    is_pre_commit: bool,
) -> Result<(usize, usize, usize), GitAiError> {
    run_with_base_commit_override(
        repo,
        author,
        kind,
        reset,
        quiet,
        agent_run_result,
        is_pre_commit,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_with_base_commit_override(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    reset: bool,
    quiet: bool,
    agent_run_result: Option<AgentRunResult>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
) -> Result<(usize, usize, usize), GitAiError> {
    let checkpoint_start = Instant::now();
    debug_log("[BENCHMARK] Starting checkpoint run");
    let resolved = resolve_live_checkpoint_execution(
        repo,
        kind,
        agent_run_result.as_ref(),
        is_pre_commit,
        base_commit_override,
    )?;
    let Some(resolved) = resolved else {
        debug_log(&format!(
            "[BENCHMARK] Total checkpoint run took {:?}",
            checkpoint_start.elapsed()
        ));
        return Ok((0, 0, 0));
    };

    execute_resolved_checkpoint(
        repo,
        author,
        kind,
        reset,
        quiet,
        agent_run_result,
        is_pre_commit,
        resolved,
        checkpoint_start,
    )
}

fn filtered_pathspecs_for_agent_run_result(
    repo: &Repository,
    kind: CheckpointKind,
    agent_run_result: Option<&AgentRunResult>,
) -> Option<Vec<String>> {
    let (_, paths) = explicit_capture_target_paths(kind, agent_run_result)?;
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

#[allow(clippy::too_many_arguments)]
fn resolve_live_checkpoint_execution(
    repo: &Repository,
    kind: CheckpointKind,
    agent_run_result: Option<&AgentRunResult>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
) -> Result<Option<ResolvedCheckpointExecution>, GitAiError> {
    let base_commit = resolve_base_commit(repo, base_commit_override);

    if repo.workdir().is_err() {
        eprintln!("Cannot run checkpoint on bare repositories");
        return Err(GitAiError::Generic(
            "Cannot run checkpoint on bare repositories".to_string(),
        ));
    }

    crate::commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(repo);

    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let ignore_matcher = build_ignore_matcher(&ignore_patterns);

    let storage_start = Instant::now();
    let repo_storage = repo.storage.clone();
    let mut working_log = repo_storage.working_log_for_base_commit(&base_commit);
    debug_log(&format!(
        "[BENCHMARK] Storage initialization took {:?}",
        storage_start.elapsed()
    ));

    if is_pre_commit && base_commit_override.is_none() {
        let has_no_ai_edits = working_log
            .all_ai_touched_files()
            .map(|files| files.is_empty())
            .unwrap_or(true);
        let has_initial_attributions = !working_log.read_initial_attributions().files.is_empty();

        if has_no_ai_edits
            && !has_initial_attributions
            && !Config::get().get_feature_flags().inter_commit_move
        {
            debug_log("No AI edits in pre-commit checkpoint, skipping");
            return Ok(None);
        }
    }

    if let Some(dirty_files) = agent_run_result.and_then(|result| result.dirty_files.clone()) {
        working_log.set_dirty_files(Some(dirty_files));
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let pathspec_start = Instant::now();
    let filtered_pathspec = filtered_pathspecs_for_agent_run_result(repo, kind, agent_run_result);
    debug_log(&format!(
        "[BENCHMARK] Pathspec filtering took {:?}",
        pathspec_start.elapsed()
    ));

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
    debug_log(&format!(
        "[BENCHMARK] get_all_tracked_files found {} files, took {:?}",
        files.len(),
        files_start.elapsed()
    ));

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
    reset: bool,
    quiet: bool,
    agent_run_result: Option<AgentRunResult>,
    is_pre_commit: bool,
    resolved: ResolvedCheckpointExecution,
    checkpoint_start: Instant,
) -> Result<(usize, usize, usize), GitAiError> {
    let mut working_log = repo
        .storage
        .working_log_for_base_commit(&resolved.base_commit);
    if !resolved.dirty_files.is_empty() {
        working_log.set_dirty_files(Some(resolved.dirty_files.clone()));
    }

    let read_checkpoints_start = Instant::now();
    let mut checkpoints = if reset {
        working_log.reset_working_log()?;
        Vec::new()
    } else {
        working_log.read_all_checkpoints()?
    };
    debug_log(&format!(
        "[BENCHMARK] Reading {} checkpoints took {:?}",
        checkpoints.len(),
        read_checkpoints_start.elapsed()
    ));

    let save_states_start = Instant::now();
    let file_content_hashes = save_current_file_states(&working_log, &resolved.files)?;
    debug_log(&format!(
        "[BENCHMARK] save_current_file_states for {} files took {:?}",
        resolved.files.len(),
        save_states_start.elapsed()
    ));

    let hash_compute_start = Instant::now();
    let mut ordered_hashes: Vec<_> = file_content_hashes.iter().collect();
    ordered_hashes.sort_by_key(|(file_path, _)| *file_path);

    let mut combined_hasher = Sha256::new();
    for (file_path, hash) in ordered_hashes {
        combined_hasher.update(file_path.as_bytes());
        combined_hasher.update(hash.as_bytes());
    }
    let combined_hash = format!("{:x}", combined_hasher.finalize());
    debug_log(&format!(
        "[BENCHMARK] Hash computation took {:?}",
        hash_compute_start.elapsed()
    ));

    let entries_start = Instant::now();
    let (entries, file_stats) = smol::block_on(get_checkpoint_entries(
        kind,
        repo,
        &working_log,
        &resolved.files,
        &file_content_hashes,
        &checkpoints,
        agent_run_result.as_ref(),
        resolved.ts,
        is_pre_commit,
        Some(resolved.base_commit.as_str()),
    ))?;
    debug_log(&format!(
        "[BENCHMARK] get_checkpoint_entries generated {} entries, took {:?}",
        entries.len(),
        entries_start.elapsed()
    ));

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

        if kind != CheckpointKind::Human
            && let Some(agent_run) = &agent_run_result
        {
            checkpoint.transcript = Some(agent_run.transcript.clone().unwrap_or_default());
            checkpoint.agent_id = Some(agent_run.agent_id.clone());
            checkpoint.agent_metadata = agent_run.agent_metadata.clone();
        }
        debug_log(&format!(
            "[BENCHMARK] Checkpoint creation took {:?}",
            checkpoint_create_start.elapsed()
        ));

        if kind != CheckpointKind::Human
            && checkpoint.agent_id.is_some()
            && checkpoint.transcript.is_some()
            && let Err(e) = upsert_checkpoint_prompt_to_db(
                &checkpoint,
                working_log.repo_workdir.to_string_lossy().to_string(),
                None,
            )
        {
            debug_log(&format!(
                "[Warning] Failed to upsert prompt to database: {}",
                e
            ));
            crate::observability::log_error(
                &e,
                Some(serde_json::json!({
                    "operation": "checkpoint_prompt_upsert",
                    "agent_tool": checkpoint.agent_id.as_ref().map(|a| a.tool.as_str())
                })),
            );
        }

        let append_start = Instant::now();
        working_log.append_checkpoint(&checkpoint)?;
        debug_log(&format!(
            "[BENCHMARK] Appending checkpoint to working log took {:?}",
            append_start.elapsed()
        ));
        checkpoints.push(checkpoint.clone());

        let attrs =
            build_checkpoint_attrs(repo, &resolved.base_commit, checkpoint.agent_id.as_ref());

        if kind != CheckpointKind::Human
            && let Some(agent_id) = checkpoint.agent_id.as_ref()
            && should_emit_agent_usage(agent_id)
        {
            let values = crate::metrics::AgentUsageValues::new();
            crate::metrics::record(values, attrs.clone());
        }

        for (entry, file_stat) in entries.iter().zip(file_stats.iter()) {
            let values = crate::metrics::CheckpointValues::new()
                .checkpoint_ts(checkpoint.timestamp)
                .kind(checkpoint.kind.to_str().to_string())
                .file_path(entry.file.clone())
                .lines_added(file_stat.additions)
                .lines_deleted(file_stat.deletions)
                .lines_added_sloc(file_stat.additions_sloc)
                .lines_deleted_sloc(file_stat.deletions_sloc);
            let file_attrs = attrs.clone().author(&checkpoint.author);
            crate::metrics::record(values, file_attrs);
        }
    }

    let agent_tool = if kind != CheckpointKind::Human
        && let Some(agent_run_result) = &agent_run_result
    {
        Some(agent_run_result.agent_id.tool.as_str())
    } else {
        None
    };

    if reset {
        debug_log("Working log reset. Starting fresh checkpoint.");
    }

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

    debug_log(&format!(
        "[BENCHMARK] Total checkpoint run took {:?}",
        checkpoint_start.elapsed()
    ));
    Ok((entries.len(), resolved.files.len(), checkpoints.len()))
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_captured_checkpoint(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    reset: bool,
    agent_run_result: Option<&AgentRunResult>,
    is_pre_commit: bool,
    base_commit_override: Option<&str>,
) -> Result<Option<PreparedCheckpointCapture>, GitAiError> {
    let Some((explicit_path_role, _)) = explicit_capture_target_paths(kind, agent_run_result)
    else {
        return Err(GitAiError::Generic(
            "captured checkpoint requires explicit edited_filepaths or will_edit_filepaths"
                .to_string(),
        ));
    };

    let Some(resolved) = resolve_live_checkpoint_execution(
        repo,
        kind,
        agent_run_result,
        is_pre_commit,
        base_commit_override,
    )?
    else {
        return Ok(None);
    };

    if resolved.files.is_empty() && !reset {
        return Ok(None);
    }

    let explicit_paths = filtered_pathspecs_for_agent_run_result(repo, kind, agent_run_result)
        .ok_or_else(|| {
            GitAiError::Generic(
                "captured checkpoint requires explicit in-repository target paths".to_string(),
            )
        })?;

    let capture_id = new_async_checkpoint_capture_id();
    let capture_dir = async_checkpoint_capture_dir(&capture_id)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(capture_dir.join("blobs"))?;

    let mut files = Vec::with_capacity(resolved.files.len());
    for file_path in &resolved.files {
        let source = if let Some(content) = resolved.dirty_files.get(file_path).cloned() {
            PreparedCheckpointFileSource::DirtyFileContent { content }
        } else {
            let content = repo
                .storage
                .working_log_for_base_commit(&resolved.base_commit)
                .read_current_file_content(file_path)?;
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

    let mut stored_agent_run_result = agent_run_result.cloned();
    if let Some(agent_run_result) = stored_agent_run_result.as_mut() {
        agent_run_result.dirty_files = None;
    }

    let manifest = PreparedCheckpointManifest {
        repo_working_dir: repo
            .workdir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        base_commit: resolved.base_commit,
        captured_at_ms: resolved.ts,
        kind,
        author: author.to_string(),
        reset,
        is_pre_commit,
        explicit_path_role,
        explicit_paths,
        files,
        agent_run_result: stored_agent_run_result,
    };
    fs::write(
        async_checkpoint_manifest_path(&capture_id)?,
        serde_json::to_vec(&manifest)?,
    )?;

    Ok(Some(PreparedCheckpointCapture {
        capture_id,
        repo_working_dir: manifest.repo_working_dir,
        file_count: manifest.files.len(),
    }))
}

fn load_captured_checkpoint_manifest(
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

pub fn execute_captured_checkpoint(
    repo: &Repository,
    capture_id: &str,
) -> Result<(usize, usize, usize), GitAiError> {
    let checkpoint_start = Instant::now();
    debug_log("[BENCHMARK] Starting captured checkpoint replay");

    let manifest = load_captured_checkpoint_manifest(capture_id)?;
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
        manifest.reset,
        true,
        manifest.agent_run_result,
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
    debug_log(&format!(
        "[BENCHMARK]   git status call took {:?}",
        status_start.elapsed()
    ));

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
            debug_log(&format!(
                "Skipping INITIAL file outside repository: {}",
                normalized_path
            ));
            continue;
        }
        if should_ignore_file_with_matcher(&normalized_path, ignore_matcher) {
            continue;
        }
        if is_text_file(working_log, &normalized_path) {
            files.insert(normalized_path);
        }
    }
    debug_log(&format!(
        "[BENCHMARK]   Reading INITIAL attributions in get_all_tracked_files took {:?}",
        initial_read_start.elapsed()
    ));

    let checkpoints_read_start = Instant::now();
    if let Ok(working_log_data) = working_log.read_all_checkpoints() {
        for checkpoint in &working_log_data {
            for entry in &checkpoint.entries {
                // Normalize path separators to forward slashes
                let normalized_path = normalize_to_posix(&entry.file);
                // Filter out paths outside the repository to prevent git command failures
                if !is_path_in_repo(&normalized_path) {
                    debug_log(&format!(
                        "Skipping checkpoint file outside repository: {}",
                        normalized_path
                    ));
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
    debug_log(&format!(
        "[BENCHMARK]   Reading checkpoints in get_all_tracked_files took {:?}",
        checkpoints_read_start.elapsed()
    ));

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
    debug_log(&format!(
        "[BENCHMARK]   get_status_of_files in get_all_tracked_files took {:?}",
        status_files_start.elapsed()
    ));

    // Ensure to always include all dirty files
    if let Some(ref dirty_files) = working_log.dirty_files {
        for file_path in dirty_files.keys() {
            // Normalize path separators to forward slashes
            let normalized_path = normalize_to_posix(file_path);
            // Filter out paths outside the repository to prevent git command failures
            if !is_path_in_repo(&normalized_path) {
                debug_log(&format!(
                    "Skipping dirty file outside repository: {}",
                    normalized_path
                ));
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

fn working_log_entry_has_non_human_attribution(entry: &WorkingLogEntry) -> bool {
    entry
        .line_attributions
        .iter()
        .any(|attr| attr.author_id != CheckpointKind::Human.to_str())
        || entry
            .attributions
            .iter()
            .any(|attr| attr.author_id != CheckpointKind::Human.to_str())
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

            if checkpoint.kind != CheckpointKind::Human
                || working_log_entry_has_non_human_attribution(entry)
            {
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
    if is_pre_commit
        && kind == CheckpointKind::Human
        && !has_prior_ai_edits
        && initial_attrs_for_file.is_empty()
    {
        return Ok(None);
    }

    let current_content = working_log
        .read_current_file_content(&file_path)
        .unwrap_or_default();

    // Non-pre-commit fast path:
    // Preserve existing `git-ai checkpoint` behavior for human-only files by writing an
    // attribution-empty entry while still capturing line stats.
    if kind == CheckpointKind::Human && !has_prior_ai_edits && initial_attrs_for_file.is_empty() {
        let previous_content = if let Some(state) = previous_state.as_ref() {
            working_log
                .get_file_version(&state.blob_sha)
                .unwrap_or_default()
        } else {
            get_previous_content_from_head(&repo, &file_path, head_tree_id.as_ref())
        };

        if current_content == previous_content {
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
        if current_content == previous_content && initial_attrs_for_file.is_empty() {
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

        debug_log(&format!(
            "[BENCHMARK] Blame for {} took {:?}",
            file_path,
            blame_start.elapsed()
        ));

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
        if kind != CheckpointKind::Human {
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
    if is_from_checkpoint && current_content == previous_content {
        return Ok(None);
    }

    let (entry, stats) = make_entry_for_file(FileEntryInput {
        file_path: &file_path,
        blob_sha: &file_content_hash,
        author_id: author_id.as_ref(),
        is_ai_checkpoint: kind != CheckpointKind::Human,
        previous_content: &previous_content,
        previous_attributions: &prev_attributions,
        content: &current_content,
        ts,
    })?;
    debug_log(&format!(
        "[BENCHMARK] Processing file {} took {:?}",
        file_path,
        file_start.elapsed()
    ));
    Ok(Some((entry, stats)))
}

#[allow(clippy::too_many_arguments)]
async fn get_checkpoint_entries(
    kind: CheckpointKind,
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    files: &[String],
    file_content_hashes: &HashMap<String, String>,
    previous_checkpoints: &[Checkpoint],
    agent_run_result: Option<&AgentRunResult>,
    ts: u128,
    is_pre_commit: bool,
    head_commit_override: Option<&str>,
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
    debug_log(&format!(
        "[BENCHMARK] Reading initial attributions took {:?}",
        initial_read_start.elapsed()
    ));

    let precompute_start = Instant::now();
    let (previous_file_state_by_file, ai_touched_files) =
        build_previous_file_state_maps(previous_checkpoints, &initial_attributions);
    debug_log(&format!(
        "[BENCHMARK] Precomputing previous state maps took {:?}",
        precompute_start.elapsed()
    ));

    // Determine author_id based on checkpoint kind and agent_id
    let author_id = if kind != CheckpointKind::Human {
        // For AI checkpoints, use session hash
        agent_run_result
            .map(|result| {
                crate::authorship::authorship_log_serialization::generate_short_hash(
                    &result.agent_id.id,
                    &result.agent_id.tool,
                )
            })
            .unwrap_or_else(|| kind.to_str())
    } else {
        // For human checkpoints, use checkpoint kind string
        kind.to_str()
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
    debug_log(&format!(
        "[BENCHMARK] Spawning {} tasks took {:?}",
        tasks.len(),
        spawn_start.elapsed()
    ));

    // Await all tasks concurrently
    let await_start = Instant::now();
    let results = futures::future::join_all(tasks).await;
    debug_log(&format!(
        "[BENCHMARK] Awaiting {} tasks took {:?}",
        results.len(),
        await_start.elapsed()
    ));

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
    debug_log(&format!(
        "[BENCHMARK] Processing {} results took {:?}",
        results_count,
        process_start.elapsed()
    ));
    debug_log(&format!(
        "[BENCHMARK] get_checkpoint_entries function total took {:?}",
        entries_fn_start.elapsed()
    ));

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
    debug_log(&format!(
        "[BENCHMARK]   attribute_unattributed_ranges for {} took {:?}",
        file_path,
        fill_start.elapsed()
    ));

    let update_start = Instant::now();
    let new_attributions = tracker.update_attributions_for_checkpoint(
        previous_content,
        content,
        &filled_in_prev_attributions,
        author_id,
        ts,
        is_ai_checkpoint,
    )?;
    debug_log(&format!(
        "[BENCHMARK]   update_attributions for {} took {:?}",
        file_path,
        update_start.elapsed()
    ));

    // TODO Consider discarding any "uncontentious" attributions for the human author. Any human attributions that do not share a line with any other author's attributions can be discarded.
    // let filtered_attributions = crate::authorship::attribution_tracker::discard_uncontentious_attributions_for_author(&new_attributions, &CheckpointKind::Human.to_str());

    let line_attr_start = Instant::now();
    let line_attributions =
        crate::authorship::attribution_tracker::attributions_to_line_attributions_for_checkpoint(
            &new_attributions,
            content,
            is_ai_checkpoint,
        );
    debug_log(&format!(
        "[BENCHMARK]   attributions_to_line_attributions for {} took {:?}",
        file_path,
        line_attr_start.elapsed()
    ));

    // Compute line stats while we already have both contents in memory
    let stats_start = Instant::now();
    let line_stats = compute_file_line_stats(previous_content, content);
    debug_log(&format!(
        "[BENCHMARK]   compute_file_line_stats for {} took {:?}",
        file_path,
        stats_start.elapsed()
    ));

    let entry = WorkingLogEntry::new(
        file_path.to_string(),
        blob_sha.to_string(),
        new_attributions,
        line_attributions,
    );

    Ok((entry, line_stats))
}

/// Compute line statistics for a single file by diffing previous and current content
fn compute_file_line_stats(previous_content: &str, current_content: &str) -> FileLineStats {
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

/// Upsert a checkpoint prompt to the internal database
fn upsert_checkpoint_prompt_to_db(
    checkpoint: &Checkpoint,
    workdir: String,
    commit_sha: Option<String>,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};

    let record = PromptDbRecord::from_checkpoint(checkpoint, Some(workdir), commit_sha)
        .ok_or_else(|| {
            GitAiError::Generic("Failed to create prompt record from checkpoint".to_string())
        })?;

    let db = InternalDatabase::global()?;
    let mut db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    db_guard.upsert_prompt(&record)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::transcript::AiTranscript;
    use crate::authorship::working_log::AgentId;
    use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
    use crate::git::test_utils::TmpRepo;
    use std::collections::HashMap;

    fn test_agent_run_result(
        checkpoint_kind: CheckpointKind,
        edited_filepaths: Option<Vec<&str>>,
        will_edit_filepaths: Option<Vec<&str>>,
        dirty_files: Option<HashMap<&str, &str>>,
    ) -> AgentRunResult {
        AgentRunResult {
            agent_id: AgentId {
                tool: "test-agent".to_string(),
                id: "test-capture".to_string(),
                model: "test-model".to_string(),
            },
            agent_metadata: None,
            checkpoint_kind,
            transcript: Some(AiTranscript { messages: vec![] }),
            repo_working_dir: None,
            edited_filepaths: edited_filepaths.map(|paths| {
                paths
                    .into_iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            }),
            will_edit_filepaths: will_edit_filepaths.map(|paths| {
                paths
                    .into_iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            }),
            dirty_files: dirty_files.map(|files| {
                files
                    .into_iter()
                    .map(|(path, content)| (path.to_string(), content.to_string()))
                    .collect()
            }),
        }
    }

    #[test]
    fn test_explicit_capture_target_paths_accepts_non_empty_edited_filepaths() {
        let agent_run_result = test_agent_run_result(
            CheckpointKind::AiAgent,
            Some(vec!["src/main.rs"]),
            None,
            None,
        );

        assert_eq!(
            explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&agent_run_result)),
            Some((PreparedPathRole::Edited, vec!["src/main.rs".to_string()]))
        );
    }

    #[test]
    fn test_explicit_capture_target_paths_accepts_non_empty_will_edit_filepaths() {
        let agent_run_result =
            test_agent_run_result(CheckpointKind::Human, None, Some(vec!["src/lib.rs"]), None);

        assert_eq!(
            explicit_capture_target_paths(CheckpointKind::Human, Some(&agent_run_result)),
            Some((PreparedPathRole::WillEdit, vec!["src/lib.rs".to_string()]))
        );
    }

    #[test]
    fn test_explicit_capture_target_paths_rejects_dirty_files_without_explicit_paths() {
        let agent_run_result = test_agent_run_result(
            CheckpointKind::AiAgent,
            None,
            None,
            Some(HashMap::from([("src/main.rs", "fn main() {}\n")])),
        );

        assert_eq!(
            explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&agent_run_result)),
            None
        );
    }

    #[test]
    fn test_explicit_capture_target_paths_rejects_empty_explicit_lists() {
        let human_result =
            test_agent_run_result(CheckpointKind::Human, None, Some(vec!["", "   "]), None);
        let ai_result =
            test_agent_run_result(CheckpointKind::AiAgent, Some(vec!["", "   "]), None, None);

        assert_eq!(
            explicit_capture_target_paths(CheckpointKind::Human, Some(&human_result)),
            None
        );
        assert_eq!(
            explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&ai_result)),
            None
        );
    }

    #[test]
    fn test_checkpoint_with_staged_changes() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make changes to the file
        file.append("New line added by user\n").unwrap();

        // Note: TmpFile.append() automatically stages changes (see write_to_disk in test_utils)
        // So at this point, the file has staged changes

        // Run checkpoint - it should track the changes even though they're staged
        let (entries_len, files_len, _checkpoints_len) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        // The bug: when changes are staged, entries_len is 0 instead of 1
        assert_eq!(files_len, 1, "Should have 1 file with changes");
        assert_eq!(
            entries_len, 1,
            "Should have 1 file entry in checkpoint (staged changes should be tracked)"
        );
    }

    #[test]
    fn test_checkpoint_with_staged_changes_after_previous_checkpoint() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make first changes and checkpoint
        file.append("First change\n").unwrap();
        let (entries_len_1, files_len_1, _) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        assert_eq!(
            files_len_1, 1,
            "First checkpoint: should have 1 file with changes"
        );
        assert_eq!(
            entries_len_1, 1,
            "First checkpoint: should have 1 file entry"
        );

        // Make second changes - these are already staged by append()
        file.append("Second change\n").unwrap();

        // Run checkpoint again - it should track the staged changes even after a previous checkpoint
        let (entries_len_2, files_len_2, _) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        assert_eq!(
            files_len_2, 1,
            "Second checkpoint: should have 1 file with changes"
        );
        assert_eq!(
            entries_len_2, 1,
            "Second checkpoint: should have 1 file entry in checkpoint (staged changes should be tracked)"
        );
    }

    #[test]
    fn test_checkpoint_with_only_staged_no_unstaged_changes() {
        use std::fs;

        // Create a repo with an initial commit
        let (tmp_repo, file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the file path
        let file_path = file.path();
        let filename = file.filename();

        // Manually modify the file (bypassing TmpFile's automatic staging)
        let mut content = fs::read_to_string(&file_path).unwrap();
        content.push_str("New line for staging test\n");
        fs::write(&file_path, &content).unwrap();

        // Now manually stage it using git (this is what "git add" does)
        tmp_repo.stage_file(filename).unwrap();

        // At this point: HEAD has old content, index has new content, workdir has new content
        // And unstaged should be "Unmodified" because workdir == index

        // Now run checkpoint
        let (entries_len, files_len, _checkpoints_len) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        // This should work: we should see 1 file with 1 entry
        assert_eq!(files_len, 1, "Should detect 1 file with staged changes");
        assert_eq!(
            entries_len, 1,
            "Should track the staged changes in checkpoint"
        );
    }

    #[test]
    fn test_checkpoint_with_only_unstaged_changes_for_ai_without_pathspec() {
        use std::fs;

        // Create a repo with an initial commit
        let (tmp_repo, file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Manually modify the file without staging it
        let file_path = file.path();
        let mut content = fs::read_to_string(&file_path).unwrap();
        content.push_str("New unstaged AI line\n");
        fs::write(&file_path, &content).unwrap();

        // Trigger AI checkpoint without edited_filepaths (pathspec-less flow used by some agents)
        let (entries_len, files_len, _checkpoints_len) = tmp_repo
            .trigger_checkpoint_with_ai("Codex", Some("gpt-5-codex"), Some("codex"))
            .unwrap();

        assert_eq!(
            files_len, 1,
            "Should detect unstaged changes without pathspecs"
        );
        assert_eq!(
            entries_len, 1,
            "Should create an AI checkpoint entry for unstaged changes without pathspecs"
        );
    }

    #[test]
    fn test_checkpoint_base_override_controls_head_context_for_entry_generation() {
        use crate::authorship::transcript::AiTranscript;
        use crate::authorship::working_log::AgentId;
        use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
        use std::collections::HashMap;
        use std::fs;

        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();
        let filename = file.filename().to_string();

        file.update("line from commit A\n").unwrap();
        tmp_repo.commit_with_message("commit A").unwrap();
        let base_commit = tmp_repo.get_head_commit_sha().unwrap();

        file.update("line from commit B\n").unwrap();
        tmp_repo.commit_with_message("commit B").unwrap();

        // Keep the worktree dirty so git status returns this file, but inject deterministic
        // content from commit B via dirty_files.
        fs::write(file.path(), "line from uncommitted edit\n").unwrap();

        let mut dirty_files = HashMap::new();
        dirty_files.insert(filename.clone(), "line from commit B\n".to_string());
        let agent_run_result = AgentRunResult {
            agent_id: AgentId {
                tool: "mock_ai".to_string(),
                id: "base-override-regression".to_string(),
                model: "test".to_string(),
            },
            agent_metadata: None,
            transcript: Some(AiTranscript { messages: vec![] }),
            checkpoint_kind: CheckpointKind::AiAgent,
            repo_working_dir: None,
            edited_filepaths: Some(vec![filename]),
            will_edit_filepaths: None,
            dirty_files: Some(dirty_files),
        };

        let (entries_len, files_len, _) = run_with_base_commit_override(
            tmp_repo.gitai_repo(),
            "mock-ai",
            CheckpointKind::AiAgent,
            false,
            true,
            Some(agent_run_result),
            false,
            Some(base_commit.as_str()),
        )
        .unwrap();

        assert_eq!(
            files_len, 1,
            "Expected one tracked file for the checkpoint run"
        );
        assert_eq!(
            entries_len, 1,
            "When base override points to commit A, current content from commit B must produce an entry"
        );
    }

    #[test]
    fn test_checkpoint_skips_conflicted_files() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the current branch name (whatever the default is)
        let base_branch = tmp_repo.current_branch().unwrap();

        // Create a branch and make different changes on each branch to create a conflict
        tmp_repo.create_branch("feature-branch").unwrap();

        // On feature branch, modify the file
        file.append("Feature branch change\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("FeatureUser")
            .unwrap();
        tmp_repo.commit_with_message("Feature commit").unwrap();

        // Switch back to base branch and make conflicting changes
        tmp_repo.switch_branch(&base_branch).unwrap();
        file.append("Main branch change\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("MainUser").unwrap();
        tmp_repo.commit_with_message("Main commit").unwrap();

        // Attempt to merge feature-branch into base branch - this should create a conflict
        let has_conflicts = tmp_repo.merge_with_conflicts("feature-branch").unwrap();
        assert!(has_conflicts, "Should have merge conflicts");

        // Try to checkpoint while there are conflicts
        let (entries_len, files_len, _) = tmp_repo.trigger_checkpoint_with_author("Human").unwrap();

        // Checkpoint should skip conflicted files
        assert_eq!(
            files_len, 0,
            "Should have 0 files (conflicted file should be skipped)"
        );
        assert_eq!(
            entries_len, 0,
            "Should have 0 entries (conflicted file should be skipped)"
        );
    }

    #[test]
    fn test_checkpoint_with_paths_outside_repo() {
        use crate::authorship::transcript::AiTranscript;
        use crate::authorship::working_log::AgentId;
        use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;

        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make changes to the file
        file.append("New line added\n").unwrap();

        // Create agent run result with paths outside the repo
        let agent_run_result = AgentRunResult {
            agent_id: AgentId {
                tool: "test_tool".to_string(),
                id: "test_session".to_string(),
                model: "test_model".to_string(),
            },
            agent_metadata: None,
            transcript: Some(AiTranscript { messages: vec![] }),
            checkpoint_kind: CheckpointKind::AiAgent,
            repo_working_dir: None,
            edited_filepaths: Some(vec![
                "/tmp/outside_file.txt".to_string(),
                "../outside_parent.txt".to_string(),
                file.filename().to_string(), // This one is valid
            ]),
            will_edit_filepaths: None,
            dirty_files: None,
        };

        // Run checkpoint - should not crash even with paths outside repo
        let result =
            tmp_repo.trigger_checkpoint_with_agent_result("test_user", Some(agent_run_result));

        // Should succeed without crashing
        assert!(
            result.is_ok(),
            "Checkpoint should succeed even with paths outside repo: {:?}",
            result.err()
        );

        let (entries_len, files_len, _) = result.unwrap();
        // Should only process the valid file
        assert_eq!(files_len, 1, "Should process 1 valid file");
        assert_eq!(entries_len, 1, "Should create 1 entry");
    }

    #[test]
    fn test_checkpoint_filters_external_paths_from_stored_checkpoints() {
        use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};

        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get access to the working log storage
        let repo =
            crate::git::repository::find_repository_in_path(tmp_repo.path().to_str().unwrap())
                .expect("Repository should exist");
        let base_commit = repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());

        // Manually inject a checkpoint with an external file path (simulating the bug)
        // This is what happens when a file outside the repo was tracked before the fix
        let working_log = repo.storage.working_log_for_base_commit(&base_commit);

        let external_entry = WorkingLogEntry::new(
            "/external/path/outside/repo.txt".to_string(),
            "fake_sha_for_external".to_string(),
            vec![],
            vec![],
        );

        let fake_checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            "fake_diff".to_string(),
            "test_author".to_string(),
            vec![external_entry],
        );

        // Store the checkpoint with external path
        working_log
            .append_checkpoint(&fake_checkpoint)
            .expect("Should be able to append checkpoint");

        // Now make actual changes to a file in the repo
        file.append("New line for testing\n").unwrap();

        // Run checkpoint - this should NOT crash even though there's an external path stored
        // Previously this would fail with: "fatal: /external/path/outside/repo.txt is outside repository"
        let result = tmp_repo.trigger_checkpoint_with_author("Human");

        assert!(
            result.is_ok(),
            "Checkpoint should succeed even with external paths stored in previous checkpoints: {:?}",
            result.err()
        );

        let (entries_len, files_len, _) = result.unwrap();
        // Should only process the valid file in the repo
        assert_eq!(
            files_len, 1,
            "Should process 1 valid file (external path should be filtered)"
        );
        assert_eq!(entries_len, 1, "Should create 1 entry for the in-repo file");
    }

    #[test]
    fn test_checkpoint_works_after_conflict_resolution_maintains_authorship() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the current branch name (whatever the default is)
        let base_branch = tmp_repo.current_branch().unwrap();

        // Checkpoint initial state to track the base authorship
        let file_path = file.path();
        let initial_content = std::fs::read_to_string(&file_path).unwrap();
        println!("Initial content:\n{}", initial_content);

        // Create a branch and make changes
        tmp_repo.create_branch("feature-branch").unwrap();
        file.append("Feature line 1\n").unwrap();
        file.append("Feature line 2\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("AI_Agent").unwrap();
        tmp_repo.commit_with_message("Feature commit").unwrap();

        // Switch back to base branch and make conflicting changes
        tmp_repo.switch_branch(&base_branch).unwrap();
        file.append("Main line 1\n").unwrap();
        file.append("Main line 2\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("Human").unwrap();
        tmp_repo.commit_with_message("Main commit").unwrap();

        // Attempt to merge feature-branch into base branch - this should create a conflict
        let has_conflicts = tmp_repo.merge_with_conflicts("feature-branch").unwrap();
        assert!(has_conflicts, "Should have merge conflicts");

        // While there are conflicts, checkpoint should skip the file
        let (entries_len_conflict, files_len_conflict, _) =
            tmp_repo.trigger_checkpoint_with_author("Human").unwrap();
        assert_eq!(
            files_len_conflict, 0,
            "Should skip conflicted files during conflict"
        );
        assert_eq!(
            entries_len_conflict, 0,
            "Should not create entries for conflicted files"
        );

        // Resolve the conflict by choosing "ours" (base branch)
        tmp_repo.resolve_conflict(file.filename(), "ours").unwrap();

        // Verify content to ensure the resolution was applied correctly
        let resolved_content = std::fs::read_to_string(&file_path).unwrap();
        println!("Resolved content after resolution:\n{}", resolved_content);
        assert!(
            resolved_content.contains("Main line 1"),
            "Should contain base branch content (we chose 'ours')"
        );
        assert!(
            resolved_content.contains("Main line 2"),
            "Should contain base branch content (we chose 'ours')"
        );
        assert!(
            !resolved_content.contains("Feature line 1"),
            "Should not contain feature branch content (we chose 'ours')"
        );

        // After resolution, make additional changes to test that checkpointing works again
        file.append("Post-resolution line 1\n").unwrap();
        file.append("Post-resolution line 2\n").unwrap();

        // Now checkpoint should work and track the new changes
        let (entries_len_after, files_len_after, _) =
            tmp_repo.trigger_checkpoint_with_author("Human").unwrap();

        println!(
            "After resolution and new changes: entries_len={}, files_len={}",
            entries_len_after, files_len_after
        );

        // The file should be tracked with the new changes
        assert_eq!(
            files_len_after, 1,
            "Should detect 1 file with new changes after conflict resolution"
        );
        assert_eq!(
            entries_len_after, 1,
            "Should create 1 entry for new changes after conflict resolution"
        );
    }

    #[test]
    fn test_human_checkpoint_without_ai_history_uses_empty_attributions() {
        let repo = TmpRepo::new().unwrap();
        let mut file = repo.write_file("simple.txt", "one\n", true).unwrap();

        repo.trigger_checkpoint_with_author("seed").unwrap();
        repo.commit_with_message("seed commit").unwrap();

        file.append("two\n").unwrap();
        repo.trigger_checkpoint_with_author("human").unwrap();

        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo.path().to_str().unwrap())
                .expect("Repository should exist");
        let base_commit = gitai_repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = gitai_repo.storage.working_log_for_base_commit(&base_commit);
        let checkpoints = working_log.read_all_checkpoints().unwrap();
        let latest = checkpoints.last().unwrap();
        let entry = latest
            .entries
            .iter()
            .find(|entry| entry.file == "simple.txt")
            .unwrap();

        assert!(
            entry.attributions.is_empty(),
            "Human-only file should skip char-level attribution generation"
        );
        assert!(
            entry.line_attributions.is_empty(),
            "Human-only file should skip line-level attribution generation"
        );
        assert!(
            latest.line_stats.additions > 0,
            "Fast path should still record line stats"
        );
    }

    #[test]
    fn test_human_checkpoint_keeps_attributions_for_ai_touched_file() {
        let (repo, mut lines_file, mut alphabet_file) = TmpRepo::new_with_base_commit().unwrap();

        lines_file.append("ai change\n").unwrap();
        repo.trigger_checkpoint_with_ai("mock_ai", None, None)
            .unwrap();

        lines_file.append("human after ai\n").unwrap();
        alphabet_file.append("human only\n").unwrap();
        repo.trigger_checkpoint_with_author("human").unwrap();

        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo.path().to_str().unwrap())
                .expect("Repository should exist");
        let base_commit = gitai_repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = gitai_repo.storage.working_log_for_base_commit(&base_commit);
        let checkpoints = working_log.read_all_checkpoints().unwrap();
        let latest = checkpoints.last().unwrap();

        let ai_touched_entry = latest
            .entries
            .iter()
            .find(|entry| entry.file == "lines.md")
            .unwrap();
        assert!(
            !ai_touched_entry.attributions.is_empty()
                || !ai_touched_entry.line_attributions.is_empty(),
            "AI-touched file should keep attribution tracking"
        );

        let human_only_entry = latest
            .entries
            .iter()
            .find(|entry| entry.file == "alphabet.md")
            .unwrap();
        assert!(
            human_only_entry.attributions.is_empty(),
            "Human-only file should use fast path with empty char attributions"
        );
        assert!(
            human_only_entry.line_attributions.is_empty(),
            "Human-only file should use fast path with empty line attributions"
        );
    }

    #[test]
    fn test_checkpoint_skips_default_ignored_files() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("README.md", "# repo\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        std::fs::write(repo.path().join("README.md"), "# repo\n\nupdated\n").unwrap();
        std::fs::write(repo.path().join("Cargo.lock"), "# lock\n# lock2\n").unwrap();

        repo.trigger_checkpoint_with_author("human").unwrap();

        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo.path().to_str().unwrap())
                .expect("Repository should exist");
        let base_commit = gitai_repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = gitai_repo.storage.working_log_for_base_commit(&base_commit);
        let checkpoints = working_log.read_all_checkpoints().unwrap();
        let latest = checkpoints.last().unwrap();

        assert!(
            latest.entries.iter().any(|entry| entry.file == "README.md"),
            "Expected non-ignored source file to be checkpointed"
        );
        assert!(
            latest
                .entries
                .iter()
                .all(|entry| entry.file != "Cargo.lock"),
            "Expected Cargo.lock to be filtered by default ignore patterns"
        );
    }

    #[test]
    fn test_checkpoint_skips_linguist_generated_files_from_root_gitattributes() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("README.md", "# repo\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        repo.write_file(".gitattributes", "generated/** linguist-generated\n", true)
            .unwrap();
        repo.commit_with_message("attrs").unwrap();

        std::fs::create_dir_all(repo.path().join("generated")).unwrap();
        std::fs::write(
            repo.path().join("generated").join("api.generated.ts"),
            "// generated\n// generated 2\n",
        )
        .unwrap();
        repo.write_file("main.rs", "fn main() {}\n", false).unwrap();

        repo.trigger_checkpoint_with_author("human").unwrap();

        let gitai_repo =
            crate::git::repository::find_repository_in_path(repo.path().to_str().unwrap())
                .expect("Repository should exist");
        let base_commit = gitai_repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = gitai_repo.storage.working_log_for_base_commit(&base_commit);
        let checkpoints = working_log.read_all_checkpoints().unwrap();
        let latest = checkpoints.last().unwrap();

        assert!(
            latest.entries.iter().any(|entry| entry.file == "main.rs"),
            "Expected non-generated file to be checkpointed"
        );
        assert!(
            latest
                .entries
                .iter()
                .all(|entry| entry.file != "generated/api.generated.ts"),
            "Expected linguist-generated file to be filtered via .gitattributes"
        );
    }

    #[test]
    fn test_compute_line_stats_ignores_whitespace_only_lines() {
        let (tmp_repo, _lines_file, _alphabet_file) = TmpRepo::new_with_base_commit().unwrap();

        let repo =
            crate::git::repository::find_repository_in_path(tmp_repo.path().to_str().unwrap())
                .expect("Repository should exist");

        let base_commit = repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = repo.storage.working_log_for_base_commit(&base_commit);

        let mut test_file = tmp_repo
            .write_file("whitespace.txt", "Seed line\n", true)
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Setup")
            .expect("Setup checkpoint should succeed");

        test_file
            .append("\n\n   \nVisible line one\n\n\t\nVisible line two\n  \n")
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Aidan")
            .expect("First checkpoint should succeed");

        let after_add_stats = working_log
            .read_all_checkpoints()
            .expect("Should read checkpoints after addition");
        let after_add_last = after_add_stats
            .last()
            .expect("At least one checkpoint expected")
            .line_stats
            .clone();

        assert_eq!(
            after_add_last.additions, 8,
            "Additions includes empty lines"
        );
        assert_eq!(after_add_last.deletions, 0, "No deletions expected yet");
        assert_eq!(
            after_add_last.additions_sloc, 2,
            "Only visible lines counted"
        );
        assert_eq!(
            after_add_last.deletions_sloc, 0,
            "No deletions expected yet"
        );

        let cleaned_content = std::fs::read_to_string(test_file.path()).unwrap();
        let cleaned_lines: Vec<&str> = cleaned_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let cleaned_body = format!("{}\n", cleaned_lines.join("\n"));
        test_file.update(&cleaned_body).unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Aidan")
            .expect("Second checkpoint should succeed");

        let after_delete_stats = working_log
            .read_all_checkpoints()
            .expect("Should read checkpoints after deletion");
        let latest_stats = after_delete_stats
            .last()
            .expect("At least one checkpoint expected")
            .line_stats
            .clone();

        assert_eq!(
            latest_stats.additions, 0,
            "No additions in cleanup checkpoint"
        );
        assert_eq!(latest_stats.deletions, 6, "Deletions includes empty lines");
        assert_eq!(
            latest_stats.additions_sloc, 0,
            "No additions in cleanup checkpoint"
        );
        assert_eq!(
            latest_stats.deletions_sloc, 0,
            "Whitespace deletions ignored"
        );
    }
}
