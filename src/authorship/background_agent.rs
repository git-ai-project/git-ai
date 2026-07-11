use crate::authorship::authorship_log::{LineRange, MAX_MATERIALIZED_LINE_COUNT, SessionRecord};
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, generate_session_id, generate_trace_id,
};
use crate::authorship::working_log::AgentId;
use std::collections::HashMap;

const DEVIN_ID_PATH: &str = "/opt/.devin/devin_id";
const DEVIN_DIR_PATH: &str = "/opt/.devin";
const MAX_BACKGROUND_AGENT_ID_BYTES: usize = 4 * 1024;

fn read_background_agent_id(path: &std::path::Path) -> Option<String> {
    crate::utils::read_text_file_with_limit(
        path,
        MAX_BACKGROUND_AGENT_ID_BYTES as u64,
        "background agent ID",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundAgent {
    WithHooks { tool: String },
    NoHooks { tool: String, id: String },
    None,
}

pub fn detect() -> BackgroundAgent {
    // With-hooks agents are explicitly declared and take precedence over
    // directory-based no-hooks detection.
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

    // No-hooks background agents declared via environment variables.
    if std::env::vars().any(|(k, _)| k.starts_with("CLOUD_AGENT_")) {
        return BackgroundAgent::NoHooks {
            tool: "cloud-agent".to_string(),
            id: placeholder_id("CLOUD_AGENT"),
        };
    }

    if std::env::var("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
        .map(|v| v == "codex_web_agent")
        .unwrap_or(false)
    {
        let id = std::env::var("CODEX_THREAD_ID")
            .ok()
            .filter(|value| !value.is_empty() && value.len() <= MAX_BACKGROUND_AGENT_ID_BYTES)
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

    // Directory-based Devin detection can only be active when the git-ai daemon
    // is running a real user session (not a test suite). Test suites set
    // GIT_AI_TEST_DB_PATH for spawned commands/daemons, so skip /opt/.devin
    // when that marker is present.
    if std::env::var_os("GIT_AI_TEST_DB_PATH").is_none()
        && std::env::var_os("GITAI_TEST_DB_PATH").is_none()
        && std::path::Path::new(DEVIN_DIR_PATH).is_dir()
    {
        let id = read_background_agent_id(std::path::Path::new(DEVIN_ID_PATH))
            .unwrap_or_else(|| placeholder_id("DEVIN"));
        return BackgroundAgent::NoHooks {
            tool: "devin".to_string(),
            id,
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

    let mut committed_line_count = 0u64;
    for range in committed_hunks.values().flatten() {
        let range_line_count = range.covered_line_count();
        if range_line_count == 0 {
            return false;
        }
        committed_line_count = committed_line_count.saturating_add(range_line_count);
        if committed_line_count > MAX_MATERIALIZED_LINE_COUNT {
            return false;
        }
    }

    // Collect already-attributed ranges per file.
    let mut attributed_ranges: HashMap<&str, Vec<LineRange>> = HashMap::new();
    for file_attestation in &authorship_log.attestations {
        let ranges = attributed_ranges
            .entry(&file_attestation.file_path)
            .or_default();
        for entry in &file_attestation.entries {
            ranges.extend(entry.line_ranges.iter().cloned());
        }
    }
    for ranges in attributed_ranges.values_mut() {
        *ranges = LineRange::normalize(ranges);
    }

    // Find unattributed ranges per file without expanding them per line.
    let mut unattributed_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();
    for (file_path, line_ranges) in committed_hunks {
        let existing = attributed_ranges
            .get(file_path.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let unattributed = LineRange::subtract_all(line_ranges, existing);
        if !unattributed.is_empty() {
            unattributed_hunks.insert(file_path.clone(), unattributed);
        }
    }

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

#[cfg(test)]
mod tests {
    use super::read_background_agent_id;

    #[test]
    fn oversized_background_agent_id_is_rejected_before_loading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agent-id");
        std::fs::write(&path, vec![b'x'; 8 * 1024]).unwrap();

        assert_eq!(read_background_agent_id(&path), None);
    }
}
