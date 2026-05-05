use crate::authorship::authorship_log::SessionRecord;
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, generate_session_id, generate_trace_id,
};
use crate::authorship::working_log::AgentId;
use std::collections::HashMap;

const DEVIN_ID_PATH: &str = "/opt/.devin/devin_id";
const DEVIN_DIR_PATH: &str = "/opt/.devin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundAgent {
    WithHooks { tool: String },
    NoHooks { tool: String, id: String },
    None,
}

pub fn detect() -> BackgroundAgent {
    if std::env::var("CLAUDE_CODE_REMOTE")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        return BackgroundAgent::WithHooks {
            tool: "claude-web".to_string(),
        };
    }

    if std::env::var("CURSOR_AGENT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return BackgroundAgent::WithHooks {
            tool: "cursor-agent".to_string(),
        };
    }

    if std::env::vars().any(|(k, _)| k.starts_with("CLOUD_AGENT_")) {
        return BackgroundAgent::NoHooks {
            tool: "cloud-agent".to_string(),
            id: placeholder_id("CLOUD_AGENT"),
        };
    }

    if std::path::Path::new(DEVIN_DIR_PATH).is_dir() {
        let id = std::fs::read_to_string(DEVIN_ID_PATH)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| placeholder_id("DEVIN"));
        return BackgroundAgent::NoHooks {
            tool: "devin".to_string(),
            id,
        };
    }

    if std::env::var("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
        .map(|v| v == "codex_web_agent")
        .unwrap_or(false)
    {
        let id = std::env::var("CODEX_THREAD_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| placeholder_id("CODEX_CLOUD"));
        return BackgroundAgent::NoHooks {
            tool: "codex-cloud".to_string(),
            id,
        };
    }

    if std::env::var("GIT_AI_CLOUD_AGENT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return BackgroundAgent::NoHooks {
            tool: "git-ai-cloud-agent".to_string(),
            id: placeholder_id("GIT_AI_CLOUD_AGENT"),
        };
    }

    BackgroundAgent::None
}

/// If running in a no-hooks background agent with an empty authorship log (no
/// checkpoints were fired), blanket-attribute all committed lines to the agent.
/// Returns true if attribution was applied.
pub fn apply_blanket_attribution(
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<crate::authorship::authorship_log::LineRange>>,
    human_author: &str,
) -> bool {
    let BackgroundAgent::NoHooks { tool, id } = detect() else {
        return false;
    };

    if committed_hunks.is_empty() {
        return false;
    }

    let agent_id = AgentId {
        tool: tool.clone(),
        id: id.clone(),
        model: "unknown".to_string(),
    };

    let session_key = generate_session_id(&id, &tool);
    let trace_id = generate_trace_id();
    let attestation_hash = format!("{}::{}", session_key, trace_id);

    authorship_log.metadata.sessions.insert(
        session_key,
        SessionRecord {
            agent_id,
            human_author: Some(human_author.to_string()),
            custom_attributes: None,
        },
    );

    for (file_path, line_ranges) in committed_hunks {
        if line_ranges.is_empty() {
            continue;
        }
        let file_attestation = authorship_log.get_or_create_file(file_path);
        file_attestation.add_entry(AttestationEntry::new(
            attestation_hash.clone(),
            line_ranges.clone(),
        ));
    }

    true
}

fn placeholder_id(name: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{name}_SESSION{ts}")
}
