use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::bash_tool::{self, HookEvent};
use crate::commands::checkpoint_agent::presets::{
    KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    TranscriptSource, UntrackedEdit,
};
use crate::error::GitAiError;
use crate::git::repository::{Repository, find_repository_for_file};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
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
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

fn find_repository_for_file_paths(file_paths: &[PathBuf]) -> Result<Repository, GitAiError> {
    let first_path = file_paths.first().ok_or_else(|| {
        GitAiError::PresetError("No file paths provided for repo discovery".to_string())
    })?;
    find_repository_for_file(&first_path.to_string_lossy(), None)
}

fn find_repository_for_cwd(cwd: &std::path::Path) -> Result<Repository, GitAiError> {
    find_repository_for_file(&cwd.to_string_lossy(), None)
}

fn validate_absolute_paths(paths: &[PathBuf]) -> Result<(), GitAiError> {
    for path in paths {
        if !path.is_absolute() {
            return Err(GitAiError::Generic(format!(
                "Checkpoint requires absolute file paths, got relative: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn build_checkpoint_files(
    file_paths: &[PathBuf],
    repo: &Repository,
) -> Result<Vec<CheckpointFile>, GitAiError> {
    validate_absolute_paths(file_paths)?;
    let repo_work_dir = repo.workdir()?;
    let base_commit_sha = repo.head()?.target()?;
    let mut files = Vec::with_capacity(file_paths.len());
    for path in file_paths {
        let content = if path.exists() {
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };
        files.push(CheckpointFile {
            path: path.clone(),
            content,
            repo_work_dir: repo_work_dir.clone(),
            base_commit_sha: base_commit_sha.clone(),
        });
    }
    Ok(files)
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
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.context.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.context.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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
    let repo = find_repository_for_cwd(&e.context.cwd)?;
    let repo_work_dir = repo.workdir()?;

    let _pre_result = match bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        Ok(result) => result,
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.session_id,
                error
            );
            return Ok(None);
        }
    };

    // Always emit a human checkpoint for the pre-hook
    Ok(Some(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files: vec![],
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: e.context.metadata,
    }))
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    let repo = find_repository_for_cwd(&e.context.cwd)?;
    let repo_work_dir = repo.workdir()?;

    let bash_result = bash_tool::handle_bash_tool(
        HookEvent::PostToolUse,
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => match &result.action {
            bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                paths.iter().map(PathBuf::from).collect()
            }
            _ => vec![],
        },
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            vec![]
        }
    };

    let base_commit_sha = repo.head()?.target()?;
    let files: Vec<CheckpointFile> = file_paths
        .iter()
        .filter_map(|rel_path| {
            let abs_path = repo_work_dir.join(rel_path);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            Some(CheckpointFile {
                path: abs_path,
                content,
                repo_work_dir: repo_work_dir.clone(),
                base_commit_sha: base_commit_sha.clone(),
            })
        })
        .collect();

    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(e.context.agent_id),
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
    })
}
