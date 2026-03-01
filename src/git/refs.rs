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

fn notes_path_for_object(oid: &str) -> String {
    if oid.len() <= 2 {
        oid.to_string()
    } else {
        format!("{}/{}", &oid[..2], &oid[2..])
    }
}

fn flat_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!("refs/notes/ai:{}", commit_sha)
}

fn fanout_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!("refs/notes/ai:{}", notes_path_for_object(commit_sha))
}

fn parse_batch_check_blob_oid(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let oid = parts.first().copied().unwrap_or_default();
    let valid_oid_len = oid.len() == 40 || oid.len() == 64;
    if parts.len() >= 2
        && parts[1] == "blob"
        && valid_oid_len
        && oid.as_bytes().iter().all(|b| b.is_ascii_hexdigit())
    {
        Some(oid.to_string())
    } else {
        None
    }
}

/// Resolve authorship note blob OIDs for a set of commits using one batched cat-file call.
///
/// Returns a map of commit SHA -> note blob SHA for commits that currently have notes.
pub fn note_blob_oids_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch-check".to_string());

    let mut stdin_data = String::new();
    for commit_sha in commit_shas {
        // Notes can be stored with either flat paths (<sha>) or fanout paths (<aa>/<bb...>).
        // Query both forms so this works regardless of repository note fanout state.
        stdin_data.push_str(&flat_note_pathspec_for_commit(commit_sha));
        stdin_data.push('\n');
        stdin_data.push_str(&fanout_note_pathspec_for_commit(commit_sha));
        stdin_data.push('\n');
    }

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let mut result = HashMap::new();

    for commit_sha in commit_shas {
        let Some(flat_line) = lines.next() else {
            break;
        };
        let fanout_line = lines.next().unwrap_or_default();

        if let Some(oid) = parse_batch_check_blob_oid(flat_line)
            .or_else(|| parse_batch_check_blob_oid(fanout_line))
        {
            result.insert(commit_sha.clone(), oid);
        }
    }

    Ok(result)
}

