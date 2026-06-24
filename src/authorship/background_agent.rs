use crate::authorship::authorship_log::{LineRange, SessionRecord};
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

    if std::env::var("HOSTNAME")
        .map(|v| v == "cursor")
        .unwrap_or(false)
        && std::env::var("CURSOR_AGENT")
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

/// If running in a no-hooks background agent, attribute any committed lines
/// that have no existing attestation ("holes") to the detected agent.
/// Existing attributions (human, other AI) are preserved.
/// Returns true if any attribution was applied.
pub fn fill_unattributed_lines(
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
    human_author: &str,
) -> bool {
    let BackgroundAgent::NoHooks { tool, id } = detect() else {
        return false;
    };

    if committed_hunks.is_empty() {
        return false;
    }

    // Find unattributed lines per file (shared with the attribution-recovery
    // pipeline so both compute "unknown" lines identically).
    let unattributed_hunks: HashMap<String, Vec<LineRange>> =
        crate::authorship::recovery::unknown_lines(authorship_log, committed_hunks)
            .into_iter()
            .map(|(file_path, lines)| (file_path, LineRange::compress_lines(&lines)))
            .collect();

    if unattributed_hunks.is_empty() {
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

    for (file_path, line_ranges) in unattributed_hunks {
        let file_attestation = authorship_log.get_or_create_file(&file_path);
        file_attestation.add_entry(AttestationEntry::new(attestation_hash.clone(), line_ranges));
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
