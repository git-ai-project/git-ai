use crate::authorship::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::authorship::working_log::Checkpoint;
use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use crate::utils::debug_log;
use serde_json;
use std::collections::{HashMap, HashSet};

// Modern refspecs without force to enable proper merging
pub const AI_AUTHORSHIP_REFNAME: &str = "ai";
pub const AI_AUTHORSHIP_PUSH_REFSPEC: &str = "refs/notes/ai:refs/notes/ai";

pub fn notes_add(
    repo: &Repository,
    commit_sha: &str,
    note_content: &str,
) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("add".to_string());
    args.push("-f".to_string()); // Always force overwrite
    args.push("-F".to_string());
    args.push("-".to_string()); // Read note content from stdin
    args.push(commit_sha.to_string());

    // Use stdin to provide the note content to avoid command line length limits
    exec_git_stdin(&args, note_content.as_bytes())?;
    Ok(())
}

// Check which commits from the given list have authorship notes.
// Uses git cat-file --batch-check to efficiently check multiple commits in one invocation.
// Returns a Vec of CommitAuthorship for each commit.
#[derive(Debug, Clone)]

pub enum CommitAuthorship {
    NoLog {
        sha: String,
        git_author: String,
    },
    Log {
        sha: String,
        git_author: String,
        authorship_log: AuthorshipLog,
    },
}
pub fn get_commits_with_notes_from_list(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<Vec<CommitAuthorship>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(Vec::new());
    }

    // Get the git authors for all commits using git rev-list
    // This approach works in both bare and normal repositories
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--no-walk".to_string());
    args.push("--pretty=format:%H%n%an%n%ae".to_string());
    for sha in commit_shas {
        args.push(sha.clone());
    }

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse git rev-list output".to_string()))?;

    let mut commit_authors = HashMap::new();
    let lines: Vec<&str> = stdout.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Skip commit headers (start with "commit ")
        if line.starts_with("commit ") {
            i += 1;
            if i + 2 < lines.len() {
                let sha = lines[i].to_string();
                let name = lines[i + 1].to_string();
                let email = lines[i + 2].to_string();
                let author = format!("{} <{}>", name, email);
                commit_authors.insert(sha, author);
                i += 3;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }

    // Build the result Vec
    let mut result = Vec::new();
    for sha in commit_shas {
        let git_author = commit_authors
            .get(sha)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());

        // Check if this commit has a note by trying to show it
        if let Some(authorship_log) = get_authorship(repo, sha) {
            result.push(CommitAuthorship::Log {
                sha: sha.clone(),
                git_author,
                authorship_log,
            });
        } else {
            result.push(CommitAuthorship::NoLog {
                sha: sha.clone(),
                git_author,
            });
        }
    }

    Ok(result)
}

// Show an authorship note and return its JSON content if found, or None if it doesn't exist.
pub fn show_authorship_note(repo: &Repository, commit_sha: &str) -> Option<String> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("show".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        Err(GitAiError::GitCliError { code: Some(1), .. }) => None,
        Err(_) => None,
    }
}

// Show an authorship note and return its JSON content if found, or None if it doesn't exist.
pub fn get_authorship(repo: &Repository, commit_sha: &str) -> Option<AuthorshipLog> {
    let content = show_authorship_note(repo, commit_sha)?;
    let authorship_log = AuthorshipLog::deserialize_from_string(&content).ok()?;
    Some(authorship_log)
}

#[allow(dead_code)]
pub fn get_reference_as_working_log(
    repo: &Repository,
    commit_sha: &str,
) -> Result<Vec<Checkpoint>, GitAiError> {
    let content = show_authorship_note(repo, commit_sha)
        .ok_or_else(|| GitAiError::Generic("No authorship note found".to_string()))?;
    let working_log = serde_json::from_str(&content)?;
    Ok(working_log)
}

pub fn get_reference_as_authorship_log_v3(
    repo: &Repository,
    commit_sha: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let content = show_authorship_note(repo, commit_sha)
        .ok_or_else(|| GitAiError::Generic("No authorship note found".to_string()))?;

    // Try to deserialize as AuthorshipLog
    let authorship_log = match AuthorshipLog::deserialize_from_string(&content) {
        Ok(log) => log,
        Err(_) => {
            return Err(GitAiError::Generic(
                "Failed to parse authorship log".to_string(),
            ));
        }
    };

    // Check version compatibility
    if authorship_log.metadata.schema_version != AUTHORSHIP_LOG_VERSION {
        return Err(GitAiError::Generic(format!(
            "Unsupported authorship log version: {} (expected: {})",
            authorship_log.metadata.schema_version, AUTHORSHIP_LOG_VERSION
        )));
    }

    Ok(authorship_log)
}

