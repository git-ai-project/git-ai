use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::checkpoint_content_budget::CheckpointContentBudget;
use crate::commands::checkpoint_agent::presets::{
    KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    StreamSource, UntrackedEdit,
};
use crate::config;
use crate::daemon::checkpoint::PreparedPathRole;
use crate::error::GitAiError;
use crate::git::repo_state::{read_head_state_for_worktree, worktree_root_for_path};
use crate::git::repository::discover_repository_in_path_no_git_exec;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BaseCommit {
    Sha(String),
    Initial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,
    pub content: Option<String>,
    pub repo_work_dir: PathBuf,
    pub base_commit: BaseCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub stream_source: Option<StreamSource>,
    pub metadata: HashMap<String, String>,
}

#[derive(Serialize)]
struct CheckpointDebugLogEntry<'a> {
    timestamp: String,
    preset_name: &'a str,
    hook_input: &'a str,
    trace_id: &'a str,
    event_count: usize,
    requests: &'a [CheckpointRequest],
}

struct RepoContext {
    repo_work_dir: PathBuf,
    base_commit: BaseCommit,
}

const MAX_CHECKPOINT_FILES: usize = 1000;
const MAX_CHECKPOINT_REPOSITORIES: usize = 64;
const MAX_CHECKPOINT_CONTEXT_STRING_BYTES: usize = 64 * 1024;
const MAX_CHECKPOINT_METADATA_ITEMS: usize = 64;
const MAX_CHECKPOINT_METADATA_KEY_BYTES: usize = 1024;
const MAX_CHECKPOINT_METADATA_VALUE_BYTES: usize = 64 * 1024;
const MAX_CHECKPOINT_METADATA_BYTES: usize = 256 * 1024;
const MAX_CHECKPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CHECKPOINT_HOOK_INPUT_BYTES: usize = 32 * 1024 * 1024;

fn agent_identity_is_bounded(agent: &AgentId) -> bool {
    [agent.tool.as_str(), agent.id.as_str(), agent.model.as_str()]
        .into_iter()
        .all(|value| value.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES)
}

fn stream_source_is_bounded(source: &StreamSource) -> bool {
    source.path.to_string_lossy().len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && source.session_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && source.external_session_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && source
            .external_parent_session_id
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES)
}

fn metadata_is_bounded(metadata: &HashMap<String, String>) -> bool {
    if metadata.len() > MAX_CHECKPOINT_METADATA_ITEMS {
        return false;
    }
    let mut total_bytes = 0usize;
    metadata.iter().all(|(key, value)| {
        total_bytes = total_bytes
            .saturating_add(key.len())
            .saturating_add(value.len());
        key.len() <= MAX_CHECKPOINT_METADATA_KEY_BYTES
            && value.len() <= MAX_CHECKPOINT_METADATA_VALUE_BYTES
            && total_bytes <= MAX_CHECKPOINT_METADATA_BYTES
    })
}

fn preset_context_is_bounded(
    context: &crate::commands::checkpoint_agent::presets::PresetContext,
) -> bool {
    agent_identity_is_bounded(&context.agent_id)
        && context.external_session_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && context.trace_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && context.cwd.to_string_lossy().len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        && metadata_is_bounded(&context.metadata)
}

fn parsed_hook_event_is_bounded(event: &ParsedHookEvent) -> bool {
    match event {
        ParsedHookEvent::PreFileEdit(event) => {
            preset_context_is_bounded(&event.context)
                && event
                    .tool_use_id
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES)
        }
        ParsedHookEvent::PostFileEdit(event) => {
            preset_context_is_bounded(&event.context)
                && event
                    .tool_use_id
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES)
                && event
                    .stream_source
                    .as_ref()
                    .is_none_or(stream_source_is_bounded)
        }
        ParsedHookEvent::PreBashCall(event) => {
            preset_context_is_bounded(&event.context)
                && event.tool_use_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
                && event
                    .command
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_CHECKPOINT_COMMAND_BYTES)
        }
        ParsedHookEvent::PostBashCall(event) => {
            preset_context_is_bounded(&event.context)
                && event.tool_use_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
                && event
                    .command
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_CHECKPOINT_COMMAND_BYTES)
                && event
                    .stream_source
                    .as_ref()
                    .is_none_or(stream_source_is_bounded)
        }
        ParsedHookEvent::KnownHumanEdit(event) => {
            event.trace_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
                && event.cwd.to_string_lossy().len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
                && metadata_is_bounded(&event.editor_metadata)
        }
        ParsedHookEvent::UntrackedEdit(event) => {
            event.trace_id.len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
                && event.cwd.to_string_lossy().len() <= MAX_CHECKPOINT_CONTEXT_STRING_BYTES
        }
    }
}

