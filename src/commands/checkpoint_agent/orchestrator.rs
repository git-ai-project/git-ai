use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::bash_tool::{self, HookEvent};
use crate::commands::checkpoint_agent::presets::{
    BashPreHookStrategy, KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, TranscriptSource, UntrackedEdit,
};
use crate::error::GitAiError;
use crate::git::repository::find_repository_for_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFileEntry {
    pub path: PathBuf,
    pub content: String,
    pub repo_work_dir: PathBuf,
    pub base_commit_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFileEntry>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

fn build_file_entries(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFileEntry>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(vec![]);
    }
    // Cache repo lookups — files in the same repo share work_dir and head
    let mut repo_cache: HashMap<PathBuf, (PathBuf, String)> = HashMap::new();
    let mut entries = Vec::with_capacity(file_paths.len());

    for path in file_paths {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }
        let repo = find_repository_for_file(&path.to_string_lossy(), None)?;
        let work_dir = repo.workdir()?;
        let (repo_work_dir, base_commit_sha) = repo_cache
            .entry(work_dir.clone())
            .or_insert_with(|| {
                let head = repo.rev_parse_head().unwrap_or_default();
                (work_dir, head)
            })
            .clone();

        let content = fs::read_to_string(path).unwrap_or_default();
        entries.push(CheckpointFileEntry {
            path: path.clone(),
            content,
            repo_work_dir,
            base_commit_sha,
        });
    }
    Ok(entries)
}

fn build_file_entries_with_content(
    files_with_content: &[(PathBuf, String)],
) -> Result<Vec<CheckpointFileEntry>, GitAiError> {
    if files_with_content.is_empty() {
        return Ok(vec![]);
    }
    let mut repo_cache: HashMap<PathBuf, (PathBuf, String)> = HashMap::new();
    let mut entries = Vec::with_capacity(files_with_content.len());

    for (path, content) in files_with_content {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }
        let repo = find_repository_for_file(&path.to_string_lossy(), None)?;
        let work_dir = repo.workdir()?;
        let (repo_work_dir, base_commit_sha) = repo_cache
            .entry(work_dir.clone())
            .or_insert_with(|| {
                let head = repo.rev_parse_head().unwrap_or_default();
                (work_dir, head)
            })
            .clone();

        entries.push(CheckpointFileEntry {
            path: path.clone(),
            content: content.clone(),
            repo_work_dir,
            base_commit_sha,
        });
    }
    Ok(entries)
}

pub fn execute_preset_checkpoint(
    preset_name: &str,
    hook_input: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;

    events
        .into_iter()
        .map(|event| execute_event(event, preset_name))
        .collect::<Result<Vec<_>, _>>()
        .map(|v| v.into_iter().flatten().collect())
}

fn execute_event(
    event: ParsedHookEvent,
    preset_name: &str,
) -> Result<Option<CheckpointRequest>, GitAiError> {
    match event {
        ParsedHookEvent::PreFileEdit(e) => execute_pre_file_edit(e).map(Some),
        ParsedHookEvent::PostFileEdit(e) => execute_post_file_edit(e, preset_name).map(Some),
        ParsedHookEvent::PreBashCall(e) => execute_pre_bash_call(e),
        ParsedHookEvent::PostBashCall(e) => execute_post_bash_call(e).map(Some),
        ParsedHookEvent::KnownHumanEdit(e) => execute_known_human_edit(e).map(Some),
        ParsedHookEvent::UntrackedEdit(e) => execute_untracked_edit(e).map(Some),
    }
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files,
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: e.context.metadata,
    })
}

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
    let checkpoint_kind = match preset_name {
        "ai_tab" => CheckpointKind::AiTab,
        _ => CheckpointKind::AiAgent,
    };
    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind,
        agent_id: Some(e.context.agent_id),
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
    })
}

fn execute_known_human_edit(e: KnownHumanEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
    Ok(CheckpointRequest {
        trace_id: e.trace_id,
        checkpoint_kind: CheckpointKind::KnownHuman,
        agent_id: None,
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: None,
        metadata: e.editor_metadata,
    })
}

fn execute_untracked_edit(e: UntrackedEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
    Ok(CheckpointRequest {
        trace_id: e.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files,
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: HashMap::new(),
    })
}

fn execute_pre_bash_call(e: PreBashCall) -> Result<Option<CheckpointRequest>, GitAiError> {
    let repo_working_dir = resolve_repo_working_dir_from_cwd(&e.context.cwd)?;

    let captured_checkpoint_id = match bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_working_dir,
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        Ok(result) => result.captured_checkpoint.map(|info| info.capture_id),
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.session_id,
                error
            );
            None
        }
    };

    match e.strategy {
        BashPreHookStrategy::EmitHumanCheckpoint => Ok(Some(CheckpointRequest {
            trace_id: e.context.trace_id,
            checkpoint_kind: CheckpointKind::Human,
            agent_id: None,
            repo_working_dir,
            file_paths: vec![],
            path_role: PreparedPathRole::WillEdit,
            dirty_files: None,
            transcript_source: None,
            metadata: e.context.metadata,
            captured_checkpoint_id,
        })),
        BashPreHookStrategy::SnapshotOnly => Ok(None),
    }
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    let repo_working_dir = resolve_repo_working_dir_from_cwd(&e.context.cwd)?;

    let bash_result = bash_tool::handle_bash_tool(
        HookEvent::PostToolUse,
        &repo_working_dir,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let (file_paths, captured_checkpoint_id) = match &bash_result {
        Ok(result) => {
            let paths = match &result.action {
                bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                    paths.iter().map(PathBuf::from).collect()
                }
                bash_tool::BashCheckpointAction::NoChanges => vec![],
                bash_tool::BashCheckpointAction::Fallback => vec![],
                bash_tool::BashCheckpointAction::TakePreSnapshot => vec![],
            };
            let cap_id = result
                .captured_checkpoint
                .as_ref()
                .map(|info| info.capture_id.clone());
            (paths, cap_id)
        }
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            (vec![], None)
        }
    };

    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(e.context.agent_id),
        repo_working_dir,
        file_paths,
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
        captured_checkpoint_id,
    })
}
