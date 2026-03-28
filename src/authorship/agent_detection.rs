//! Detection of known AI agent commits by email pattern or GitHub username.
//!
//! This module provides public helper functions for matching commit author
//! emails and GitHub usernames to known AI coding agents/platforms.
//! It also provides logic for simulating authorship data for agent commits
//! that lack explicit authorship notes.

use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, AuthorshipMetadata, FileAttestation, generate_short_hash,
};
use crate::authorship::working_log::AgentId;

/// Known agent email mappings: (email_suffix, tool_name)
/// For GitHub noreply emails, we match after the `+` to ignore numeric user ID prefixes.
const AGENT_EMAIL_MAPPINGS: &[(&str, &str)] = &[
    ("cursoragent@cursor.com", "cursor-agent"),
    ("+copilot@users.noreply.github.com", "github-copilot-agent"),
    (
        "+devin-ai-integration[bot]@users.noreply.github.com",
        "devin",
    ),
    ("noreply@anthropic.com", "claude-web"),
    ("noreply@openai.com", "codex-cloud"),
    ("roomote@roocode.com", "roo-background"),
];

/// Known GitHub username mappings: (username, platform)
const AGENT_USERNAME_MAPPINGS: &[(&str, &str)] = &[
    ("copilot-swe-agent[bot]", "github-copilot-agent"),
    ("devin-ai-integration[bot]", "devin"),
    ("cursor[bot]", "cursor-agent"),
];

/// Match a commit author email to a known AI agent tool name.
///
/// Returns the tool name (e.g. "cursor-agent", "github-copilot-agent", "devin", "claude-web", "codex-cloud")
/// if the email matches a known agent pattern, or `None` otherwise.
///
/// # Examples
/// ```
/// use git_ai::authorship::agent_detection::match_email_to_agent;
///
/// assert_eq!(match_email_to_agent("cursoragent@cursor.com"), Some("cursor-agent"));
/// assert_eq!(match_email_to_agent("noreply@anthropic.com"), Some("claude-web"));
/// assert_eq!(match_email_to_agent("user@example.com"), None);
/// ```
pub fn match_email_to_agent(email: &str) -> Option<&'static str> {
    let email_lower = email.to_lowercase();
    AGENT_EMAIL_MAPPINGS
        .iter()
        .find(|(pattern, _)| {
            let pattern_lower = pattern.to_lowercase();
            if pattern_lower.starts_with('+') {
                // Suffix match: ignore numeric ID prefix in GitHub noreply emails
                email_lower.ends_with(&pattern_lower)
            } else {
                email_lower == pattern_lower
            }
        })
        .map(|(_, tool)| *tool)
}

/// Match a GitHub username to a known AI platform name.
///
/// Returns the platform name (e.g. "github-copilot", "devin", "cursor")
/// if the username matches a known agent pattern, or `None` otherwise.
///
/// # Examples
/// ```
/// use git_ai::authorship::agent_detection::match_username_to_platform;
///
/// assert_eq!(match_username_to_platform("copilot-swe-agent[bot]"), Some("github-copilot-agent"));
/// assert_eq!(match_username_to_platform("devin-ai-integration[bot]"), Some("devin"));
/// assert_eq!(match_username_to_platform("regular-user"), None);
/// ```
pub fn match_username_to_platform(username: &str) -> Option<&'static str> {
    let username_lower = username.to_lowercase();
    AGENT_USERNAME_MAPPINGS
        .iter()
        .find(|(pattern, _)| pattern.to_lowercase() == username_lower)
        .map(|(_, platform)| *platform)
}