fn apply_dirty_file_overrides(
    files: &mut [CheckpointFile],
    dirty_files: &HashMap<PathBuf, String>,
) {
    let mut budget = CheckpointContentBudget::from_config(config::Config::get());
    for file in files {
        if let Some(override_content) = dirty_files.get(&file.path) {
            file.content = if budget.reserve(file.path.display(), override_content) {
                Some(override_content.clone())
            } else {
                None
            };
        } else if file
            .content
            .as_ref()
            .is_some_and(|content| !budget.reserve(file.path.display(), content))
        {
            file.content = None;
        }
    }
}

fn build_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    let perf = std::env::var("GIT_AI_DEBUG_PERFORMANCE").is_ok_and(|v| !v.is_empty() && v != "0");

    if file_paths.len() > MAX_CHECKPOINT_FILES {
        tracing::warn!(
            "build_checkpoint_files called with {} paths (max {}); truncating",
            file_paths.len(),
            MAX_CHECKPOINT_FILES,
        );
    }
    let mut repo_cache: HashMap<PathBuf, RepoContext> = HashMap::new();
    let mut files = Vec::new();
    let mut content_budget = CheckpointContentBudget::from_config(config::Config::get());
    let max_size = content_budget.max_file_size_bytes();
    let mut seen_paths = HashSet::new();

    for path in file_paths.iter().take(MAX_CHECKPOINT_FILES) {
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }

        let ctx = {
            let t_discover = std::time::Instant::now();
            let repo_work_dir = worktree_root_for_path(path).ok_or_else(|| {
                GitAiError::Generic(format!(
                    "No git repository found for path: {}",
                    path.display()
                ))
            })?;
            if !repo_cache.contains_key(&repo_work_dir) {
                let t_head = std::time::Instant::now();
                let base_commit = match read_head_state_for_worktree(&repo_work_dir) {
                    Some(state) => match state.head {
                        Some(sha) => BaseCommit::Sha(sha),
                        None => BaseCommit::Initial,
                    },
                    None => BaseCommit::Initial,
                };
                let head_ms = t_head.elapsed().as_secs_f64() * 1000.0;

                if perf {
                    eprintln!(
                        "[perf] build_checkpoint_files: discover={:.1}ms head={:.1}ms (repo={})",
                        t_discover.elapsed().as_secs_f64() * 1000.0,
                        head_ms,
                        repo_work_dir.display(),
                    );
                }

                let key = repo_work_dir.clone();
                repo_cache.insert(
                    key,
                    RepoContext {
                        repo_work_dir: repo_work_dir.clone(),
                        base_commit,
                    },
                );
            }
            repo_cache.get(&repo_work_dir).unwrap()
        };

        let t_read = std::time::Instant::now();
        let content = if let Ok(meta) = fs::metadata(path) {
            if meta.len() > max_size as u64 {
                tracing::warn!(
                    "skipping file larger than max_checkpoint_file_size_bytes: {} ({} bytes)",
                    path.display(),
                    meta.len(),
                );
                continue;
            }
            crate::utils::read_text_file_with_limit(path, max_size as u64, "checkpoint file").ok()
        } else {
            Some(String::new())
        };
        if perf {
            eprintln!(
                "[perf] build_checkpoint_files: read_file={:.1}ms (path={}, size={})",
                t_read.elapsed().as_secs_f64() * 1000.0,
                path.display(),
                content.as_ref().map(|c| c.len()).unwrap_or(0),
            );
        }

        let content = content.filter(|content| content_budget.reserve(path.display(), content));

        files.push(CheckpointFile {
            path: path.clone(),
            content,
            repo_work_dir: ctx.repo_work_dir.clone(),
            base_commit: ctx.base_commit.clone(),
        });
    }

    Ok(files)
}

