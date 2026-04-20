use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::commands::checkpoint_agent::bash_tool::{self, HookEvent};
use crate::commands::checkpoint_agent::presets::{
    ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit, TranscriptSource,
};
use crate::commands::checkpoint_agent::transcript_readers;
use crate::error::GitAiError;
use crate::git::repository::find_repository_for_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResult {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: AgentId,
    pub repo_working_dir: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub path_role: PreparedPathRole,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
    pub captured_checkpoint_id: Option<String>,
}

impl CheckpointResult {
    /// Convert a `CheckpointResult` into the legacy `AgentRunResult` used by
    /// the existing checkpoint pipeline in `git_ai_handlers.rs`.
    /// This is a temporary bridge until the downstream code is migrated to
    /// consume `CheckpointResult` directly.
    pub fn into_agent_run_result(mut self) -> AgentRunResult {
        // Read transcript from source (if any) and extract model
        let (transcript, transcript_model) = self
            .transcript_source
            .as_ref()
            .and_then(|src| transcript_readers::read_transcript(src).ok())
            .unwrap_or_default();

        // If the preset set model to "unknown", prefer the transcript-extracted model
        if self.agent_id.model == "unknown"
            && let Some(model) = transcript_model
        {
            self.agent_id.model = model;
        }

        let (edited_filepaths, will_edit_filepaths) = match self.path_role {
            PreparedPathRole::Edited => {
                let paths: Vec<String> = self
                    .file_paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                (if paths.is_empty() { None } else { Some(paths) }, None)
            }
            PreparedPathRole::WillEdit => {
                let paths: Vec<String> = self
                    .file_paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                (None, if paths.is_empty() { None } else { Some(paths) })
            }
        };

        let dirty_files = self.dirty_files.map(|df| {
            df.into_iter()
                .map(|(k, v)| (k.to_string_lossy().to_string(), v))
                .collect()
        });

        let transcript_opt = if transcript.messages().is_empty() {
            None
        } else {
            Some(transcript)
        };

        AgentRunResult {
            agent_id: self.agent_id,
            agent_metadata: if self.metadata.is_empty() {
                None
            } else {
                Some(self.metadata)
            },
            checkpoint_kind: self.checkpoint_kind,
            transcript: transcript_opt,
            repo_working_dir: Some(self.repo_working_dir.to_string_lossy().to_string()),
            edited_filepaths,
            will_edit_filepaths,
            dirty_files,
            captured_checkpoint_id: self.captured_checkpoint_id,
        }
    }
}

pub fn execute_preset_checkpoint(
    preset_name: &str,
    hook_input: &str,
) -> Result<Vec<CheckpointResult>, GitAiError> {
    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;

    events
        .into_iter()
        .map(|event| execute_event(event, preset_name))
        .collect()
}

fn resolve_repo_working_dir_from_file_paths(file_paths: &[PathBuf]) -> Result<PathBuf, GitAiError> {
    let first_path = file_paths.first().ok_or_else(|| {
        GitAiError::PresetError("No file paths provided for repo discovery".to_string())
    })?;
    let repo = find_repository_for_file(&first_path.to_string_lossy(), None)?;
    repo.workdir()
}

fn resolve_repo_working_dir_from_cwd(cwd: &std::path::Path) -> Result<PathBuf, GitAiError> {
    let repo = find_repository_for_file(&cwd.to_string_lossy(), None)?;
    repo.workdir()
}

fn execute_event(
    event: ParsedHookEvent,
    preset_name: &str,
) -> Result<CheckpointResult, GitAiError> {
    match event {
        ParsedHookEvent::PreFileEdit(e) => execute_pre_file_edit(e),
        ParsedHookEvent::PostFileEdit(e) => execute_post_file_edit(e, preset_name),
        ParsedHookEvent::PreBashCall(e) => execute_pre_bash_call(e),
        ParsedHookEvent::PostBashCall(e) => execute_post_bash_call(e),
    }
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = if !e.file_paths.is_empty() {
        resolve_repo_working_dir_from_file_paths(&e.file_paths)?
    } else {
        resolve_repo_working_dir_from_cwd(&e.context.cwd)?
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths: e.file_paths,
        path_role: PreparedPathRole::WillEdit,
        dirty_files: e.dirty_files,
        transcript_source: None,
        metadata: e.context.metadata,
        captured_checkpoint_id: None,
    })
}

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = if !e.file_paths.is_empty() {
        resolve_repo_working_dir_from_file_paths(&e.file_paths)?
    } else {
        resolve_repo_working_dir_from_cwd(&e.context.cwd)?
    };

    let checkpoint_kind = if preset_name == "ai_tab" {
        CheckpointKind::AiTab
    } else {
        CheckpointKind::AiAgent
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths: e.file_paths,
        path_role: PreparedPathRole::Edited,
        dirty_files: e.dirty_files,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
        captured_checkpoint_id: None,
    })
}

fn execute_pre_bash_call(e: PreBashCall) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = resolve_repo_working_dir_from_cwd(&e.context.cwd)?;

    let pre_hook_result = super::agent_presets::prepare_agent_bash_pre_hook(
        true,
        Some(&repo_working_dir.to_string_lossy()),
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
        e.strategy,
    )?;

    match pre_hook_result {
        super::agent_presets::BashPreHookResult::EmitHumanCheckpoint {
            captured_checkpoint_id,
        } => Ok(CheckpointResult {
            trace_id: e.context.trace_id,
            checkpoint_kind: CheckpointKind::Human,
            agent_id: e.context.agent_id,
            repo_working_dir,
            file_paths: vec![],
            path_role: PreparedPathRole::WillEdit,
            dirty_files: None,
            transcript_source: None,
            metadata: e.context.metadata,
            captured_checkpoint_id,
        }),
        super::agent_presets::BashPreHookResult::SkipCheckpoint { .. } => {
            // SnapshotOnly strategy: the bash pre-hook already took a snapshot
            // but we should NOT emit a Human checkpoint downstream. Return an
            // error so the caller skips checkpoint processing (matches old preset
            // behavior where the old code returned early/exited).
            Err(GitAiError::PresetError(
                "PreBashCall with SnapshotOnly strategy: checkpoint skipped".to_string(),
            ))
        }
    }
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointResult, GitAiError> {
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

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths,
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
        captured_checkpoint_id,
    })
}
