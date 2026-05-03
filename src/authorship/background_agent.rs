use crate::utils::is_interactive_terminal;

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

    if is_interactive_terminal() {
        return BackgroundAgent::NotInBackgroundAgent;
    }

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
    !matches!(detect_background_agent(), BackgroundAgent::NotInBackgroundAgent)
}

fn placeholder(name: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{name}_SESSION{ts}")
}