pub fn execute_preset_checkpoint(
    preset_name: &str,
    hook_input: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    if hook_input.len() > MAX_CHECKPOINT_HOOK_INPUT_BYTES {
        return Err(GitAiError::PresetError(format!(
            "hook input exceeded the {} byte limit ({})",
            MAX_CHECKPOINT_HOOK_INPUT_BYTES,
            hook_input.len()
        )));
    }
    let perf = std::env::var("GIT_AI_DEBUG_PERFORMANCE").is_ok_and(|v| !v.is_empty() && v != "0");
    let t0 = std::time::Instant::now();

    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;
    let events_len = events.len();

    if perf {
        eprintln!(
            "[perf] orchestrator: parse={:.1}ms (events={})",
            t0.elapsed().as_secs_f64() * 1000.0,
            events_len,
        );
    }

    let mut requests = Vec::new();
    for event in events {
        if !parsed_hook_event_is_bounded(&event) {
            tracing::warn!("skipping checkpoint event with oversized context");
            continue;
        }
        let t_event = std::time::Instant::now();
        let event_name = format!("{:?}", std::mem::discriminant(&event));
        let new_requests = execute_event(event, preset_name)?;
        if perf {
            eprintln!(
                "[perf] orchestrator: execute_event({})={:.1}ms (requests={})",
                event_name,
                t_event.elapsed().as_secs_f64() * 1000.0,
                new_requests.len(),
            );
        }
        requests.extend(new_requests);
    }

    if config::Config::get()
        .get_feature_flags()
        .checkpoint_debug_log
    {
        write_checkpoint_debug_log(preset_name, hook_input, &trace_id, events_len, &requests);
    }

    Ok(requests)
}

fn write_checkpoint_debug_log(
    preset_name: &str,
    hook_input: &str,
    trace_id: &str,
    event_count: usize,
    requests: &[CheckpointRequest],
) {
    let Some(internal_dir) = config::internal_dir_path() else {
        return;
    };

    let log_dir = internal_dir.join("checkpoint-debug-logs");
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let log_path = log_dir.join(format!("{}.log", date));

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("[checkpoint_debug_log] failed to create dir: {}", e);
        return;
    }

    cleanup_old_debug_logs(&log_dir);

    let entry = CheckpointDebugLogEntry {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        preset_name,
        hook_input,
        trace_id,
        event_count,
        requests,
    };

    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };

    let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    else {
        return;
    };

    let _ = file
        .write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.flush());
}

fn cleanup_old_debug_logs(log_dir: &Path) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };

    let cutoff = chrono::Utc::now() - chrono::Duration::days(14);

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(file_date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d")
            && file_date < cutoff.date_naive()
        {
            let _ = fs::remove_file(&path);
        }
    }
}

fn execute_event(
    event: ParsedHookEvent,
    preset_name: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    match event {
        ParsedHookEvent::PreFileEdit(e) => execute_pre_file_edit(e),
        ParsedHookEvent::PostFileEdit(e) => execute_post_file_edit(e, preset_name),
        ParsedHookEvent::PreBashCall(e) => execute_pre_bash_call(e),
        ParsedHookEvent::PostBashCall(e) => execute_post_bash_call(e),
        ParsedHookEvent::KnownHumanEdit(e) => execute_known_human_edit(e),
        ParsedHookEvent::UntrackedEdit(e) => execute_untracked_edit(e),
    }
}

