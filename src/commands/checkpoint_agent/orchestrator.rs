use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::presets::{
    KnownHumanEdit, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    TranscriptSource, UntrackedEdit,
};
use crate::error::GitAiError;
use crate::git::repository::discover_repository_in_path_no_git_exec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

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

struct RepoContext {
    repo_work_dir: PathBuf,
    base_commit: BaseCommit,
    unmerged_paths: std::collections::HashSet<PathBuf>,
}

fn build_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    let mut repo_cache: HashMap<PathBuf, RepoContext> = HashMap::new();
    let mut files = Vec::new();

    for path in file_paths {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }

        let ctx = {
            let repo = discover_repository_in_path_no_git_exec(path)?;
            let repo_work_dir = repo.workdir()?;
            if !repo_cache.contains_key(&repo_work_dir) {
                let base_commit = match repo.head() {
                    Ok(head) => match head.target() {
                        Ok(sha) => BaseCommit::Sha(sha),
                        Err(_) => BaseCommit::Initial,
                    },
                    Err(_) => BaseCommit::Initial,
                };
                let unmerged_paths = repo.get_unmerged_paths().unwrap_or_default();
                let key = repo_work_dir.clone();
                repo_cache.insert(
                    key,
                    RepoContext {
                        repo_work_dir: repo_work_dir.clone(),
                        base_commit,
                        unmerged_paths,
                    },
                );
            }
            repo_cache.get(&repo_work_dir).unwrap()
        };

        if ctx.unmerged_paths.contains(path) {
            continue;
        }

        let content = fs::read_to_string(path).ok();

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
    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;

    let mut requests = Vec::new();
    for event in events {
        requests.extend(execute_event(event, preset_name)?);
    }
    Ok(requests)
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
    transcript_source: Option<TranscriptSource>,
    metadata: HashMap<String, String>,
) -> Vec<CheckpointRequest> {
    let mut by_repo: HashMap<PathBuf, Vec<CheckpointFile>> = HashMap::new();
    for f in all_files {
        by_repo.entry(f.repo_work_dir.clone()).or_default().push(f);
    }

    by_repo
        .into_values()
        .map(|files| CheckpointRequest {
            trace_id: trace_id.clone(),
            checkpoint_kind,
            agent_id: agent_id.clone(),
            files,
            path_role,
            transcript_source: transcript_source.clone(),
            metadata: metadata.clone(),
        })
        .collect()
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let mut files = build_checkpoint_files(&e.file_paths)?;
    if let Some(ref dirty) = e.dirty_files {
        for f in &mut files {
            if let Some(override_content) = dirty.get(&f.path) {
                f.content = Some(override_content.clone());
            }
        }
    }
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::Human,
        None,
        PreparedPathRole::WillEdit,
        None,
        e.context.metadata,
    ))
}

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;
    let checkpoint_kind = match preset_name {
        "ai_tab" => CheckpointKind::AiTab,
        _ => CheckpointKind::AiAgent,
    };
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        checkpoint_kind,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.transcript_source,
        e.context.metadata,
    ))
}

fn execute_known_human_edit(e: KnownHumanEdit) -> Result<Vec<CheckpointRequest>, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;
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
        Ok(_) => Ok(vec![]),
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.session_id,
                error
            );
            Ok(vec![])
        }
    }
}

fn execute_post_bash_call(e: PostBashCall) -> Result<Vec<CheckpointRequest>, GitAiError> {
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
    Ok(split_files_into_requests(
        files,
        e.context.trace_id,
        CheckpointKind::AiAgent,
        Some(e.context.agent_id),
        PreparedPathRole::Edited,
        e.transcript_source,
        e.context.metadata,
    ))
}
