use std::collections::HashSet;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::error::GitAiError;
use crate::git::refs::{commits_with_authorship_notes, note_blob_oids_for_commits};
#[cfg(test)]
use crate::git::repository::exec_git;
use crate::git::repository::{Repository, exec_git_stdin};

pub async fn load_ai_touched_files_for_commits(
    repo: &Repository,
    commit_shas: Vec<String>,
) -> Result<HashSet<String>, GitAiError> {
    let repo = repo.clone();

    smol::unblock(move || {
        if commit_shas.is_empty() {
            return Ok(HashSet::new());
        }

        let note_blob_map = note_blob_oids_for_commits(&repo, &commit_shas)?;
        if note_blob_map.is_empty() {
            return Ok(HashSet::new());
        }

        let mut unique_blob_oids = HashSet::new();
        for blob_oid in note_blob_map.values() {
            unique_blob_oids.insert(blob_oid.clone());
        }
        let mut blob_oids: Vec<String> = unique_blob_oids.into_iter().collect();
        blob_oids.sort();

        let blob_contents = batch_read_blobs_with_oids(&repo.global_args_for_exec(), &blob_oids)?;

        let mut all_files = HashSet::new();
        for blob_oid in note_blob_map.into_values() {
            if let Some(content) = blob_contents.get(&blob_oid) {
                extract_file_paths_from_note(content, &mut all_files);
            }
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

    Ok(!commits_with_authorship_notes(repo, commit_shas)?.is_empty())
}

/// Get all notes as (note_blob_sha, commit_sha) pairs
#[cfg(test)]
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

fn batch_read_blobs_with_oids(
    global_args: &[String],
    blob_oids: &[String],
) -> Result<std::collections::HashMap<String, String>, GitAiError> {
    if blob_oids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let mut args = global_args.to_vec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = blob_oids.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;

    parse_cat_file_batch_output_with_oids(&output.stdout)
}

fn parse_cat_file_batch_output_with_oids(
    data: &[u8],
) -> Result<std::collections::HashMap<String, String>, GitAiError> {
    let mut results = std::collections::HashMap::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end])?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 2 {
            pos = header_end + 1;
            continue;
        }

        let oid = parts[0].to_string();
        if parts[1] == "missing" {
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

        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            return Err(GitAiError::Generic(
                "Malformed cat-file --batch output: truncated content".to_string(),
            ));
        }

        let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
        results.insert(oid, content);

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
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
