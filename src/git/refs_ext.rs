pub use super::refs_impl::{
    AI_AUTHORSHIP_FORK_TRACKING_REF, AI_AUTHORSHIP_FULL_REF, AI_AUTHORSHIP_PUSH_REFSPEC,
    AI_AUTHORSHIP_REFNAME, CommitAuthorship, copy_ref, fallback_merge_notes_ours,
    fanout_note_pathspec_for_commit, fanout_note_pathspec_for_ref, flat_note_pathspec_for_commit,
    flat_note_pathspec_for_ref, get_authorship, get_commits_with_notes_from_list,
    get_reference_as_authorship_log_v3, get_reference_as_working_log, grep_ai_notes,
    merge_notes_from_ref, notes_path_for_object, parse_batch_check_blob_oid, ref_exists,
    sanitize_remote_name, show_authorship_note, tracking_ref_for_remote,
};

use crate::error::GitAiError;
use crate::git::refs_impl;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use std::collections::{HashMap, HashSet};

#[doc(hidden)]
pub fn note_paths_for_object(oid: &str) -> Vec<String> {
    let mut paths = vec![oid.to_string()];

    let two_level = refs_impl::notes_path_for_object(oid);
    if !paths.contains(&two_level) {
        paths.push(two_level);
    }

    if oid.len() > 4 {
        let three_level = format!("{}/{}/{}", &oid[..2], &oid[2..4], &oid[4..]);
        if !paths.contains(&three_level) {
            paths.push(three_level);
        }
    }

    paths
}

fn parse_cat_file_batch_output_with_oids(
    data: &[u8],
) -> Result<HashMap<String, String>, GitAiError> {
    let mut results = HashMap::new();
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
            .map_err(|e| GitAiError::Generic(format!("Invalid size in cat-file output: {e}")))?;

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

fn batch_read_blob_contents(
    repo: &Repository,
    blob_oids: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if blob_oids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = blob_oids.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    parse_cat_file_batch_output_with_oids(&output.stdout)
}

pub fn notes_add(
    repo: &Repository,
    commit_sha: &str,
    note_content: &str,
) -> Result<(), GitAiError> {
    notes_add_batch(repo, &[(commit_sha.to_string(), note_content.to_string())])
}

pub fn note_blob_oids_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    note_blob_oids_for_commits_from_ref(repo, refs_impl::AI_AUTHORSHIP_FULL_REF, commit_shas)
}

pub fn note_blob_oids_for_commits_from_ref(
    repo: &Repository,
    notes_ref: &str,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch-check".to_string());

    let mut stdin_data = String::new();
    let mut path_counts = Vec::with_capacity(commit_shas.len());
    for commit_sha in commit_shas {
        let paths = note_paths_for_object(commit_sha);
        path_counts.push(paths.len());
        for path in paths {
            stdin_data.push_str(&format!("{}:{}", notes_ref, path));
            stdin_data.push('\n');
        }
    }

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let mut result = HashMap::new();

    for (commit_sha, path_count) in commit_shas.iter().zip(path_counts) {
        let mut note_oid = None;
        for _ in 0..path_count {
            let Some(line) = lines.next() else {
                break;
            };
            if note_oid.is_none() {
                note_oid = refs_impl::parse_batch_check_blob_oid(line);
            }
        }

        if let Some(oid) = note_oid {
            result.insert(commit_sha.clone(), oid);
        }
    }

    Ok(result)
}

pub fn copy_missing_notes_for_commits_from_ref(
    repo: &Repository,
    source_ref: &str,
    commit_shas: &[String],
) -> Result<usize, GitAiError> {
    if commit_shas.is_empty() || !refs_impl::ref_exists(repo, source_ref) {
        return Ok(0);
    }

    let source_note_oids = note_blob_oids_for_commits_from_ref(repo, source_ref, commit_shas)?;
    if source_note_oids.is_empty() {
        return Ok(0);
    }

    let local_note_oids = note_blob_oids_for_commits(repo, commit_shas)?;
    let entries: Vec<(String, String)> = commit_shas
        .iter()
        .filter(|commit_sha| !local_note_oids.contains_key(*commit_sha))
        .filter_map(|commit_sha| {
            source_note_oids
                .get(commit_sha)
                .map(|blob_oid| (commit_sha.clone(), blob_oid.clone()))
        })
        .collect();

    let copied = entries.len();
    notes_add_blob_batch(repo, &entries)?;
    Ok(copied)
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
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {e}")))?
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
        let fanout_path = refs_impl::notes_path_for_object(commit_sha);
        for old_path in note_paths_for_object(commit_sha) {
            script.extend_from_slice(format!("D {}\n", old_path).as_bytes());
        }
        script.extend_from_slice(format!("M 100644 :{} {}\n", idx + 1, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;
    crate::authorship::git_ai_hooks::post_notes_updated(repo, &deduped_entries);

    Ok(())
}

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
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {e}")))?
        .as_secs();

    let mut script = Vec::<u8>::new();
    script.extend_from_slice(b"commit refs/notes/ai\n");
    script.extend_from_slice(format!("committer git-ai <git-ai@local> {} +0000\n", now).as_bytes());
    script.extend_from_slice(b"data 0\n");
    if let Some(existing_tip) = existing_notes_tip {
        script.extend_from_slice(format!("from {}\n", existing_tip).as_bytes());
    }

    for (commit_sha, blob_oid) in &deduped_entries {
        let fanout_path = refs_impl::notes_path_for_object(commit_sha);
        for old_path in note_paths_for_object(commit_sha) {
            script.extend_from_slice(format!("D {}\n", old_path).as_bytes());
        }
        script.extend_from_slice(format!("M 100644 {} {}\n", blob_oid, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;

    let has_post_notes_updated_hooks = crate::config::Config::get()
        .git_ai_hook_commands("post_notes_updated")
        .is_some_and(|commands| !commands.is_empty());
    if has_post_notes_updated_hooks {
        let hook_entries = (|| -> Result<Vec<(String, String)>, GitAiError> {
            let mut unique_blob_oids: Vec<String> = deduped_entries
                .iter()
                .map(|(_commit_sha, blob_oid)| blob_oid.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            unique_blob_oids.sort();
            let blob_contents = batch_read_blob_contents(repo, &unique_blob_oids)?;

            Ok(deduped_entries
                .iter()
                .filter_map(|(commit_sha, blob_oid)| {
                    blob_contents
                        .get(blob_oid)
                        .map(|note_content| (commit_sha.clone(), note_content.clone()))
                })
                .collect())
        })();
        match hook_entries {
            Ok(entries) if !entries.is_empty() => {
                crate::authorship::git_ai_hooks::post_notes_updated(repo, &entries)
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(
                "Failed to prepare post_notes_updated payload for notes_add_blob_batch: {}",
                e
            ),
        }
    }

    Ok(())
}

pub fn commits_with_authorship_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashSet<String>, GitAiError> {
    Ok(note_blob_oids_for_commits(repo, commit_shas)?
        .into_keys()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_note_paths_for_object_includes_flat_two_and_three_level_paths() {
        let oid = "abcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(
            note_paths_for_object(oid),
            vec![
                "abcdef1234567890abcdef1234567890abcdef12".to_string(),
                "ab/cdef1234567890abcdef1234567890abcdef12".to_string(),
                "ab/cd/ef1234567890abcdef1234567890abcdef12".to_string(),
            ]
        );
    }
}
