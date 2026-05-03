use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::presets::{
    KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, TranscriptSource, UntrackedEdit,
};
use crate::error::GitAiError;
use crate::git::repository::discover_repository_in_path_no_git_exec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
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
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}

fn resolve_file_context(path: &Path) -> Result<(PathBuf, BaseCommit), GitAiError> {
    let repo = discover_repository_in_path_no_git_exec(path)?;
    let repo_work_dir = repo.workdir()?;
    let base_commit = match repo.head() {
        Ok(head) => match head.target() {
            Ok(sha) => BaseCommit::Sha(sha),
            Err(_) => BaseCommit::Initial,
        },
        Err(_) => BaseCommit::Initial,
    };
    Ok((repo_work_dir, base_commit))
}

fn build_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    let mut repo_cache: HashMap<PathBuf, (PathBuf, BaseCommit)> = HashMap::new();

    file_paths
        .iter()
        .map(|path| {
            if !path.is_absolute() {
                return Err(GitAiError::PresetError(format!(
                    "file path must be absolute: {}",
                    path.display()
                )));
            }

            let (repo_work_dir, base_commit) = {
                let mut found = None;
                for (cached_dir, cached) in &repo_cache {
                    if path.starts_with(cached_dir) {
                        found = Some(cached.clone());
                        break;
                    }
                }
                match found {
                    Some(cached) => cached,
                    None => {
                        let resolved = resolve_file_context(path)?;
                        repo_cache.insert(resolved.0.clone(), resolved.clone());
                        resolved
                    }
                }
            };

            let content = fs::read_to_string(path).ok();

            Ok(CheckpointFile {
                path: path.clone(),
                content,
                repo_work_dir,
                base_commit,
            })
        })
        .collect()
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
    let files = build_checkpoint_files(&e.file_paths)?;

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
    let files = build_checkpoint_files(&e.file_paths)?;

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
    let files = build_checkpoint_files(&e.file_paths)?;

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
    let files = build_checkpoint_files(&e.file_paths)?;

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
    use crate::commands::checkpoint_agent::bash_tool;

    let repo = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?;
    let repo_work_dir = repo.workdir()?;

    match bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        Ok(_) => Ok(None),
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.session_id,
                error
            );
            Ok(None)
        }
    }
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    use crate::commands::checkpoint_agent::bash_tool;

    let repo = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?;
    let repo_work_dir = repo.workdir()?;

    let bash_result = bash_tool::handle_bash_tool(
        bash_tool::HookEvent::PostToolUse,
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => match &result.action {
            bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                paths.iter().map(|p| repo_work_dir.join(p)).collect()
            }
            _ => vec![],
        },
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            vec![]
        }
    };

    let files = build_checkpoint_files(&file_paths)?;

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