/// Sanitize a remote name to create a safe ref name
/// Replaces special characters with underscores to ensure valid ref names
fn sanitize_remote_name(remote: &str) -> String {
    remote
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Generate a tracking ref name for notes from a specific remote
/// Returns a ref like "refs/notes/ai-remote/origin"
///
/// SAFETY: These tracking refs are stored under refs/notes/ai-remote/* which:
/// - Won't be pushed by `git push` (only pushes refs/heads/* by default)
/// - Won't be pushed by `git push --all` (only pushes refs/heads/*)
/// - Won't be pushed by `git push --tags` (only pushes refs/tags/*)
/// - **WILL** be pushed by `git push --mirror` (usually only used for backups, etc.)
/// - **WILL** be pushed if user explicitly specifies refs/notes/ai-remote/* (extremely rare)
pub fn tracking_ref_for_remote(remote_name: &str) -> String {
    format!("refs/notes/ai-remote/{}", sanitize_remote_name(remote_name))
}

/// Check if a ref exists in the repository
pub fn ref_exists(repo: &Repository, ref_name: &str) -> bool {
    let mut args = repo.global_args_for_exec();
    args.push("show-ref".to_string());
    args.push("--verify".to_string());
    args.push("--quiet".to_string());
    args.push(ref_name.to_string());

    exec_git(&args).is_ok()
}

/// Merge notes from a source ref into refs/notes/ai
/// Uses the 'ours' strategy to combine notes without data loss
pub fn merge_notes_from_ref(repo: &Repository, source_ref: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push(format!("--ref={}", AI_AUTHORSHIP_REFNAME));
    args.push("merge".to_string());
    args.push("-s".to_string());
    args.push("ours".to_string());
    args.push("--quiet".to_string());
    args.push(source_ref.to_string());

    debug_log(&format!(
        "Merging notes from {} into refs/notes/ai",
        source_ref
    ));
    exec_git(&args)?;
    Ok(())
}

/// Copy a ref to another location (used for initial setup of local notes from tracking ref)
pub fn copy_ref(repo: &Repository, source_ref: &str, dest_ref: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("update-ref".to_string());
    args.push(dest_ref.to_string());
    args.push(source_ref.to_string());

    debug_log(&format!("Copying ref {} to {}", source_ref, dest_ref));
    exec_git(&args)?;
    Ok(())
}

/// Search AI notes for a pattern and return matching commit SHAs ordered by commit date (newest first)
/// Uses git grep to search through refs/notes/ai
pub fn grep_ai_notes(repo: &Repository, pattern: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("--no-pager".to_string());
    args.push("grep".to_string());
    args.push("-nI".to_string());
    args.push(pattern.to_string());
    args.push("refs/notes/ai".to_string());

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse git grep output".to_string()))?;

    // Parse output format: refs/notes/ai:ab/cdef123...:line_number:matched_content
    // Extract the commit SHA from the path
    let mut shas = HashSet::new();
    for line in stdout.lines() {
        if let Some(path_and_rest) = line.strip_prefix("refs/notes/ai:") {
            if let Some(path_end) = path_and_rest.find(':') {
                let path = &path_and_rest[..path_end];
                // Path is in format "ab/cdef123..." - combine to get full SHA
                let sha = path.replace('/', "");
                shas.insert(sha);
            }
        }
    }

    // If we have multiple results, sort by commit date (newest first)
    if shas.len() > 1 {
        let sha_vec: Vec<String> = shas.into_iter().collect();
        let mut args = repo.global_args_for_exec();
        args.push("log".to_string());
        args.push("--format=%H".to_string());
        args.push("--date-order".to_string());
        args.push("--no-walk".to_string());
        for sha in &sha_vec {
            args.push(sha.clone());
        }

        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| GitAiError::Generic("Failed to parse git log output".to_string()))?;

        Ok(stdout.lines().map(|s| s.to_string()).collect())
    } else {
        Ok(shas.into_iter().collect())
    }
}

/// Search AI notes for a pattern and return matching note paths (not commit SHAs).
/// This works even for notes attached to deleted commits.
/// (hopefully no scm platform deletes hanging notes)
pub fn grep_ai_notes_paths(repo: &Repository, pattern: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("--no-pager".to_string());
    args.push("grep".to_string());
    args.push("-l".to_string()); // Only show file paths
    args.push(pattern.to_string());
    args.push("refs/notes/ai".to_string());

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse git grep output".to_string()))?;

    // Parse output format: refs/notes/ai:ab/cdef123...
    // Extract the path portion (ab/cdef123...)
    let mut paths: HashSet<String> = HashSet::new();
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("refs/notes/ai:") {
            paths.insert(path.to_string());
        }
    }

    Ok(paths.into_iter().collect())
}

