use std::collections::HashMap;

use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, FileAttestation,
};
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::repository::{Repository, exec_git};

/// Handle a `git revert` commit by reconstructing attribution for re-introduced lines.
///
/// Uses `git-ai blame` on the grandparent to determine correct attribution for
/// lines that the revert re-introduces. This ensures human-overridden lines are
/// correctly identified as human even if older commits had AI attestation.
pub fn handle_revert_commit(
    repo: &Repository,
    revert_commit: &str,
    parent: Option<&str>,
) -> Result<(), GitAiError> {
    let parent_sha = match parent {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            let mut args = repo.global_args_for_exec();
            args.extend_from_slice(&["rev-parse".to_string(), format!("{}~1", revert_commit)]);
            let output = exec_git(&args)?;
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
    };

    // Grandparent = parent of the reverted commit
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&["rev-parse".to_string(), format!("{}~1", parent_sha)]);
    let grandparent_sha = match exec_git(&args) {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => return Ok(()),
    };

    if grandparent_sha.is_empty() {
        return Ok(());
    }

    // Find lines added by the revert relative to its parent
    let added_lines = repo.diff_added_lines(&parent_sha, revert_commit, None)?;
    if added_lines.is_empty() {
        return Ok(());
    }

    // For each file with added lines, run git-ai blame analysis on the grandparent
    // to determine correct attribution (handles human-override cases correctly).
    // Group by (file, tool_name) so each tool gets its own session in the note.
    let mut ai_lines_per_file_tool: HashMap<(String, String), Vec<u32>> = HashMap::new();

    let options = GitAiBlameOptions {
        newest_commit: Some(grandparent_sha.clone()),
        no_output: true,
        ..Default::default()
    };

    for (file_path, lines) in &added_lines {
        if lines.is_empty() {
            continue;
        }

        let (line_authors, _prompts) = match repo.blame(file_path, &options) {
            Ok(result) => result,
            Err(_) => continue,
        };

        for &line_num in lines {
            if let Some(author) = line_authors.get(&line_num)
                && is_ai_author(author)
            {
                ai_lines_per_file_tool
                    .entry((file_path.clone(), author.clone()))
                    .or_default()
                    .push(line_num);
            }
        }
    }

    if ai_lines_per_file_tool.is_empty() {
        return Ok(());
    }

    // Build attestation grouped by tool name — each unique tool gets its own session
    let mut log = AuthorshipLog::new();
    log.metadata.base_commit_sha = revert_commit.to_string();

    // Collect unique tool names and create sessions
    let mut tool_sessions: HashMap<String, String> = HashMap::new();
    for (_file, tool_name) in ai_lines_per_file_tool.keys() {
        if !tool_sessions.contains_key(tool_name) {
            let session_id = format!("s_revert_{}", tool_name.replace(' ', "_"));
            tool_sessions.insert(tool_name.clone(), session_id);
        }
    }

    // Group lines by file (merging across tools for the same file)
    let mut file_entries: HashMap<String, Vec<(String, Vec<u32>)>> = HashMap::new();
    for ((file_path, tool_name), mut lines) in ai_lines_per_file_tool {
        lines.sort();
        lines.dedup();
        let session_id = tool_sessions[&tool_name].clone();
        file_entries
            .entry(file_path)
            .or_default()
            .push((session_id, lines));
    }

    for (file_path, entries) in file_entries {
        let mut fa = FileAttestation::new(file_path);
        for (session_id, lines) in entries {
            let ranges = LineRange::compress_lines(&lines);
            fa.add_entry(AttestationEntry::new(session_id, ranges));
        }
        log.attestations.push(fa);
    }

    // Add session records for each tool
    for (tool_name, session_id) in &tool_sessions {
        log.metadata.sessions.insert(
            session_id.clone(),
            crate::authorship::authorship_log::SessionRecord {
                agent_id: crate::authorship::working_log::AgentId {
                    tool: tool_name.clone(),
                    id: String::new(),
                    model: "unknown".to_string(),
                },
                human_author: None,
                custom_attributes: None,
            },
        );
    }

    let note_str = log.serialize_to_string().map_err(|_| {
        GitAiError::Generic("Failed to serialize revert authorship log".to_string())
    })?;

    notes_add(repo, revert_commit, &note_str)?;
    Ok(())
}

pub fn is_ai_author(author: &str) -> bool {
    let ai_tools = [
        "mock_ai",
        "claude",
        "continue-cli",
        "gpt",
        "copilot",
        "cursor",
        "codex",
        "gemini",
        "windsurf",
        "aider",
        "devin",
        "cline",
        "roo",
    ];
    let lower = author.to_lowercase();
    ai_tools.iter().any(|tool| lower.contains(tool))
}