pub fn notes_add_batch(repo: &Repository, entries: &[(String, String)]) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push("--verify".to_string());
    args.push("refs/notes/ai".to_string());
    let existing_notes_tip = match exec_git(&args) {
        Ok(output) => Some(String::from_utf8(output.stdout)?.trim().to_string()),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        })
        | Err(GitAiError::GitCliError { code: Some(1), .. }) => None,
        Err(e) => return Err(e),
    };

    let mut deduped_entries: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (commit_sha, note_content) in entries.iter().rev() {
        if seen.insert(commit_sha.as_str()) {
            deduped_entries.push((commit_sha.clone(), note_content.clone()));
        }
    }
    deduped_entries.reverse();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {}", e)))?
        .as_secs();

    let mut script = Vec::<u8>::new();

    for (idx, (_commit_sha, note_content)) in deduped_entries.iter().enumerate() {
        script.extend_from_slice(b"blob\n");
        script.extend_from_slice(format!("mark :{}\n", idx + 1).as_bytes());
        script.extend_from_slice(format!("data {}\n", note_content.len()).as_bytes());
        script.extend_from_slice(note_content.as_bytes());
        script.extend_from_slice(b"\n");
    }

    script.extend_from_slice(b"commit refs/notes/ai\n");
    script.extend_from_slice(format!("committer git-ai <git-ai@local> {} +0000\n", now).as_bytes());
    script.extend_from_slice(b"data 0\n");
    if let Some(existing_tip) = existing_notes_tip {
        script.extend_from_slice(format!("from {}\n", existing_tip).as_bytes());
    }

    for (idx, (commit_sha, _note_content)) in deduped_entries.iter().enumerate() {
        let fanout_path = notes_path_for_object(commit_sha);
        let flat_path = commit_sha.clone();
        if flat_path != fanout_path {
            script.extend_from_slice(format!("D {}\n", flat_path).as_bytes());
        }
        script.extend_from_slice(format!("D {}\n", fanout_path).as_bytes());
        script.extend_from_slice(format!("M 100644 :{} {}\n", idx + 1, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;

    Ok(())
}

/// Batch-attach existing note blobs to commits without rewriting blob contents.
///
/// Each entry is (commit_sha, existing_note_blob_oid).
#[allow(dead_code)]
pub fn notes_add_blob_batch(
    repo: &Repository,
    entries: &[(String, String)],
) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push("--verify".to_string());
    args.push("refs/notes/ai".to_string());
    let existing_notes_tip = match exec_git(&args) {
        Ok(output) => Some(String::from_utf8(output.stdout)?.trim().to_string()),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        })
        | Err(GitAiError::GitCliError { code: Some(1), .. }) => None,
        Err(e) => return Err(e),
    };

    let mut deduped_entries: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (commit_sha, blob_oid) in entries.iter().rev() {
        if seen.insert(commit_sha.as_str()) {
            deduped_entries.push((commit_sha.clone(), blob_oid.clone()));
        }
    }
    deduped_entries.reverse();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {}", e)))?
        .as_secs();

    let mut script = Vec::<u8>::new();
    script.extend_from_slice(b"commit refs/notes/ai\n");
    script.extend_from_slice(format!("committer git-ai <git-ai@local> {} +0000\n", now).as_bytes());
    script.extend_from_slice(b"data 0\n");
    if let Some(existing_tip) = existing_notes_tip {
        script.extend_from_slice(format!("from {}\n", existing_tip).as_bytes());
    }

    for (commit_sha, blob_oid) in &deduped_entries {
        let fanout_path = notes_path_for_object(commit_sha);
        let flat_path = commit_sha.clone();
        if flat_path != fanout_path {
            script.extend_from_slice(format!("D {}\n", flat_path).as_bytes());
        }
        script.extend_from_slice(format!("D {}\n", fanout_path).as_bytes());
        script.extend_from_slice(format!("M 100644 {} {}\n", blob_oid, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;

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

/// Return the subset of `commit_shas` that currently has an authorship note.
///
/// This uses a single `git notes --ref=ai list` invocation instead of one
/// `git notes show` call per commit.
pub fn commits_with_authorship_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashSet<String>, GitAiError> {
    Ok(note_blob_oids_for_commits(repo, commit_shas)?
        .into_keys()
        .collect())
}

// Show an authorship note and return its JSON content if found, or None if it doesn't exist.
pub fn get_authorship(repo: &Repository, commit_sha: &str) -> Option<AuthorshipLog> {
    let content = show_authorship_note(repo, commit_sha)?;
    let mut authorship_log = AuthorshipLog::deserialize_from_string(&content).ok()?;
    // Keep metadata aligned with the commit where this note is attached.
    authorship_log.metadata.base_commit_sha = commit_sha.to_string();
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
    let mut authorship_log = match AuthorshipLog::deserialize_from_string(&content) {
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

    // Keep metadata aligned with the commit where this note is attached.
    authorship_log.metadata.base_commit_sha = commit_sha.to_string();

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
        if let Some(path_and_rest) = line.strip_prefix("refs/notes/ai:")
            && let Some(path_end) = path_and_rest.find(':')
        {
            let path = &path_and_rest[..path_end];
            // Path is in format "ab/cdef123..." - combine to get full SHA
            let sha = path.replace('/', "");
            shas.insert(sha);
        }
    }

    // If we have multiple results, sort by commit date (newest first)
    if shas.len() > 1 {
        let sha_vec: Vec<String> = shas.into_iter().collect();
        
        eprintln!("[DEBUG] Found {} commits with pattern '{}' in git notes", sha_vec.len(), pattern);
        for sha in &sha_vec {
            eprintln!("[DEBUG]   - {}", sha);
        }
        
        // FILTER OUT NON-EXISTENT COMMITS using git cat-file -e
        let existing_shas: Vec<String> = sha_vec
            .into_iter()
            .filter(|sha| {
                let mut args = repo.global_args_for_exec();
                args.push("cat-file".to_string());
                args.push("-e".to_string());
                args.push(sha.clone());
                
                let exists = exec_git(&args).is_ok();
                if !exists {
                    eprintln!("[DEBUG] Filtering out non-existent commit: {}", sha);
                }
                exists
            })
            .collect();
        
        eprintln!("[DEBUG] After filtering: {} existing commits", existing_shas.len());
        for sha in &existing_shas {
            eprintln!("[DEBUG]   - {}", sha);
        }
        
        if existing_shas.is_empty() {
            return Err(GitAiError::Generic(
                "No existing commits found in git notes for pattern".to_string()
            ));
        }
        
        if existing_shas.len() == 1 {
            return Ok(existing_shas);
        }
        
        // Sort only existing commits by date
        let mut args = repo.global_args_for_exec();
        args.push("log".to_string());
        args.push("--format=%H".to_string());
        args.push("--date-order".to_string());
        args.push("--no-walk".to_string());
        for sha in &existing_shas {
            args.push(sha.clone());
        }

        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| GitAiError::Generic("Failed to parse git log output".to_string()))?;

        Ok(stdout.lines().map(|s| s.to_string()).collect())
    } else {
        // Single or no result - verify it exists
        let sha_vec: Vec<String> = shas.into_iter().collect();
        if !sha_vec.is_empty() {
            let sha = &sha_vec[0];
            let mut args = repo.global_args_for_exec();
            args.push("cat-file".to_string());
            args.push("-e".to_string());
            args.push(sha.clone());
            
            if exec_git(&args).is_ok() {
                Ok(sha_vec)
            } else {
                eprintln!("[DEBUG] Single commit {} does not exist", sha);
                Err(GitAiError::Generic(
                    "Commit referenced in git notes does not exist".to_string()
                ))
            }
        } else {
            Ok(sha_vec)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_parse_batch_check_blob_oid_accepts_sha1_and_sha256() {
        let sha1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 10";
        let sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb blob 20";
        let invalid = "cccccccc blob 10";

        assert_eq!(
            parse_batch_check_blob_oid(sha1),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())
        );
        assert_eq!(
            parse_batch_check_blob_oid(sha256),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string())
        );
        assert_eq!(parse_batch_check_blob_oid(invalid), None);
    }

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
    fn test_notes_add_batch_writes_multiple_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        let entries = vec![
            (commit_a.clone(), "{\"note\":\"a\",\"value\":1}".to_string()),
            (commit_b.clone(), "{\"note\":\"b\",\"value\":2}".to_string()),
        ];

        notes_add_batch(tmp_repo.gitai_repo(), &entries).expect("batch notes add");

        let note_a = show_authorship_note(tmp_repo.gitai_repo(), &commit_a).expect("note A");
        let note_b = show_authorship_note(tmp_repo.gitai_repo(), &commit_b).expect("note B");
        assert!(note_a.contains("\"note\":\"a\""));
        assert!(note_b.contains("\"note\":\"b\""));
    }

    #[test]
    fn test_notes_add_blob_batch_reuses_existing_note_blob() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = commit_a.clone();
        let note_content = log.serialize_to_string().expect("serialize authorship log");
        notes_add(tmp_repo.gitai_repo(), &commit_a, &note_content).expect("add note A");

        let blob_oids =
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_a))
                .expect("resolve note blob oid");
        let blob_oid = blob_oids
            .get(&commit_a)
            .expect("blob oid for commit A")
            .clone();

        let blob_entry = (commit_b.clone(), blob_oid);
        notes_add_blob_batch(tmp_repo.gitai_repo(), std::slice::from_ref(&blob_entry))
            .expect("batch add blob-backed note");

        let raw_note_b = show_authorship_note(tmp_repo.gitai_repo(), &commit_b).expect("note B");
        assert_eq!(raw_note_b, note_content);

        let parsed_note_b =
            get_reference_as_authorship_log_v3(tmp_repo.gitai_repo(), &commit_b).expect("parse B");
        assert_eq!(parsed_note_b.metadata.base_commit_sha, commit_b);
    }

    #[test]
    fn test_sanitize_remote_name() {
        assert_eq!(sanitize_remote_name("origin"), "origin");
        assert_eq!(sanitize_remote_name("my-remote"), "my-remote");
        assert_eq!(sanitize_remote_name("remote_123"), "remote_123");
        assert_eq!(
            sanitize_remote_name("remote/with/slashes"),
            "remote_with_slashes"
        );
        assert_eq!(
            sanitize_remote_name("remote@with#special$chars"),
            "remote_with_special_chars"
        );
        assert_eq!(sanitize_remote_name("has spaces"), "has_spaces");
    }

    #[test]
    fn test_tracking_ref_for_remote() {
        assert_eq!(
            tracking_ref_for_remote("origin"),
            "refs/notes/ai-remote/origin"
        );
        assert_eq!(
            tracking_ref_for_remote("upstream"),
            "refs/notes/ai-remote/upstream"
        );
        assert_eq!(
            tracking_ref_for_remote("my-fork"),
            "refs/notes/ai-remote/my-fork"
        );
        // Special characters get sanitized
        assert_eq!(
            tracking_ref_for_remote("remote/with/slashes"),
            "refs/notes/ai-remote/remote_with_slashes"
        );
    }

    #[test]
    fn test_ref_exists() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create initial commit
        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("commit");

        // HEAD should exist
        assert!(ref_exists(tmp_repo.gitai_repo(), "HEAD"));

        // refs/heads/main (or master) should exist
        let branch_name = tmp_repo.current_branch().expect("get branch");
        assert!(ref_exists(
            tmp_repo.gitai_repo(),
            &format!("refs/heads/{}", branch_name)
        ));

        // Non-existent ref should not exist
        assert!(!ref_exists(
            tmp_repo.gitai_repo(),
            "refs/heads/nonexistent-branch"
        ));
        assert!(!ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-test"));
    }

    #[test]
    fn test_merge_notes_from_ref() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commits - they will auto-create notes on refs/notes/ai
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        // Create a third commit without checkpoint to ensure we have a commit without notes
        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");

        // Manually create commit without checkpoint
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&["add".to_string(), ".".to_string()]);
        crate::git::repository::exec_git(&args).expect("add files");

        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "commit".to_string(),
            "-m".to_string(),
            "Commit C".to_string(),
        ]);
        crate::git::repository::exec_git(&args).expect("commit");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Add note to commit C on a different ref
        let note_c = "{\"note\":\"c\"}";
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "notes".to_string(),
            "--ref=test".to_string(),
            "add".to_string(),
            "-f".to_string(),
            "-m".to_string(),
            note_c.to_string(),
            commit_c.clone(),
        ]);
        crate::git::repository::exec_git(&args).expect("add note C on test ref");

        // Verify initial state - commit C should not have note on refs/notes/ai
        let initial_note_c = show_authorship_note(tmp_repo.gitai_repo(), &commit_c);

        // Merge notes from refs/notes/test into refs/notes/ai
        merge_notes_from_ref(tmp_repo.gitai_repo(), "refs/notes/test").expect("merge notes");

        // After merge, commit C should have a note on refs/notes/ai
        let final_note_c = show_authorship_note(tmp_repo.gitai_repo(), &commit_c);

        // If initially had no note, should now have one. If it had one, should still have one.
        assert!(final_note_c.is_some() || initial_note_c.is_some());
    }

    #[test]
    fn test_copy_ref() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commit with note
        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note_content = "{\"test\":\"note\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note_content).expect("add note");

        // refs/notes/ai should exist
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai"));

        // refs/notes/ai-backup should not exist
        assert!(!ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-backup"));

        // Copy refs/notes/ai to refs/notes/ai-backup
        copy_ref(
            tmp_repo.gitai_repo(),
            "refs/notes/ai",
            "refs/notes/ai-backup",
        )
        .expect("copy ref");

        // Both should now exist and point to the same commit
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai"));
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-backup"));

        // Verify content is accessible from both refs
        let note_from_ai =
            show_authorship_note(tmp_repo.gitai_repo(), &commit_sha).expect("note from ai");

        // Read from backup ref
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "notes".to_string(),
            "--ref=ai-backup".to_string(),
            "show".to_string(),
            commit_sha.clone(),
        ]);
        let output = crate::git::repository::exec_git(&args).expect("show note from backup");
        let note_from_backup = String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string();

        assert_eq!(note_from_ai, note_from_backup);
    }

    #[test]
    fn test_grep_ai_notes_single_match() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note = "{\"tool\":\"cursor\",\"model\":\"claude-3-sonnet\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note).expect("add note");

        // Search for "cursor" should find the commit
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor").expect("grep");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], commit_sha);
    }

    #[test]
    fn test_grep_ai_notes_multiple_matches() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create three commits with notes
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");
        tmp_repo.commit_with_message("Commit C").expect("commit C");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Add notes with "cursor" to all three
        notes_add(tmp_repo.gitai_repo(), &commit_a, "{\"tool\":\"cursor\"}").expect("add note A");
        notes_add(tmp_repo.gitai_repo(), &commit_b, "{\"tool\":\"cursor\"}").expect("add note B");
        notes_add(tmp_repo.gitai_repo(), &commit_c, "{\"tool\":\"cursor\"}").expect("add note C");

        // Search should find all three, sorted by commit date (newest first)
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor").expect("grep");

        // Should find at least 3 commits (may find more from auto-created notes)
        assert!(
            results.len() >= 3,
            "Expected at least 3 results, got {}",
            results.len()
        );

        // Verify our three commits are in the results
        assert!(
            results.contains(&commit_a),
            "Results should contain commit A"
        );
        assert!(
            results.contains(&commit_b),
            "Results should contain commit B"
        );
        assert!(
            results.contains(&commit_c),
            "Results should contain commit C"
        );
    }

    #[test]
    fn test_grep_ai_notes_no_match() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note = "{\"tool\":\"cursor\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note).expect("add note");

        // Search for non-existent pattern
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "vscode");
        // grep may return empty or error if no matches, both are acceptable
        match results {
            Ok(refs) => assert_eq!(refs.len(), 0),
            Err(_) => {} // Also acceptable - git grep returns non-zero when no matches
        }
    }

    #[test]
    fn test_grep_ai_notes_no_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");

        // No notes exist, search should return empty or error
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor");
        // grep may return empty or error if refs/notes/ai doesn't exist
        match results {
            Ok(refs) => assert_eq!(refs.len(), 0),
            Err(_) => {} // Also acceptable - refs/notes/ai may not exist yet
        }
    }

    #[test]
    fn test_get_commits_with_notes_from_list() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commits - commit_with_message auto-creates authorship notes,
        // so all commits will have notes. This is expected behavior.
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");
        tmp_repo.commit_with_message("Commit C").expect("commit C");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Get authorship for all commits
        let commit_list = vec![commit_a.clone(), commit_b.clone(), commit_c.clone()];
        let result = get_commits_with_notes_from_list(tmp_repo.gitai_repo(), &commit_list)
            .expect("get commits");

        assert_eq!(result.len(), 3);

        // All commits should have logs since commit_with_message creates them
        for (idx, commit_authorship) in result.iter().enumerate() {
            match commit_authorship {
                CommitAuthorship::Log {
                    sha,
                    git_author: _,
                    authorship_log: _,
                } => {
                    // This is expected - verify SHA matches
                    let expected_sha = &commit_list[idx];
                    assert_eq!(sha, expected_sha);
                }
                CommitAuthorship::NoLog { .. } => {
                    // Also acceptable if checkpoint system didn't run
                }
            }
        }
    }

    #[test]
    fn test_notes_path_for_object() {
        // Short SHA (edge case)
        assert_eq!(notes_path_for_object("a"), "a");
        assert_eq!(notes_path_for_object("ab"), "ab");

        // Normal SHA (40 chars)
        assert_eq!(
            notes_path_for_object("abcdef1234567890abcdef1234567890abcdef12"),
            "ab/cdef1234567890abcdef1234567890abcdef12"
        );

        // SHA-256 (64 chars)
        assert_eq!(
            notes_path_for_object(
                "abc1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
            ),
            "ab/c1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
        );
    }

    #[test]
    fn test_flat_note_pathspec_for_commit() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let pathspec = flat_note_pathspec_for_commit(sha);
        assert_eq!(
            pathspec,
            "refs/notes/ai:abcdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn test_fanout_note_pathspec_for_commit() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let pathspec = fanout_note_pathspec_for_commit(sha);
        assert_eq!(
            pathspec,
            "refs/notes/ai:ab/cdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn test_note_blob_oids_for_commits_empty() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Empty list should return empty map
        let result = note_blob_oids_for_commits(tmp_repo.gitai_repo(), &[]).expect("empty list");
        assert!(result.is_empty());
    }

    #[test]
    #[ignore] // Checkpoint system auto-creates notes, making this assertion invalid
    fn test_note_blob_oids_for_commits_no_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Commit exists but has no note
        let result =
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), &[commit_sha]).expect("no notes");
        assert!(result.is_empty());
    }

    #[test]
    fn test_commits_with_authorship_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        // Both commits may already have notes from commit_with_message
        // Add a custom note to A to ensure it has one
        notes_add(tmp_repo.gitai_repo(), &commit_a, "{\"test\":\"note\"}").expect("add note");

        let commits = vec![commit_a.clone(), commit_b.clone()];
        let result =
            commits_with_authorship_notes(tmp_repo.gitai_repo(), &commits).expect("check notes");

        // Commit A should definitely be in results
        assert!(result.contains(&commit_a), "Commit A should have a note");

        // Commit B may or may not have a note depending on checkpoint system
        // Just verify we got at least 1 result (commit A)
        assert!(
            result.len() >= 1,
            "Should have at least 1 commit with notes"
        );
    }

    #[test]
    fn test_get_reference_as_working_log() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Add a working log format note
        let working_log_json = "[]";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, working_log_json).expect("add note");

        let result = get_reference_as_working_log(tmp_repo.gitai_repo(), &commit_sha)
            .expect("get working log");
        assert_eq!(result.len(), 0); // Empty array
    }

    #[test]
    fn test_get_reference_as_authorship_log_v3_version_mismatch() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Create log with wrong version
        let mut log = AuthorshipLog::new();
        log.metadata.schema_version = "999".to_string();
        log.metadata.base_commit_sha = commit_sha.clone();

        let note_content = log.serialize_to_string().expect("serialize");
        notes_add(tmp_repo.gitai_repo(), &commit_sha, &note_content).expect("add note");

        // Should fail with version mismatch error
        let result = get_reference_as_authorship_log_v3(tmp_repo.gitai_repo(), &commit_sha);
        assert!(result.is_err());

        if let Err(GitAiError::Generic(msg)) = result {
            assert!(msg.contains("Unsupported authorship log version"));
        } else {
            panic!("Expected version mismatch error");
        }
    }
}
