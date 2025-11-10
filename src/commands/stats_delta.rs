use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::refs::show_authorship_note;
use crate::git::repo_storage::RepoStorage;
use crate::git::repository::Repository;
use std::collections::HashMap;
use std::collections::HashSet;

pub fn run(repo: &Repository, json_output: bool) -> Result<(), GitAiError> {
    // Find all working log refs
    let working_log_refs = find_working_log_refs(repo)?;

    // Filter to only show refs with > 0 checkpoints
    let mut filtered_refs: HashMap<String, usize> = working_log_refs
        .into_iter()
        .filter(|(_, checkpoint_count)| *checkpoint_count > 0)
        .collect();

    // Build reverse lookup map for child commits (much faster than checking each commit individually)
    let mut parent_to_children: HashMap<String, Vec<String>> = HashMap::new();

    // Get all references and build the parent->children map
    let refs = repo.references()?;
    let mut processed_commits = std::collections::HashSet::new();

    for reference in refs {
        let reference = reference?;
        let ref_name = reference.name().unwrap_or("");

        // Skip ai-working-log refs to avoid self-references
        if ref_name.starts_with("refs/ai-working-log/") {
            continue;
        }

        // Check if this ref points to a commit
        if let Ok(commit) = reference.peel_to_commit() {
            // Limit to last 500 commits across all branches
            if processed_commits.len() >= 500 {
                continue;
            }

            let commit_id = commit.id().to_string();
            if processed_commits.insert(commit_id.clone()) {
                // Add this commit as a child of each of its parents
                for parent in commit.parents() {
                    let parent_id = parent.id().to_string();
                    parent_to_children
                        .entry(parent_id)
                        .or_insert_with(Vec::new)
                        .push(commit_id.clone());
                }
            }
        }
    }

    // Filter using the pre-built map
    filtered_refs.retain(|commit_hash, _| parent_to_children.contains_key(commit_hash));

    if filtered_refs.is_empty() {
        // No commits with children found - this is normal if the commits with working logs
        // are the most recent commits or if the repository history has changed
        return Ok(());
    }

    // Sort by commit date (most recent first)
    let mut sorted_refs: Vec<(String, usize, i64)> = Vec::new();
    for (commit_hash, checkpoint_count) in filtered_refs {
        // TODO This should probably be optimized to be done in a single call to git
        // Get commit date
        let commit_time = match repo.revparse_single(&commit_hash) {
            Ok(obj) => {
                if let Ok(commit) = obj.peel_to_commit() {
                    commit.time()?.seconds()
                } else {
                    0 // fallback
                }
            }
            Err(_) => 0, // fallback
        };
        sorted_refs.push((commit_hash, checkpoint_count, commit_time));
    }

    // Sort by commit time (most recent first)
    sorted_refs.sort_by(|a, b| b.2.cmp(&a.2));

    // Extract commit hashes for later use
    let commit_hashes: Vec<String> = sorted_refs
        .iter()
        .map(|(hash, _, _)| hash.clone())
        .collect();

    // Create authorship logs for direct children that don't already have one
    let mut authorship_logs: HashMap<String, AuthorshipLog> = HashMap::new();

    // Initialize the storage system once
    let repo_storage = RepoStorage::for_repo_path(repo.path(), &repo.workdir()?);

    for commit_hash in &commit_hashes {
        // Get the working log for this commit
        let working_log = repo_storage.working_log_for_base_commit(commit_hash);
        let checkpoints = match working_log.read_all_checkpoints() {
            Ok(working_log_data) => working_log_data,
            Err(_) => continue, // Skip if we can't get the working log
        };

        // Get direct children of this commit
        let empty_vec = Vec::new();
        let children = parent_to_children.get(commit_hash).unwrap_or(&empty_vec);

        if children.is_empty() {
            continue;
        }

        for child_commit in children {
            // Check if authorship log already exists for this child
            if show_authorship_note(repo, child_commit).is_none() {
                // No authorship log exists, create one using the new flow

                // Create VirtualAttributions from working log (similar to post_commit flow)
                let working_va = VirtualAttributions::from_just_working_log(
                    repo.clone(),
                    commit_hash.clone(),
                    None, // No human author specified in backfill
                )?;

                // Get pathspecs for files in the working log
                let pathspecs: HashSet<String> = checkpoints
                    .iter()
                    .flat_map(|cp| cp.entries.iter().map(|e| e.file.clone()))
                    .collect();

                // Split into committed (authorship log) and uncommitted (INITIAL)
                let (mut authorship_log, _initial_attributions) = working_va
                    .to_authorship_log_and_initial_working_log(
                        repo,
                        commit_hash,
                        child_commit,
                        Some(&pathspecs),
                    )?;

                authorship_log.metadata.base_commit_sha = child_commit.clone();

                // Serialize the authorship log
                let authorship_json = authorship_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize authorship log".to_string())
                })?;

                // Create the authorship log note
                notes_add(repo, child_commit, &authorship_json)?;

                if json_output {
                    // Store the authorship log for JSON output
                    authorship_logs.insert(child_commit.clone(), authorship_log);
                } else {
                    // Print individual ref as before
                    println!("notes/ai/{}", child_commit);
                }
            }
        }
    }

    // Output JSON if requested
    if json_output && !authorship_logs.is_empty() {
        // Convert HashMap to a serializable format
        let mut json_map = serde_json::Map::new();
        for (key, v3_log) in authorship_logs {
            let serialized = v3_log.serialize_to_string().map_err(|_| {
                GitAiError::Generic("Failed to serialize authorship log".to_string())
            })?;
            json_map.insert(key, serde_json::Value::String(serialized));
        }
        let json_output = serde_json::to_string(&json_map)?;
        println!("{}", json_output);
    }

    // Delete working logs after creating authorship logs
    for commit_hash in &commit_hashes {
        let empty_vec = Vec::new();
        let children = parent_to_children.get(commit_hash).unwrap_or(&empty_vec);

        let all_children_have_authorship = children
            .iter()
            .all(|child| show_authorship_note(repo, child).is_some());

        if all_children_have_authorship && !children.is_empty() {
            // Delete the working log using the new storage system
            repo_storage.delete_working_log_for_base_commit(commit_hash)?;
        }
    }

    Ok(())
}

