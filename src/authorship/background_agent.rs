use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use std::collections::HashMap;
use std::path::PathBuf;

const DEVIN_ID_PATH: &str = "/opt/.devin/devin_id";
const DEVIN_DIR_PATH: &str = "/opt/.devin";
const CODEX_INTERNAL_ORIGINATOR_OVERRIDE: &str = "CODEX_INTERNAL_ORIGINATOR_OVERRIDE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundAgent {
    BackgroundAgentWithHooks { tool: String },
    BackgroundAgentNoHooks { tool: String, id: String },
    NotInBackgroundAgent,
}

/// Returns the background-agent environment we're running in, if any.
pub fn detect_background_agent() -> BackgroundAgent {
    if std::env::var("CLAUDE_CODE_REMOTE")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        return BackgroundAgent::BackgroundAgentWithHooks {
            tool: "claude-web".to_string(),
        };
    }

    if std::env::var("CURSOR_AGENT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return BackgroundAgent::BackgroundAgentWithHooks {
            tool: "cursor-agent".to_string(),
        };
    }

    if std::env::vars().any(|(k, _)| k.starts_with("CLOUD_AGENT_")) {
        return BackgroundAgent::BackgroundAgentNoHooks {
            tool: "cloud-agent".to_string(),
            id: placeholder("CLOUD_AGENT"),
        };
    }

    if std::path::Path::new(DEVIN_DIR_PATH).is_dir() {
        let id = std::fs::read_to_string(DEVIN_ID_PATH)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| placeholder("DEVIN"));
        return BackgroundAgent::BackgroundAgentNoHooks {
            tool: "devin".to_string(),
            id,
        };
    }

    if std::env::var(CODEX_INTERNAL_ORIGINATOR_OVERRIDE)
        .map(|v| v == "codex_web_agent")
        .unwrap_or(false)
    {
        let id = std::env::var("CODEX_THREAD_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| placeholder("CODEX_CLOUD"));
        return BackgroundAgent::BackgroundAgentNoHooks {
            tool: "codex-cloud".to_string(),
            id,
        };
    }

    if std::env::var("GIT_AI_CLOUD_AGENT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return BackgroundAgent::BackgroundAgentNoHooks {
            tool: "git-ai-cloud-agent".to_string(),
            id: placeholder("GIT_AI_CLOUD_AGENT"),
        };
    }

    BackgroundAgent::NotInBackgroundAgent
}

/// Returns true if the process is running inside a background AI agent environment.
pub fn is_in_background_agent() -> bool {
    !matches!(
        detect_background_agent(),
        BackgroundAgent::NotInBackgroundAgent
    )
}

/// If we're running inside a `BackgroundAgentNoHooks` environment, build a
/// synthetic AI `CheckpointRequest` that callers can hand to
/// `checkpoint::run` so the resulting commit gets attributed wholly to the
/// detected tool.
///
/// Returns `None` for `BackgroundAgentWithHooks` (those agents fire their
/// own checkpoints) and for `NotInBackgroundAgent`.
///
/// Callers may overlay `file_paths` / `dirty_files` on the returned request
/// before passing it through — the daemon path supplies a precomputed diff
/// snapshot, while the wrapper path leaves them empty and lets
/// `checkpoint::run` discover dirty files itself.
pub fn synthetic_ai_checkpoint_request_for_no_hooks_agent(
    repo_working_dir: PathBuf,
) -> Option<(AgentId, CheckpointRequest)> {
    let BackgroundAgent::BackgroundAgentNoHooks { tool, id } = detect_background_agent() else {
        return None;
    };

    let agent_id = AgentId {
        tool,
        id,
        model: "unknown".to_string(),
    };

    let request = CheckpointRequest {
        trace_id: generate_trace_id(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(agent_id.clone()),
        repo_working_dir,
        file_paths: vec![],
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    Some((agent_id, request))
}

fn placeholder(name: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{name}_SESSION{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_agent_env_vars() {
        unsafe {
            std::env::remove_var("CLAUDE_CODE_REMOTE");
            std::env::remove_var("CURSOR_AGENT");
            std::env::remove_var(CODEX_INTERNAL_ORIGINATOR_OVERRIDE);
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("GIT_AI_CLOUD_AGENT");
            for (k, _) in std::env::vars() {
                if k.starts_with("CLOUD_AGENT_") {
                    std::env::remove_var(&k);
                }
            }
        }
    }

    #[test]
    #[serial]
    fn synthetic_request_returns_none_when_not_in_agent() {
        clear_agent_env_vars();
        let result = synthetic_ai_checkpoint_request_for_no_hooks_agent(PathBuf::from("/tmp"));
        assert!(result.is_none());
    }

    #[test]
    #[serial]
    fn synthetic_request_returns_none_for_with_hooks_agents() {
        clear_agent_env_vars();
        unsafe {
            std::env::set_var("CLAUDE_CODE_REMOTE", "true");
        }
        let result = synthetic_ai_checkpoint_request_for_no_hooks_agent(PathBuf::from("/tmp"));
        clear_agent_env_vars();
        assert!(
            result.is_none(),
            "BackgroundAgentWithHooks must not synthesise a checkpoint"
        );
    }

    #[test]
    #[serial]
    fn synthetic_request_for_codex_cloud() {
        clear_agent_env_vars();
        unsafe {
            std::env::set_var(CODEX_INTERNAL_ORIGINATOR_OVERRIDE, "codex_web_agent");
            std::env::set_var("CODEX_THREAD_ID", "thread-abc-123");
        }
        let result = synthetic_ai_checkpoint_request_for_no_hooks_agent(PathBuf::from("/tmp/repo"));
        clear_agent_env_vars();

        let (agent_id, _) = result.expect("should synthesise an AI request");
        assert_eq!(agent_id.tool, "codex-cloud");
        assert_eq!(agent_id.id, "thread-abc-123");
    }
}
