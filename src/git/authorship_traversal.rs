use std::collections::HashSet;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};

pub async fn load_ai_touched_files_for_commits(
    repo: &Repository,
    commit_shas: Vec<String>,
) -> Result<HashSet<String>, GitAiError> {
    let global_args = repo.global_args_for_exec();

    smol::unblock(move || {
        if commit_shas.is_empty() {
            return Ok(HashSet::new());
        }

        // Get all notes mappings (note_sha -> commit_sha) using git notes list
        let note_mappings = get_notes_list(&global_args)?;

        if note_mappings.is_empty() {
            return Ok(HashSet::new());
        }

        // Filter to only notes for commits we care about
        let commit_set: HashSet<&str> = commit_shas.iter().map(|s| s.as_str()).collect();
        let filtered_blob_shas: Vec<String> = note_mappings
            .into_iter()
            .filter(|(_, commit_sha)| commit_set.contains(commit_sha.as_str()))
            .map(|(note_sha, _)| note_sha)
            .collect();

        if filtered_blob_shas.is_empty() {
            return Ok(HashSet::new());
        }

        // Use cat-file --batch to read the filtered blobs efficiently
        let blob_contents = batch_read_blobs(&global_args, &filtered_blob_shas)?;

        // Extract file paths from all blob contents
        let mut all_files = HashSet::new();
        for content in blob_contents {
            extract_file_paths_from_note(&content, &mut all_files);
        }

        Ok(all_files)
    })
    .await
}

/// Return true if any of the provided commits has an authorship note attached.
pub fn commits_have_authorship_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<bool, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(false);
    }

    let global_args = repo.global_args_for_exec();
    let note_mappings = get_notes_list(&global_args)?;
    if note_mappings.is_empty() {
        return Ok(false);
    }

    let commit_set: HashSet<&str> = commit_shas.iter().map(|s| s.as_str()).collect();
    Ok(note_mappings
        .iter()
        .any(|(_, commit_sha)| commit_set.contains(commit_sha.as_str())))
}

/// Get all notes as (note_blob_sha, commit_sha) pairs
fn get_notes_list(global_args: &[String]) -> Result<Vec<(String, String)>, GitAiError> {
    let mut args = global_args.to_vec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("list".to_string());

    let output = match exec_git(&args) {
        Ok(output) => output,
        Err(GitAiError::GitCliError { code: Some(1), .. }) => {
            // No notes exist yet
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };

    let stdout = String::from_utf8(output.stdout)?;

    // Parse notes list output: "<note_blob_sha> <commit_sha>"
    let mut mappings = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            mappings.push((parts[0].to_string(), parts[1].to_string()));
        }
    }

    Ok(mappings)
}

/// Read multiple blobs efficiently using cat-file --batch
fn batch_read_blobs(
    global_args: &[String],
    blob_shas: &[String],
) -> Result<Vec<String>, GitAiError> {
    if blob_shas.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = global_args.to_vec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    // Prepare stdin: one SHA per line
    let stdin_data = blob_shas.join("\n") + "\n";

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;

    // Parse batch output
    // Format for each object:
    // <sha> <type> <size>\n
    // <content>\n
    parse_cat_file_batch_output(&output.stdout)
}

/// Parse the output of git cat-file --batch
///
/// Format:
/// <sha> <type> <size>\n
/// <content bytes>\n
/// (repeat for each object)
fn parse_cat_file_batch_output(data: &[u8]) -> Result<Vec<String>, GitAiError> {
    let mut results = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Find the header line ending with \n
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end])
            .map_err(|e| GitAiError::Generic(format!("Invalid UTF-8 in header: {}", e)))?;

        // Parse header: "<sha> <type> <size>" or "<sha> missing"
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 2 {
            pos = header_end + 1;
            continue;
        }

        if parts[1] == "missing" {
            // Object doesn't exist, skip
            pos = header_end + 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            continue;
        }

        let size: usize = parts[2]
            .parse()
            .map_err(|e| GitAiError::Generic(format!("Invalid size in cat-file output: {}", e)))?;

        // Content starts after the header newline
        let content_start = header_end + 1;
        let content_end = content_start + size;

        if content_end > data.len() {
            break;
        }

        // Try to parse content as UTF-8
        if let Ok(content) = std::str::from_utf8(&data[content_start..content_end]) {
            results.push(content.to_string());
        }

        // Move past content and the trailing newline
        pos = content_end + 1;
    }

    Ok(results)
}

/// Extract file paths from a note blob content
fn extract_file_paths_from_note(content: &str, files: &mut HashSet<String>) {
    // Find the divider and slice before it, then add minimal metadata to make it parseable
    if let Some(divider_pos) = content.find("\n---\n") {
        let attestation_section = &content[..divider_pos];
        // Create a complete parseable format with empty metadata
        let parseable = format!(
            "{}\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"base_commit_sha\":\"\",\"prompts\":{{}}}}",
            attestation_section
        );

        if let Ok(log) = AuthorshipLog::deserialize_from_string(&parseable) {
            for attestation in log.attestations {
                files.insert(attestation.file_path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{find_repository_in_path, sync_authorship::fetch_authorship_notes};
    use std::time::Instant;

    #[test]
    fn test_load_ai_touched_files_for_specific_commits() {
        smol::block_on(async {
            let repo = find_repository_in_path(".").unwrap();

            if let Err(err) = fetch_authorship_notes(&repo, "origin") {
                println!(
                    "Skipping test: failed to fetch authorship notes from origin: {}",
                    err
                );
                return;
            }

            // Get all notes to find commits that have notes attached
            let global_args = repo.global_args_for_exec();
            let all_notes = get_notes_list(&global_args).unwrap();

            if all_notes.len() < 3 {
                println!(
                    "Skipping test: only {} notes available, need at least 3",
                    all_notes.len()
                );
                return;
            }

            // Pick 3 commits that have notes
            let selected_commits: Vec<String> = all_notes
                .iter()
                .take(3)
                .map(|(_, commit_sha)| commit_sha.clone())
                .collect();

            println!("Testing with commits: {:?}", selected_commits);

            let start = Instant::now();
            let files = load_ai_touched_files_for_commits(&repo, selected_commits.clone())
                .await
                .unwrap();
            let elapsed = start.elapsed();

            println!(
                "Found {} unique AI-touched files from 3 commits in {:?}",
                files.len(),
                elapsed
            );

            // Show the files found
            let mut sorted_files: Vec<_> = files.iter().collect();
            sorted_files.sort();
            for file in sorted_files.iter() {
                println!("  {}", file);
            }

            // Verify we got some results (since we picked commits with notes)
            assert!(
                !files.is_empty(),
                "Should find files from commits with notes"
            );
        });
    }

    #[test]
    fn test_load_ai_touched_files_for_nonexistent_commit() {
        smol::block_on(async {
            let repo = find_repository_in_path(".").unwrap();

            // Use a fake SHA that doesn't exist
            let fake_commits = vec![
                "0000000000000000000000000000000000000000".to_string(),
                "1111111111111111111111111111111111111111".to_string(),
            ];

            let files = load_ai_touched_files_for_commits(&repo, fake_commits)
                .await
                .unwrap();

            // Should return empty set, not crash
            assert!(
                files.is_empty(),
                "Should return empty set for non-existent commits"
            );
        });
    }
}