fn find_working_log_refs(repo: &Repository) -> Result<HashMap<String, usize>, GitAiError> {
    let mut working_log_refs = HashMap::new();

    // Initialize the new storage system
    let repo_storage = RepoStorage::for_repo_path(repo.path(), &repo.workdir()?);

    // Check if the working logs directory exists
    if !repo_storage.working_logs.exists() {
        return Ok(working_log_refs);
    }

    // Read all subdirectories in the working logs directory
    let entries = std::fs::read_dir(&repo_storage.working_logs)?;

    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let base_commit = entry.file_name().to_string_lossy().to_string();
            let working_log = repo_storage.working_log_for_base_commit(&base_commit);

            match working_log.read_all_checkpoints() {
                Ok(working_log_data) => {
                    working_log_refs.insert(base_commit, working_log_data.len());
                }
                Err(_) => {
                    // If we can't read the checkpoints, still include it but with 0 count
                    working_log_refs.insert(base_commit, 0);
                }
            }
        }
    }

    Ok(working_log_refs)
}

/// Get file contents from a commit tree for specified pathspecs
fn get_committed_files_content(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;

    let mut files = HashMap::new();

    for file_path in pathspecs {
        match tree.get_path(std::path::Path::new(file_path)) {
            Ok(entry) => {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let blob_content = blob.content().unwrap_or_default();
                    let content = String::from_utf8_lossy(&blob_content).to_string();
                    files.insert(file_path.clone(), content);
                }
            }
            Err(_) => {
                // File doesn't exist in this commit (could be deleted), skip it
            }
        }
    }

    Ok(files)
}