/// Simulate an `AuthorshipLog` for a commit made by a known AI agent
/// that has no explicit authorship note.
///
/// This creates:
/// - An `AgentId` with the detected tool, commit SHA as session id, model "unknown"
/// - A `PromptRecord` with stats: accepted_lines = total lines, all AI
/// - An `AuthorshipLog` with a single file attestation covering all specified lines
///
/// The prompt hash is derived from the commit SHA and tool name.
///
/// # Arguments
/// * `commit_sha` - The commit SHA (used as prompt_id / session id)
/// * `tool` - The detected tool name (e.g. "cursor", "devin")
/// * `file_path` - The file path for attestation
/// * `line_start` - Start line (inclusive)
/// * `line_end` - End line (inclusive)
pub fn simulate_agent_authorship(
    commit_sha: &str,
    tool: &str,
    file_path: &str,
    line_start: u32,
    line_end: u32,
) -> (AuthorshipLog, String) {
    use crate::authorship::authorship_log::LineRange;

    let total_lines = if line_end >= line_start {
        line_end - line_start + 1
    } else {
        0
    };

    let agent_id = AgentId {
        tool: tool.to_string(),
        id: commit_sha.to_string(),
        model: "unknown".to_string(),
    };

    let prompt_hash = generate_short_hash(&agent_id.id, &agent_id.tool);

    let prompt_record = PromptRecord {
        agent_id,
        human_author: None,
        messages: vec![],
        total_additions: total_lines,
        total_deletions: 0,
        accepted_lines: total_lines,
        overriden_lines: 0,
        messages_url: None,
        custom_attributes: None,
    };

    let line_range = if line_start == line_end {
        LineRange::Single(line_start)
    } else {
        LineRange::Range(line_start, line_end)
    };

    let entry = AttestationEntry::new(prompt_hash.clone(), vec![line_range]);
    let mut file_attestation = FileAttestation::new(file_path.to_string());
    file_attestation.add_entry(entry);

    let mut metadata = AuthorshipMetadata::new();
    metadata.base_commit_sha = commit_sha.to_string();
    metadata.prompts.insert(prompt_hash.clone(), prompt_record);

    let log = AuthorshipLog {
        attestations: vec![file_attestation],
        metadata,
    };

    (log, prompt_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // match_email_to_agent tests
    // ========================================================================

    #[test]
    fn test_match_email_cursor() {
        assert_eq!(
            match_email_to_agent("cursoragent@cursor.com"),
            Some("cursor-agent")
        );
    }

    #[test]
    fn test_match_email_copilot() {
        assert_eq!(
            match_email_to_agent("198982749+Copilot@users.noreply.github.com"),
            Some("github-copilot-agent")
        );
        // Different numeric prefix should still match
        assert_eq!(
            match_email_to_agent("999999+Copilot@users.noreply.github.com"),
            Some("github-copilot-agent")
        );
    }

    #[test]
    fn test_match_email_devin() {
        assert_eq!(
            match_email_to_agent("158243242+devin-ai-integration[bot]@users.noreply.github.com"),
            Some("devin")
        );
        // Different numeric prefix should still match
        assert_eq!(
            match_email_to_agent("12345+devin-ai-integration[bot]@users.noreply.github.com"),
            Some("devin")
        );
    }

    #[test]
    fn test_match_email_claude() {
        assert_eq!(
            match_email_to_agent("noreply@anthropic.com"),
            Some("claude-web")
        );
    }

    #[test]
    fn test_match_email_codex() {
        assert_eq!(
            match_email_to_agent("noreply@openai.com"),
            Some("codex-cloud")
        );
    }

    #[test]
    fn test_match_email_case_insensitive() {
        assert_eq!(
            match_email_to_agent("CursorAgent@Cursor.com"),
            Some("cursor-agent")
        );
        assert_eq!(
            match_email_to_agent("NOREPLY@ANTHROPIC.COM"),
            Some("claude-web")
        );
    }

    #[test]
    fn test_match_email_no_match() {
        assert_eq!(match_email_to_agent("user@example.com"), None);
        assert_eq!(match_email_to_agent("john@github.com"), None);
        assert_eq!(match_email_to_agent(""), None);
    }

    // ========================================================================
    // match_username_to_platform tests
    // ========================================================================

    #[test]
    fn test_match_username_copilot() {
        assert_eq!(
            match_username_to_platform("copilot-swe-agent[bot]"),
            Some("github-copilot-agent")
        );
    }

    #[test]
    fn test_match_username_devin() {
        assert_eq!(
            match_username_to_platform("devin-ai-integration[bot]"),
            Some("devin")
        );
    }

    #[test]
    fn test_match_username_cursor() {
        assert_eq!(
            match_username_to_platform("cursor[bot]"),
            Some("cursor-agent")
        );
    }

    #[test]
    fn test_match_username_case_insensitive() {
        assert_eq!(
            match_username_to_platform("Copilot-SWE-Agent[bot]"),
            Some("github-copilot-agent")
        );
    }

    #[test]
    fn test_match_username_no_match() {
        assert_eq!(match_username_to_platform("regular-user"), None);
        assert_eq!(match_username_to_platform("octocat"), None);
        assert_eq!(match_username_to_platform(""), None);
    }

    // ========================================================================
    // simulate_agent_authorship tests
    // ========================================================================

    #[test]
    fn test_simulate_agent_authorship_basic() {
        let (log, prompt_hash) =
            simulate_agent_authorship("abc123def456", "cursor", "src/main.rs", 1, 10);

        // Verify log structure
        assert_eq!(log.attestations.len(), 1);
        assert_eq!(log.attestations[0].file_path, "src/main.rs");
        assert_eq!(log.attestations[0].entries.len(), 1);
        assert_eq!(log.attestations[0].entries[0].hash, prompt_hash);

        // Verify prompt record
        let prompt = log.metadata.prompts.get(&prompt_hash).unwrap();
        assert_eq!(prompt.agent_id.tool, "cursor");
        assert_eq!(prompt.agent_id.id, "abc123def456");
        assert_eq!(prompt.agent_id.model, "unknown");
        assert_eq!(prompt.accepted_lines, 10);
        assert_eq!(prompt.total_additions, 10);
        assert_eq!(prompt.total_deletions, 0);
        assert_eq!(prompt.overriden_lines, 0);
        assert!(prompt.human_author.is_none());
        assert!(prompt.messages.is_empty());
    }

    #[test]
    fn test_simulate_agent_authorship_single_line() {
        let (log, prompt_hash) = simulate_agent_authorship("sha123", "devin", "test.ts", 5, 5);

        let prompt = log.metadata.prompts.get(&prompt_hash).unwrap();
        assert_eq!(prompt.accepted_lines, 1);
        assert_eq!(prompt.total_additions, 1);
    }

    #[test]
    fn test_simulate_agent_authorship_uses_commit_sha_as_id() {
        let commit_sha = "deadbeef12345678";
        let (log, _) = simulate_agent_authorship(commit_sha, "claude", "file.py", 1, 5);

        assert_eq!(log.metadata.base_commit_sha, commit_sha);
        let prompt = log.metadata.prompts.values().next().unwrap();
        assert_eq!(prompt.agent_id.id, commit_sha);
    }

    #[test]
    fn test_simulate_agent_authorship_deterministic_hash() {
        let (_, hash1) = simulate_agent_authorship("sha123", "cursor", "file.rs", 1, 10);
        let (_, hash2) = simulate_agent_authorship("sha123", "cursor", "other.rs", 1, 5);

        // Same commit + tool should produce the same prompt hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_simulate_agent_authorship_different_tools_different_hash() {
        let (_, hash1) = simulate_agent_authorship("sha123", "cursor", "file.rs", 1, 10);
        let (_, hash2) = simulate_agent_authorship("sha123", "devin", "file.rs", 1, 10);

        // Different tools should produce different hashes
        assert_ne!(hash1, hash2);
    }
}
