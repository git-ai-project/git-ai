use crate::{
    authorship::working_log::AgentId,
    commands::checkpoint_agent::bash_tool::{self},
    error::GitAiError,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BashPreHookStrategy {
    EmitHumanCheckpoint,
    SnapshotOnly,
}

pub(crate) enum BashPreHookResult {
    EmitHumanCheckpoint {
        captured_checkpoint_id: Option<String>,
    },
    SkipCheckpoint,
}

pub(crate) fn prepare_agent_bash_pre_hook(
    is_bash_tool: bool,
    repo_working_dir: Option<&str>,
    session_id: &str,
    tool_use_id: &str,
    agent_id: &AgentId,
    agent_metadata: Option<&HashMap<String, String>>,
    strategy: BashPreHookStrategy,
) -> Result<BashPreHookResult, GitAiError> {
    let captured_checkpoint_id = if is_bash_tool {
        if let Some(cwd) = repo_working_dir {
            match bash_tool::handle_bash_pre_tool_use_with_context(
                Path::new(cwd),
                session_id,
                tool_use_id,
                agent_id,
                agent_metadata,
            ) {
                Ok(result) => result.captured_checkpoint.map(|info| info.capture_id),
                Err(error) => {
                    tracing::debug!(
                        "Bash pre-hook snapshot failed for {} session {}: {}",
                        agent_id.tool,
                        session_id,
                        error
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(match strategy {
        BashPreHookStrategy::EmitHumanCheckpoint => BashPreHookResult::EmitHumanCheckpoint {
            captured_checkpoint_id,
        },
        BashPreHookStrategy::SnapshotOnly => BashPreHookResult::SkipCheckpoint,
    })
}

/// Check if a file path refers to a Claude plan file.
///
/// Claude plans are written under `~/.claude/plans/`. We treat a path as a plan
/// file only when it:
/// - ends with `.md` (case-insensitive), and
/// - contains the path segment pair `.claude/plans` (platform-aware separators).
pub fn is_plan_file_path(file_path: &str) -> bool {
    let path = Path::new(file_path);
    let is_markdown = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !is_markdown {
        false
    } else {
        let components: Vec<String> = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(segment) => Some(segment.to_string_lossy().to_ascii_lowercase()),
                _ => None,
            })
            .collect();

        components
            .windows(2)
            .any(|window| window[0] == ".claude" && window[1] == "plans")
    }
}

/// Extract plan content from a Write or Edit tool_use input if it targets a plan file.
///
/// Maintains a running `plan_states` map keyed by file path so that Edit operations
/// can reconstruct the full plan text (not just the replaced fragment). On Write the
/// full content is stored; on Edit the old_string->new_string replacement is applied
/// to the tracked state and the complete result is returned.
///
/// Returns None if this is not a plan file edit.
pub fn extract_plan_from_tool_use(
    tool_name: &str,
    input: &serde_json::Value,
    plan_states: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    match tool_name {
        "Write" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let content = input.get("content")?.as_str()?;
            if content.trim().is_empty() {
                return None;
            }
            plan_states.insert(file_path.to_string(), content.to_string());
            Some(content.to_string())
        }
        "Edit" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let old_string = input.get("old_string").and_then(|v| v.as_str());
            let new_string = input.get("new_string").and_then(|v| v.as_str());

            match (old_string, new_string) {
                (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                    // Apply the replacement to the tracked plan state if available
                    if let Some(current) = plan_states.get(file_path) {
                        let updated = current.replacen(old, new, 1);
                        plan_states.insert(file_path.to_string(), updated.clone());
                        Some(updated)
                    } else {
                        // No prior state tracked -- store what we can and return the fragment
                        plan_states.insert(file_path.to_string(), new.to_string());
                        Some(new.to_string())
                    }
                }
                (None, Some(new)) if !new.is_empty() => {
                    plan_states.insert(file_path.to_string(), new.to_string());
                    Some(new.to_string())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_agent_bash_pre_hook_swallows_snapshot_errors() {
        let temp = tempfile::tempdir().unwrap();
        let missing_repo = temp.path().join("missing-repo");
        let agent_id = AgentId {
            tool: "codex".to_string(),
            id: "session-1".to_string(),
            model: "gpt-5.4".to_string(),
        };

        let result = prepare_agent_bash_pre_hook(
            true,
            Some(missing_repo.to_string_lossy().as_ref()),
            "session-1",
            "tool-1",
            &agent_id,
            None,
            BashPreHookStrategy::EmitHumanCheckpoint,
        )
        .expect("pre-hook helper should treat snapshot failures as best-effort");

        match result {
            BashPreHookResult::EmitHumanCheckpoint {
                captured_checkpoint_id,
            } => {
                assert!(
                    captured_checkpoint_id.is_none(),
                    "failed pre-hook snapshot should not produce a captured checkpoint"
                );
            }
            BashPreHookResult::SkipCheckpoint => {
                panic!("expected EmitHumanCheckpoint result");
            }
        }
    }
}