fn split_files_into_requests(
    all_files: Vec<CheckpointFile>,
    trace_id: String,
    checkpoint_kind: CheckpointKind,
    agent_id: Option<AgentId>,
    path_role: PreparedPathRole,
    stream_source: Option<StreamSource>,
    metadata: HashMap<String, String>,
) -> Vec<CheckpointRequest> {
    if agent_id
        .as_ref()
        .is_some_and(|agent| !agent_identity_is_bounded(agent))
    {
        tracing::warn!("skipping checkpoint with oversized agent identity");
        return Vec::new();
    }
    if stream_source
        .as_ref()
        .is_some_and(|source| !stream_source_is_bounded(source))
    {
        tracing::warn!("skipping checkpoint with oversized stream source");
        return Vec::new();
    }

    let mut bounded_metadata = HashMap::new();
    let mut metadata_bytes = 0usize;
    for (key, value) in metadata {
        let item_bytes = key.len().saturating_add(value.len());
        if bounded_metadata.len() >= MAX_CHECKPOINT_METADATA_ITEMS
            || key.len() > MAX_CHECKPOINT_METADATA_KEY_BYTES
            || value.len() > MAX_CHECKPOINT_METADATA_VALUE_BYTES
            || metadata_bytes.saturating_add(item_bytes) > MAX_CHECKPOINT_METADATA_BYTES
        {
            tracing::warn!("dropping oversized checkpoint metadata item");
            continue;
        }
        metadata_bytes += item_bytes;
        bounded_metadata.insert(key, value);
    }

    let all_files: Vec<CheckpointFile> = all_files
        .into_iter()
        .filter(|f| f.content.is_some())
        .collect();
    let mut by_repo: HashMap<PathBuf, Vec<CheckpointFile>> = HashMap::new();
    for f in all_files {
        if let Some(repo_files) = by_repo.get_mut(&f.repo_work_dir) {
            repo_files.push(f);
        } else if by_repo.len() < MAX_CHECKPOINT_REPOSITORIES {
            by_repo.insert(f.repo_work_dir.clone(), vec![f]);
        } else {
            tracing::warn!(
                max_repositories = MAX_CHECKPOINT_REPOSITORIES,
                "dropping checkpoint file beyond repository fanout limit"
            );
        }
    }

    by_repo
        .into_values()
        .map(|files| CheckpointRequest {
            trace_id: trace_id.clone(),
            checkpoint_kind,
            agent_id: agent_id.clone(),
            files,
            path_role,
            stream_source: stream_source.clone(),
            metadata: bounded_metadata.clone(),
        })
        .collect()
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    let mut metadata = e.context.metadata;
    if let Some(tuid) = e.tool_use_id {
        metadata.entry("tool_use_id".to_string()).or_insert(tuid);
    }
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::Human,
        Some(e.context.agent_id),
        PreparedPathRole::WillEdit,
        None,
        metadata,
    ))
}

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    let checkpoint_kind = match preset_name {
        "ai_tab" => CheckpointKind::AiTab,
        _ => CheckpointKind::AiAgent,
    };
    let mut metadata = e.context.metadata;
    if let Some(tuid) = e.tool_use_id {
        metadata.entry("tool_use_id".to_string()).or_insert(tuid);
    }
    metadata
        .entry("edit_kind".to_string())
        .or_insert_with(|| "file_edit".to_string());
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        checkpoint_kind,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.stream_source,
        metadata,
    ))
}

fn execute_known_human_edit(e: KnownHumanEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        apply_dirty_file_overrides(&mut files, dirty);
    }
    Ok(split_files_into_requests(
        files,
        e.trace_id,
        CheckpointKind::KnownHuman,
        None,
        PreparedPathRole::Edited,
        None,
        e.editor_metadata,
    ))
}

fn execute_untracked_edit(e: UntrackedEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;
    Ok(split_files_into_requests(
        files,
        e.trace_id,
        CheckpointKind::Human,
        None,
        PreparedPathRole::WillEdit,
        None,
        HashMap::new(),
    ))
}

fn execute_pre_bash_call(e: PreBashCall) -> Result<Vec<CheckpointRequest>, GitAiError> {
    use crate::commands::checkpoint_agent::bash_tool::{
        self, BashHookAttemptPhase, BashHookAttemptSignal,
    };

    let started_at_ns = crate::daemon::bash_history_db::unix_time_ns();
    let repo_work_dir = match discover_repository_in_path_no_git_exec(e.context.cwd.as_path())
        .and_then(|repo| repo.workdir())
    {
        Ok(repo_work_dir) => repo_work_dir,
        Err(error) => {
            let error_message = error.to_string();
            bash_tool::signal_daemon_bash_hook_attempt(
                BashHookAttemptPhase::Start,
                BashHookAttemptSignal {
                    original_cwd: e.context.cwd.as_path(),
                    discovered_repo_work_dir: None,
                    repo_discovery_error: Some(&error_message),
                    session_id: &e.context.external_session_id,
                    tool_use_id: &e.tool_use_id,
                    agent_id: &e.context.agent_id,
                    metadata: &e.context.metadata,
                    trace_id: &e.context.trace_id,
                    timestamp_ns: started_at_ns,
                    command: e.command.as_deref(),
                },
            );
            return Ok(vec![]);
        }
    };

    let dirty_paths = match bash_tool::handle_bash_pre_tool_use_with_context_and_cwd(
        &repo_work_dir,
        e.context.cwd.as_path(),
        bash_tool::BashToolHookContext {
            session_id: &e.context.external_session_id,
            tool_use_id: &e.tool_use_id,
            agent_id: &e.context.agent_id,
            agent_metadata: Some(&e.context.metadata),
            trace_id: &e.context.trace_id,
            command: e.command.as_deref(),
        },
    ) {
        Ok(result) => result.dirty_paths,
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.external_session_id,
                error
            );
            return Ok(vec![]);
        }
    };

    if dirty_paths.is_empty() {
        return Ok(vec![]);
    }

    let files = build_checkpoint_files(&dirty_paths)?;
    let mut metadata = e.context.metadata;
    metadata
        .entry("tool_use_id".to_string())
        .or_insert(e.tool_use_id);
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::Human,
        None,
        PreparedPathRole::WillEdit,
        None,
        metadata,
    ))
}

