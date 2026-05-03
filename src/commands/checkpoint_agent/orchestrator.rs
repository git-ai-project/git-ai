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

fn discover_dirty_files_in_repo(repo_work_dir: &std::path::Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(repo_work_dir)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let status = &line[..2];
            if status.starts_with('D') || status == " D" {
                return None;
            }
            let path_part = &line[3..];
            let path_part = if let Some(arrow_pos) = path_part.find(" -> ") {
                &path_part[arrow_pos + 4..]
            } else {
                path_part
            };
            let abs = repo_work_dir.join(path_part);
            if abs.exists() {
                Some(abs.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect()
}

fn resolve_head_sha(repo: &Repository) -> String {
    repo.head().and_then(|r| r.target()).unwrap_or_default()
}

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

fn unmerged_paths_in_repo(repo_work_dir: &std::path::Path) -> std::collections::HashSet<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--unmerged", "--full-name"])
        .current_dir(repo_work_dir)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashSet::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // Format: "<mode> <hash> <stage>\t<path>"
            let path_part = line.split('\t').nth(1)?;
            Some(repo_work_dir.join(path_part))
        })
        .collect()
}

fn build_checkpoint_files(
    file_paths: &[PathBuf],
    default_repo: &Repository,
) -> Result<Vec<CheckpointFile>, GitAiError> {
    validate_absolute_paths(file_paths)?;
    let default_work_dir = default_repo.workdir()?;
    let default_sha = resolve_head_sha(default_repo);
    let mut files = Vec::with_capacity(file_paths.len());
    let mut unmerged_cache: HashMap<PathBuf, std::collections::HashSet<PathBuf>> = HashMap::new();
    for path in file_paths {
        let (repo_work_dir, base_commit_sha) =
            match find_repository_for_file(&path.to_string_lossy(), None) {
                Ok(file_repo) => {
                    let wd = file_repo
                        .workdir()
                        .unwrap_or_else(|_| default_work_dir.clone());
                    let sha = resolve_head_sha(&file_repo);
                    (wd, sha)
                }
                Err(_) => (default_work_dir.clone(), default_sha.clone()),
            };
        let unmerged = unmerged_cache
            .entry(repo_work_dir.clone())
            .or_insert_with(|| unmerged_paths_in_repo(&repo_work_dir));
        if unmerged.contains(path) {
            continue;
        }
        let content = if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(s) if !s.contains('\0') => s,
                _ => continue,
            }
        } else {
            String::new()
        };
        files.push(CheckpointFile {
            path: path.clone(),
            content,
            repo_work_dir,
            base_commit_sha,
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

    let dirty_paths = discover_dirty_files_in_repo(&repo_work_dir);
    if dirty_paths.is_empty() {
        return Ok(None);
    }
    let file_paths: Vec<PathBuf> = dirty_paths.into_iter().map(PathBuf::from).collect();
    let files = build_checkpoint_files(&file_paths, &repo)?;

    Ok(Some(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files,
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

    let base_commit_sha = resolve_head_sha(&repo);
    let files: Vec<CheckpointFile> = file_paths
        .iter()
        .filter_map(|rel_path| {
            let abs_path = repo_work_dir.join(rel_path);
            let content = std::fs::read_to_string(&abs_path).ok()?;
            if content.contains('\0') {
                return None;
            }
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
