use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git};
use crate::synopsis::config::{ConversationSourceKind, SynopsisConfig};
use crate::synopsis::conversation::{
    filter_by_time_window, find_claude_code_conversation, parse_claude_code_jsonl,
};
use crate::synopsis::types::{DiffBundle, SynopsisInput};
use std::path::Path;

/// Collect the diff for the given commit SHA against its parent(s).
///
/// If `commit_sha` is `HEAD` or a bare SHA, we run `git show --stat` and
/// `git show -U<context>` against that commit.  For the staged index use-case
/// (before a commit exists) callers should pass the literal string `"--cached"`.
pub fn collect_diff(
    repo: &Repository,
    commit_sha: &str,
    context_lines: usize,
) -> Result<DiffBundle, GitAiError> {
    // Stat summary
    let stat_summary = {
        let mut args = repo.global_args_for_exec();
        if commit_sha == "--cached" {
            args.extend(["diff".into(), "--cached".into(), "--stat".into()]);
        } else {
            args.extend([
                "show".into(),
                "--stat".into(),
                "--format=".into(),
                commit_sha.to_string(),
            ]);
        }
        let output = exec_git(&args)?;
        String::from_utf8(output.stdout)
            .map_err(GitAiError::FromUtf8Error)?
            .trim()
            .to_string()
    };

    // Unified diff
    let unified_diff = {
        let mut args = repo.global_args_for_exec();
        let context_flag = format!("-U{}", context_lines);
        if commit_sha == "--cached" {
            args.extend(["diff".into(), "--cached".into(), context_flag]);
        } else {
            args.extend([
                "show".into(),
                "--format=".into(),
                context_flag,
                commit_sha.to_string(),
            ]);
        }
        let output = exec_git(&args)?;
        String::from_utf8(output.stdout)
            .map_err(GitAiError::FromUtf8Error)?
            .trim()
            .to_string()
    };

    // Parse files_changed, insertions, deletions from the stat summary
    let (files_changed, insertions, deletions) = parse_stat_summary(&stat_summary);

    Ok(DiffBundle {
        stat_summary,
        unified_diff,
        files_changed,
        insertions,
        deletions,
    })
}

/// Parse the trailing summary line of `git diff --stat` / `git show --stat`.
///
/// The line looks like:
/// ` 3 files changed, 45 insertions(+), 2 deletions(-)`
fn parse_stat_summary(stat: &str) -> (usize, usize, usize) {
    let mut files = 0usize;
    let mut ins = 0usize;
    let mut del = 0usize;

    for line in stat.lines().rev() {
        let line = line.trim();
        if !line.contains("changed") {
            continue;
        }
        // Extract numbers preceding known keywords
        for token in line.split(',') {
            let token = token.trim();
            let digits: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
            let n: usize = digits.parse().unwrap_or(0);
            if token.contains("file") {
                files = n;
            } else if token.contains("insertion") {
                ins = n;
            } else if token.contains("deletion") {
                del = n;
            }
        }
        break;
    }

    (files, ins, del)
}

/// Retrieve the commit message for a given commit SHA.
pub fn collect_commit_message(commit_sha: &str, repo: &Repository) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".into(),
        "-1".into(),
        "--format=%B".into(),
        commit_sha.to_string(),
    ]);
    let output = exec_git(&args)?;
    let msg = String::from_utf8(output.stdout)
        .map_err(GitAiError::FromUtf8Error)?
        .trim()
        .to_string();
    Ok(msg)
}

/// Retrieve the commit author as `"Name <email>"` for a given commit SHA.
fn collect_commit_author(commit_sha: &str, repo: &Repository) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".into(),
        "-1".into(),
        "--format=%an <%ae>".into(),
        commit_sha.to_string(),
    ]);
    let output = exec_git(&args)?;
    let author = String::from_utf8(output.stdout)
        .map_err(GitAiError::FromUtf8Error)?
        .trim()
        .to_string();
    Ok(author)
}

/// Resolve the working directory of the repository to a `Path`.
fn repo_work_dir(repo: &Repository) -> Option<std::path::PathBuf> {
    repo.workdir().ok()
}

/// Collect all inputs required to generate a synopsis.
///
/// If conversation loading fails, a warning is printed and the synopsis is
/// generated without conversation context (non-fatal).
pub fn collect_input(
    repo: &Repository,
    commit_sha: &str,
    config: &SynopsisConfig,
    conversation_path_override: Option<&str>,
) -> Result<SynopsisInput, GitAiError> {
    let diff = collect_diff(repo, commit_sha, config.diff_context_lines)?;
    let commit_message = collect_commit_message(commit_sha, repo)?;
    let author = collect_commit_author(commit_sha, repo).unwrap_or_else(|_| "Unknown".to_string());

    let conversation = load_conversation(repo, config, conversation_path_override);

    Ok(SynopsisInput {
        conversation,
        diff,
        commit_message,
        commit_sha: commit_sha.to_string(),
        author,
    })
}

/// Attempt to load a conversation log, returning `None` on any failure.
fn load_conversation(
    repo: &Repository,
    config: &SynopsisConfig,
    conversation_path_override: Option<&str>,
) -> Option<crate::synopsis::types::ConversationLog> {
    if config.conversation_source == ConversationSourceKind::None {
        return None;
    }

    // Determine the JSONL file path
    let jsonl_path: std::path::PathBuf = if let Some(override_path) = conversation_path_override {
        std::path::PathBuf::from(override_path)
    } else if let Some(explicit) = &config.conversation_path {
        std::path::PathBuf::from(explicit)
    } else if config.conversation_source == ConversationSourceKind::Auto
        || config.conversation_source == ConversationSourceKind::ClaudeCode
    {
        let workdir = repo_work_dir(repo)?;
        match find_claude_code_conversation(&workdir) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[synopsis] No Claude Code conversation found in ~/.claude/projects/. \
                     Generating synopsis without conversation context."
                );
                return None;
            }
        }
    } else {
        return None;
    };

    let path: &Path = &jsonl_path;
    match parse_claude_code_jsonl(path) {
        Ok(log) => {
            let filtered = filter_by_time_window(&log, config.conversation_window_minutes);
            if filtered.exchanges.is_empty() {
                eprintln!(
                    "[synopsis] Conversation loaded but no exchanges fall within the \
                     {}-minute window. Generating synopsis without conversation context.",
                    config.conversation_window_minutes
                );
                None
            } else {
                Some(filtered)
            }
        }
        Err(e) => {
            eprintln!(
                "[synopsis] Warning: Failed to parse conversation file {}: {}. \
                 Generating synopsis without conversation context.",
                path.display(),
                e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_summary_all_fields() {
        let stat = " 3 files changed, 45 insertions(+), 2 deletions(-)";
        let (f, i, d) = parse_stat_summary(stat);
        assert_eq!(f, 3);
        assert_eq!(i, 45);
        assert_eq!(d, 2);
    }

    #[test]
    fn test_parse_stat_summary_no_deletions() {
        let stat = " 1 file changed, 10 insertions(+)";
        let (f, i, d) = parse_stat_summary(stat);
        assert_eq!(f, 1);
        assert_eq!(i, 10);
        assert_eq!(d, 0);
    }

    #[test]
    fn test_parse_stat_summary_empty() {
        let (f, i, d) = parse_stat_summary("");
        assert_eq!(f, 0);
        assert_eq!(i, 0);
        assert_eq!(d, 0);
    }
}