fn execute_post_bash_call(e: PostBashCall) -> Result<Vec<CheckpointRequest>, GitAiError> {
    use crate::commands::checkpoint_agent::bash_tool::{
        self, BashHookAttemptPhase, BashHookAttemptSignal,
    };

    let ended_at_ns = crate::daemon::bash_history_db::unix_time_ns();
    let repo_work_dir = match discover_repository_in_path_no_git_exec(e.context.cwd.as_path())
        .and_then(|repo| repo.workdir())
    {
        Ok(repo_work_dir) => repo_work_dir,
        Err(error) => {
            let error_message = error.to_string();
            bash_tool::signal_daemon_bash_hook_attempt(
                BashHookAttemptPhase::End,
                BashHookAttemptSignal {
                    original_cwd: e.context.cwd.as_path(),
                    discovered_repo_work_dir: None,
                    repo_discovery_error: Some(&error_message),
                    session_id: &e.context.external_session_id,
                    tool_use_id: &e.tool_use_id,
                    agent_id: &e.context.agent_id,
                    metadata: &e.context.metadata,
                    trace_id: &e.context.trace_id,
                    timestamp_ns: ended_at_ns,
                    command: e.command.as_deref(),
                },
            );
            return Ok(vec![]);
        }
    };

    let bash_result = bash_tool::handle_bash_post_tool_use_with_cwd(
        &repo_work_dir,
        e.context.cwd.as_path(),
        bash_tool::BashToolHookContext {
            session_id: &e.context.external_session_id,
            tool_use_id: &e.tool_use_id,
            agent_id: &e.context.agent_id,
            agent_metadata: Some(&e.context.metadata),
            trace_id: &e.context.trace_id,
            command: e.command.as_deref(),
        },
    );

    let file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => match &result.action {
            bash_tool::BashCheckpointAction::Checkpoint(paths) => paths
                .iter()
                .map(|p| {
                    let joined = repo_work_dir.join(p);
                    fs::canonicalize(&joined).unwrap_or(joined)
                })
                .collect(),
            _ => vec![],
        },
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            vec![]
        }
    };

    let files = build_checkpoint_files(&file_paths)?;
    let mut metadata = e.context.metadata;
    metadata
        .entry("tool_use_id".to_string())
        .or_insert(e.tool_use_id);
    metadata
        .entry("edit_kind".to_string())
        .or_insert_with(|| "bash".to_string());
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::AiAgent,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.stream_source,
        metadata,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint_file(repo: &str) -> CheckpointFile {
        CheckpointFile {
            path: PathBuf::from(format!("/{repo}/file.txt")),
            content: Some("content".to_string()),
            repo_work_dir: PathBuf::from(format!("/{repo}")),
            base_commit: BaseCommit::Initial,
        }
    }

    fn split_for_test(
        files: Vec<CheckpointFile>,
        agent_id: Option<AgentId>,
        metadata: HashMap<String, String>,
    ) -> Vec<CheckpointRequest> {
        split_files_into_requests(
            files,
            "trace".to_string(),
            CheckpointKind::AiAgent,
            agent_id,
            PreparedPathRole::Edited,
            None,
            metadata,
        )
    }

    #[test]
    fn split_checkpoint_requests_bounds_repository_fanout() {
        let files = (0..65)
            .map(|index| checkpoint_file(&format!("repo-{index}")))
            .collect();

        assert_eq!(split_for_test(files, None, HashMap::new()).len(), 64);
    }

    #[test]
    fn split_checkpoint_requests_rejects_oversized_agent_identity() {
        let agent_id = AgentId {
            tool: "tool".to_string(),
            id: "x".repeat(64 * 1024 + 1),
            model: "model".to_string(),
        };

        assert!(
            split_for_test(
                vec![checkpoint_file("repo")],
                Some(agent_id),
                HashMap::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn split_checkpoint_requests_drops_oversized_metadata_values() {
        let metadata = HashMap::from([("command".to_string(), "x".repeat(64 * 1024 + 1))]);
        let requests = split_for_test(vec![checkpoint_file("repo")], None, metadata);

        assert_eq!(requests.len(), 1);
        assert!(!requests[0].metadata.contains_key("command"));
    }
}