/// Read a note directly from its path in the notes tree.
/// This works even when the commit the note is attached to has been deleted.
/// Accepts paths in either format: "ab/cdef123..." or "abcdef123..."
/// Git uses flat paths for small repos and tree paths (with slash) for large repos.
pub fn read_note_from_path(repo: &Repository, note_path: &str) -> Option<AuthorshipLog> {
    let original = note_path.to_string();
    let flat = note_path.replace('/', "");
    let tree = if flat.len() >= 2 {
        format!("{}/{}", &flat[..2], &flat[2..])
    } else {
        return None;
    };

    // Build deduped list: original first, then flat, then tree
    let mut paths = Vec::with_capacity(3);
    for path in [original, flat, tree] {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    for path in paths {
        let mut args = repo.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!("refs/notes/ai:{}", path));

        if let Ok(output) = exec_git(&args) {
            if let Ok(content) = String::from_utf8(output.stdout) {
                if let Ok(log) = AuthorshipLog::deserialize_from_string(&content) {
                    return Some(log);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_notes_add_and_show_authorship_note() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create a commit first
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to create initial commit");

        // Get the commit SHA
        let commit_sha = tmp_repo
            .get_head_commit_sha()
            .expect("Failed to get head commit SHA");

        // Test data - simple string content
        let note_content = "This is a test authorship note with some random content!";

        // Add the authorship note (force overwrite since commit_with_message already created one)
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note_content)
            .expect("Failed to add authorship note");

        // Read the note back
        let retrieved_content = show_authorship_note(tmp_repo.gitai_repo(), &commit_sha)
            .expect("Failed to retrieve authorship note");

        // Assert the content matches exactly
        assert_eq!(retrieved_content, note_content);

        // Test that non-existent commit returns None
        let non_existent_content = show_authorship_note(
            tmp_repo.gitai_repo(),
            "0000000000000000000000000000000000000000",
        );
        assert!(non_existent_content.is_none());
    }

    #[test]
    fn test_read_note_from_path_both_formats() {
        // Test that read_note_from_path can read notes using both path formats:
        // - Flat path: "abcdef123..." (used by git for small repos)
        // - Tree path: "ab/cdef123..." (used by git for large repos)
        //
        // The function should handle both formats transparently.

        let tmp_repo = TmpRepo::new().unwrap();

        // Create a commit with AI authorship
        let mut file = tmp_repo.write_file("test.txt", "Line1\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();

        file.append("AI Line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("TestAI", Some("test-model"), Some("test-tool"))
            .unwrap();
        tmp_repo.commit_with_message("AI commit").unwrap();

        let commit_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Generate both path formats from the commit SHA
        let flat_path = commit_sha.clone();
        let tree_path = format!("{}/{}", &commit_sha[..2], &commit_sha[2..]);

        // Test flat path format (abcdef123...)
        // This should work in small repos where git stores notes flat
        let note_flat = read_note_from_path(&tmp_repo.gitai_repo(), &flat_path);
        assert!(
            note_flat.is_some(),
            "Should be able to read note from flat path {}",
            flat_path
        );
        assert!(
            !note_flat.unwrap().metadata.prompts.is_empty(),
            "Note from flat path should have prompts"
        );

        // Test tree path format (ab/cdef123...)
        // The function should normalize this to flat format if needed
        let note_tree = read_note_from_path(&tmp_repo.gitai_repo(), &tree_path);
        assert!(
            note_tree.is_some(),
            "Should be able to read note from tree path {} (normalized to flat)",
            tree_path
        );
        assert!(
            !note_tree.unwrap().metadata.prompts.is_empty(),
            "Note from tree path should have prompts"
        );
    }

    #[test]
    fn test_grep_ai_notes_paths_finds_unreachable_commits() {
        // Test that grep_ai_notes_paths can find notes even when the commit
        // they're attached to is no longer reachable from any branch
        // (e.g., after a reset or rebase). The commit object may still exist
        // in the object store but is not reachable via any ref.
        use crate::git::repository::exec_git;

        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial commit
        let mut file = tmp_repo.write_file("test.txt", "Line1\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let initial_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Create a commit with AI authorship
        file.append("AI Line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("TestAI", Some("test-model"), Some("test-tool"))
            .unwrap();
        tmp_repo.commit_with_message("AI commit").unwrap();

        let ai_commit_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Get the prompt hash before we make the commit unreachable
        let log = get_authorship(&tmp_repo.gitai_repo(), &ai_commit_sha).unwrap();
        let prompt_hash = log.metadata.prompts.keys().next().unwrap().clone();

        // Make the AI commit unreachable by resetting to the initial commit
        // The note will still exist in refs/notes/ai even though the commit is unreachable
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend(["reset", "--hard", &initial_sha].map(String::from));
        exec_git(&args).expect("Failed to reset");

        // Verify the commit is no longer reachable from HEAD
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend(["branch", "--contains", &ai_commit_sha].map(String::from));
        let result = exec_git(&args).expect("branch --contains failed");
        let branches = String::from_utf8_lossy(&result.stdout);
        assert!(
            branches.trim().is_empty(),
            "The AI commit should not be reachable from any branch after reset"
        );

        // grep_ai_notes_paths should still find the note
        let paths =
            grep_ai_notes_paths(&tmp_repo.gitai_repo(), &format!("\"{}\"", prompt_hash)).unwrap();

        assert!(
            !paths.is_empty(),
            "grep_ai_notes_paths should find notes even for unreachable commits"
        );

        // And we should be able to read the note content
        let note = read_note_from_path(&tmp_repo.gitai_repo(), &paths[0]);
        assert!(
            note.is_some(),
            "Should be able to read note from unreachable commit via path"
        );
        assert!(
            note.unwrap().metadata.prompts.contains_key(&prompt_hash),
            "Note should contain the expected prompt"
        );
    }
}
