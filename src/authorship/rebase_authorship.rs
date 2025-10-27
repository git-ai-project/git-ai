use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::error::GitAiError;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{Commit, Repository};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;
use std::collections::HashMap;

// Process events in the rewrite log and call the correct rewrite functions in this file
pub fn rewrite_authorship_if_needed(
    repo: &Repository,
    last_event: &RewriteLogEvent,
    commit_author: String,
    _full_log: &Vec<RewriteLogEvent>,
    supress_output: bool,
) -> Result<(), GitAiError> {
    match last_event {
        RewriteLogEvent::Commit { commit } => {
            // This is going to become the regualar post-commit
            post_commit::post_commit(
                repo,
                commit.base_commit.clone(),
                commit.commit_sha.clone(),
                commit_author,
                supress_output,
            )?;
        }
        RewriteLogEvent::CommitAmend { commit_amend } => {
            rewrite_authorship_after_commit_amend(
                repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
                commit_author,
            )?;

            debug_log(&format!(
                "Ammended commit {} now has authorship log {}",
                &commit_amend.original_commit, &commit_amend.amended_commit_sha
            ));
        }
        RewriteLogEvent::MergeSquash { merge_squash } => {
            // --squash always fails if repo is not clean
            // this clears old working logs in the event you reset, make manual changes, reset, try again
            repo.storage
                .delete_working_log_for_base_commit(&merge_squash.base_head)?;

            // Prepare INITIAL attributions from the squashed changes
            prepare_working_log_after_squash(
                repo,
                &merge_squash.source_head,
                &merge_squash.base_head,
                &commit_author,
            )?;

            debug_log(&format!(
                "✓ Prepared authorship attributions for merge --squash of {} into {}",
                merge_squash.source_branch, merge_squash.base_branch
            ));
        }
        RewriteLogEvent::RebaseComplete { rebase_complete } => {
            rewrite_authorship_after_rebase_v2(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.original_commits,
                &rebase_complete.new_commits,
                &commit_author,
            )?;

            debug_log(&format!(
                "✓ Rewrote authorship for {} rebased commits",
                rebase_complete.new_commits.len()
            ));
        }
        RewriteLogEvent::CherryPickComplete {
            cherry_pick_complete,
        } => {
            rewrite_authorship_after_cherry_pick(
                repo,
                &cherry_pick_complete.source_commits,
                &cherry_pick_complete.new_commits,
                &commit_author,
            )?;

            debug_log(&format!(
                "✓ Rewrote authorship for {} cherry-picked commits",
                cherry_pick_complete.new_commits.len()
            ));
        }
        _ => {}
    }

    Ok(())
}

/// Prepare working log after a merge --squash (before commit)
///
/// This handles the case where `git merge --squash` has staged changes but hasn't committed yet.
/// Uses VirtualAttributions to merge attributions from both branches and writes everything to INITIAL
/// since merge squash leaves all changes unstaged.
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_head_sha` - SHA of the feature branch that was squashed
/// * `target_branch_head_sha` - SHA of the current HEAD (target branch where we're merging into)
/// * `_human_author` - The human author identifier (unused in current implementation)
#[allow(dead_code)]
pub fn prepare_working_log_after_squash(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    _human_author: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };
    use std::collections::HashMap;

    // Step 1: Get list of changed files between the two branches
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--name-only".to_string());
    args.push(source_head_sha.to_string());
    args.push(target_branch_head_sha.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let changed_files: Vec<String> = String::from_utf8(output.stdout)?
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if changed_files.is_empty() {
        // No files changed, nothing to do
        return Ok(());
    }

    // Step 2: Create VirtualAttributions for both branches
    let repo_clone = repo.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.to_string(),
            &changed_files,
        )
        .await
    })?;

    // Step 3: Read staged files content (final state after squash)
    let mut staged_files: HashMap<String, String> = HashMap::new();
    for file_path in &changed_files {
        let mut args = repo.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!(":{}", file_path));

        let output = crate::git::repository::exec_git(&args);
        if let Ok(output) = output {
            if let Ok(file_content) = String::from_utf8(output.stdout) {
                staged_files.insert(file_path.clone(), file_content);
            }
        }
    }

    // Step 4: Merge VirtualAttributions, favoring target branch (HEAD)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, staged_files)?;

    // Step 5: Convert to INITIAL (everything is uncommitted in a squash)
    // Pass empty committed_files since nothing has been committed yet
    let empty_committed_files: HashMap<String, String> = HashMap::new();
    let (_authorship_log, initial_attributions) =
        merged_va.to_authorship_log_and_initial_working_log(empty_committed_files)?;

    // Step 6: Write INITIAL file
    if !initial_attributions.files.is_empty() {
        let working_log = repo
            .storage
            .working_log_for_base_commit(target_branch_head_sha);
        working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)?;
    }

    Ok(())
}

/// Rewrite authorship logs after a rebase operation using VirtualAttributions
///
/// This is the new implementation that replaces the hanging commit / blame_in_context approach.
/// It processes commits sequentially, transforming attributions through each commit in the rebase.
///
/// # Arguments
/// * `repo` - Git repository
/// * `original_head` - SHA of the HEAD before rebase
/// * `original_commits` - Vector of original commit SHAs (before rebase), oldest first
/// * `new_commits` - Vector of new commit SHAs (after rebase), oldest first
/// * `_human_author` - The human author identifier (unused in this implementation)
#[allow(dead_code)]
pub fn rewrite_authorship_after_rebase_v2(
    repo: &Repository,
    original_head: &str,
    original_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    // Handle edge case: no commits to process
    if new_commits.is_empty() {
        return Ok(());
    }

    // Step 1: Extract pathspecs from all original commits
    let pathspecs = get_pathspecs_from_commits(repo, original_commits)?;

    if pathspecs.is_empty() {
        // No files were modified, nothing to do
        return Ok(());
    }

    debug_log(&format!(
        "Processing rebase: {} files modified across {} original commits -> {} new commits",
        pathspecs.len(),
        original_commits.len(),
        new_commits.len()
    ));

    // Step 2: Create VirtualAttributions from original_head (before rebase)
    let repo_clone = repo.clone();
    let original_head_clone = original_head.to_string();
    let pathspecs_clone = pathspecs.clone();

    let mut current_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::new_for_base_commit(
            repo_clone,
            original_head_clone,
            &pathspecs_clone,
        )
        .await
    })?;

    // Clone the original VA to use as a fallback for restoring attributions
    // This handles commit splitting where content from original_head gets re-applied
    let original_va_for_fallback = {
        let mut attrs = HashMap::new();
        let mut contents = HashMap::new();
        for file in current_va.files() {
            if let Some(char_attrs) = current_va.get_char_attributions(&file) {
                if let Some(line_attrs) = current_va.get_line_attributions(&file) {
                    attrs.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
                }
            }
            if let Some(content) = current_va.get_file_content(&file) {
                contents.insert(file, content.clone());
            }
        }
        crate::authorship::virtual_attribution::VirtualAttributions::from_raw_data(
            current_va.repo().clone(),
            current_va.base_commit().to_string(),
            attrs,
            contents,
            current_va.timestamp(),
        )
    };

    // Step 3: Process each new commit in order (oldest to newest)
    for (idx, new_commit) in new_commits.iter().enumerate() {
        debug_log(&format!(
            "Processing commit {}/{}: {}",
            idx + 1,
            new_commits.len(),
            new_commit
        ));

        // Get the DIFF for this commit (what actually changed)
        let commit_obj = repo.find_commit(new_commit.clone())?;
        let parent_obj = commit_obj.parent(0)?;

        let commit_tree = commit_obj.tree()?;
        let parent_tree = parent_obj.tree()?;

        let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), None, None)?;

        // Build new content by applying the diff to current content
        let mut new_content_state = HashMap::new();

        // Start with all files from current VA
        for file in current_va.files() {
            if let Some(content) = current_va.get_file_content(&file) {
                new_content_state.insert(file, content.clone());
            }
        }

        // Apply changes from this commit's diff
        for delta in diff.deltas() {
            let file_path = delta
                .new_file()
                .path()
                .or(delta.old_file().path())
                .ok_or_else(|| GitAiError::Generic("File path not available".to_string()))?;
            let file_path_str = file_path.to_string_lossy().to_string();

            // Only process files we're tracking
            if !pathspecs.contains(&file_path_str) {
                continue;
            }

            // Get new content for this file from the commit
            let new_content = if let Ok(entry) = commit_tree.get_path(file_path) {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let content = blob.content()?;
                    String::from_utf8_lossy(&content).to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            new_content_state.insert(file_path_str, new_content);
        }

        // Transform attributions based on the new content state
        // Pass original VA to restore attributions for content that existed originally
        current_va = transform_attributions_to_final_state(
            &current_va,
            new_content_state.clone(),
            Some(&original_va_for_fallback),
        )?;

        // Convert to AuthorshipLog, but filter to only files that exist in this commit
        let mut authorship_log = current_va.to_authorship_log()?;

        // Filter out attestations for files that don't exist in this commit (empty files)
        authorship_log.attestations.retain(|attestation| {
            if let Some(content) = new_content_state.get(&attestation.file_path) {
                !content.is_empty()
            } else {
                false
            }
        });

        authorship_log.metadata.base_commit_sha = new_commit.clone();

        // Save authorship log
        let authorship_json = authorship_log
            .serialize_to_string()
            .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

        crate::git::refs::notes_add(repo, new_commit, &authorship_json)?;

        debug_log(&format!(
            "Saved authorship log for commit {} ({} files)",
            new_commit,
            authorship_log.attestations.len()
        ));
    }

    Ok(())
}

/// Rewrite authorship logs after cherry-pick using VirtualAttributions
///
/// This is the new implementation that uses VirtualAttributions to transform authorship
/// through cherry-picked commits. It's simpler than rebase since cherry-pick just applies
/// patches from source commits onto the current branch.
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_commits` - Vector of source commit SHAs (commits being cherry-picked), oldest first
/// * `new_commits` - Vector of new commit SHAs (after cherry-pick), oldest first
/// * `_human_author` - The human author identifier (unused in this implementation)
#[allow(dead_code)]
pub fn rewrite_authorship_after_cherry_pick(
    repo: &Repository,
    source_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    // Handle edge case: no commits to process
    if new_commits.is_empty() {
        debug_log("Cherry-pick resulted in no new commits");
        return Ok(());
    }

    if source_commits.is_empty() {
        debug_log("Warning: Cherry-pick with no source commits");
        return Ok(());
    }

    debug_log(&format!(
        "Processing cherry-pick: {} source commits -> {} new commits",
        source_commits.len(),
        new_commits.len()
    ));

    // Step 1: Extract pathspecs from all source commits
    let pathspecs = get_pathspecs_from_commits(repo, source_commits)?;

    if pathspecs.is_empty() {
        // No files were modified, nothing to do
        debug_log("No files modified in source commits");
        return Ok(());
    }

    debug_log(&format!(
        "Processing cherry-pick: {} files modified across {} source commits",
        pathspecs.len(),
        source_commits.len()
    ));

    // Step 2: Create VirtualAttributions from the LAST source commit
    // This is the key difference from rebase: cherry-pick applies patches sequentially,
    // so the last source commit contains all the accumulated changes being cherry-picked
    let source_head = source_commits.last().unwrap();
    let repo_clone = repo.clone();
    let source_head_clone = source_head.clone();
    let pathspecs_clone = pathspecs.clone();

    let mut current_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_clone,
            &pathspecs_clone,
        )
        .await
    })?;

    // Clone the source VA to use as a fallback for restoring attributions
    // This handles commit splitting where content from source gets re-applied
    let source_va_for_fallback = {
        let mut attrs = HashMap::new();
        let mut contents = HashMap::new();
        for file in current_va.files() {
            if let Some(char_attrs) = current_va.get_char_attributions(&file) {
                if let Some(line_attrs) = current_va.get_line_attributions(&file) {
                    attrs.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
                }
            }
            if let Some(content) = current_va.get_file_content(&file) {
                contents.insert(file, content.clone());
            }
        }
        crate::authorship::virtual_attribution::VirtualAttributions::from_raw_data(
            current_va.repo().clone(),
            current_va.base_commit().to_string(),
            attrs,
            contents,
            current_va.timestamp(),
        )
    };

    // Step 3: Process each new commit in order (oldest to newest)
    for (idx, new_commit) in new_commits.iter().enumerate() {
        debug_log(&format!(
            "Processing cherry-picked commit {}/{}: {}",
            idx + 1,
            new_commits.len(),
            new_commit
        ));

        // Get the DIFF for this commit (what actually changed)
        let commit_obj = repo.find_commit(new_commit.clone())?;
        let parent_obj = commit_obj.parent(0)?;

        let commit_tree = commit_obj.tree()?;
        let parent_tree = parent_obj.tree()?;

        let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), None, None)?;

        // Build new content by applying the diff to current content
        let mut new_content_state = HashMap::new();

        // Start with all files from current VA
        for file in current_va.files() {
            if let Some(content) = current_va.get_file_content(&file) {
                new_content_state.insert(file, content.clone());
            }
        }

        // Apply changes from this commit's diff
        for delta in diff.deltas() {
            let file_path = delta
                .new_file()
                .path()
                .or(delta.old_file().path())
                .ok_or_else(|| GitAiError::Generic("File path not available".to_string()))?;
            let file_path_str = file_path.to_string_lossy().to_string();

            // Only process files we're tracking
            if !pathspecs.contains(&file_path_str) {
                continue;
            }

            // Get new content for this file from the commit
            let new_content = if let Ok(entry) = commit_tree.get_path(file_path) {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let content = blob.content()?;
                    String::from_utf8_lossy(&content).to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            new_content_state.insert(file_path_str, new_content);
        }

        // Transform attributions based on the new content state
        // Pass source VA to restore attributions for content that existed in source
        current_va = transform_attributions_to_final_state(
            &current_va,
            new_content_state.clone(),
            Some(&source_va_for_fallback),
        )?;

        // Convert to AuthorshipLog, but filter to only files that exist in this commit
        let mut authorship_log = current_va.to_authorship_log()?;

        // Filter out attestations for files that don't exist in this commit (empty files)
        authorship_log.attestations.retain(|attestation| {
            if let Some(content) = new_content_state.get(&attestation.file_path) {
                !content.is_empty()
            } else {
                false
            }
        });

        authorship_log.metadata.base_commit_sha = new_commit.clone();

        // Save authorship log
        let authorship_json = authorship_log
            .serialize_to_string()
            .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

        crate::git::refs::notes_add(repo, new_commit, &authorship_json)?;

        debug_log(&format!(
            "Saved authorship log for cherry-picked commit {} ({} files)",
            new_commit,
            authorship_log.attestations.len()
        ));
    }

    Ok(())
}

/// Check if two commits have identical trees
#[allow(dead_code)]
fn trees_identical(commit1: &Commit, commit2: &Commit) -> Result<bool, GitAiError> {
    let tree1 = commit1.tree()?;
    let tree2 = commit2.tree()?;
    Ok(tree1.id() == tree2.id())
}

/// Copy authorship log from one commit to another
#[allow(dead_code)]
fn copy_authorship_log(repo: &Repository, from_sha: &str, to_sha: &str) -> Result<(), GitAiError> {
    // Try to get the authorship log from the old commit
    match get_reference_as_authorship_log_v3(repo, from_sha) {
        Ok(mut log) => {
            // Update the base_commit_sha to the new commit
            log.metadata.base_commit_sha = to_sha.to_string();

            // Save to the new commit
            let authorship_json = log.serialize_to_string().map_err(|_| {
                GitAiError::Generic("Failed to serialize authorship log".to_string())
            })?;

            crate::git::refs::notes_add(repo, to_sha, &authorship_json)?;
            Ok(())
        }
        Err(_) => {
            // No authorship log exists for the old commit, that's ok
            debug_log(&format!("No authorship log found for {}", from_sha));
            Ok(())
        }
    }
}

/// Get file contents from a commit tree for specified pathspecs
#[allow(dead_code)]
fn get_committed_files_content(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    use std::collections::HashMap;

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

pub fn rewrite_authorship_after_commit_amend(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    _human_author: String,
) -> Result<AuthorshipLog, GitAiError> {
    use crate::authorship::virtual_attribution::VirtualAttributions;

    // Get the files that changed between original and amended commit
    let changed_files = repo.list_commit_files(amended_commit, None)?;
    let pathspecs: Vec<String> = changed_files.into_iter().collect();

    if pathspecs.is_empty() {
        // No files changed, just update the base commit SHA
        let mut authorship_log = match get_reference_as_authorship_log_v3(repo, original_commit) {
            Ok(log) => log,
            Err(_) => {
                let mut log = AuthorshipLog::new();
                log.metadata.base_commit_sha = amended_commit.to_string();
                log
            }
        };
        authorship_log.metadata.base_commit_sha = amended_commit.to_string();

        // Save the updated log
        let authorship_json = authorship_log
            .serialize_to_string()
            .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
        crate::git::refs::notes_add(repo, amended_commit, &authorship_json)?;

        // Clean up working log
        repo.storage
            .delete_working_log_for_base_commit(original_commit)?;

        return Ok(authorship_log);
    }

    // Check if original commit has an authorship log with prompts
    let has_existing_log = get_reference_as_authorship_log_v3(repo, original_commit).is_ok();
    let has_existing_prompts = if has_existing_log {
        let original_log = get_reference_as_authorship_log_v3(repo, original_commit).unwrap();
        !original_log.metadata.prompts.is_empty()
    } else {
        false
    };

    // Phase 1: Load all attributions (committed + uncommitted)
    let repo_clone = repo.clone();
    let working_va = smol::block_on(async {
        VirtualAttributions::from_working_log_for_commit(
            repo_clone,
            original_commit.to_string(),
            &pathspecs,
            if has_existing_prompts {
                None
            } else {
                Some(_human_author.clone())
            },
        )
        .await
    })?;

    // Phase 2: Read committed content from amended commit
    let committed_files = get_committed_files_content(repo, amended_commit, &pathspecs)?;

    // Phase 3: Split into committed (authorship log) vs uncommitted (INITIAL)
    let (mut authorship_log, initial_attributions) =
        working_va.to_authorship_log_and_initial_working_log(committed_files)?;

    // Update base commit SHA
    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Save authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    crate::git::refs::notes_add(repo, amended_commit, &authorship_json)?;

    // Save INITIAL file for uncommitted attributions
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo.storage.working_log_for_base_commit(amended_commit);
        new_working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)?;
    }

    // Clean up old working log
    repo.storage
        .delete_working_log_for_base_commit(original_commit)?;

    Ok(authorship_log)
}

// Build a path of commit SHAs from head_sha to the origin base
///
/// This function walks the commit history from head_sha backwards until it reaches
/// the origin_base, collecting all commit SHAs in the path. If no valid linear path
/// exists (incompatible lineage), it returns an error.
///
/// # Arguments
/// * `repo` - Git repository
/// * `head_sha` - SHA of the HEAD commit to start from
/// * `origin_base` - SHA of the origin base commit to walk to
///
/// # Returns
/// A vector of commit SHAs in chronological order (oldest first) representing
/// the path from just after origin_base to head_sha
#[allow(dead_code)]
pub fn build_commit_path_to_base(
    repo: &Repository,
    head_sha: &str,
    origin_base: &str,
) -> Result<Vec<String>, GitAiError> {
    let head_commit = repo.find_commit(head_sha.to_string())?;

    let mut commits = Vec::new();
    let mut current_commit = head_commit;

    // Walk backwards from head to origin_base
    loop {
        // If we've reached the origin base, we're done
        if current_commit.id() == origin_base.to_string() {
            break;
        }

        // Add current commit to our path
        commits.push(current_commit.id().to_string());

        // Move to parent commit
        match current_commit.parent(0) {
            Ok(parent) => current_commit = parent,
            Err(_) => {
                return Err(GitAiError::Generic(format!(
                    "Incompatible lineage: no path from {} to {}. Reached end of history without finding origin base.",
                    head_sha, origin_base
                )));
            }
        }

        // Safety check: avoid infinite loops in case of circular references
        if commits.len() > 10000 {
            return Err(GitAiError::Generic(
                "Incompatible lineage: path too long, possible circular reference".to_string(),
            ));
        }
    }

    // If we have no commits, head_sha and origin_base are the same
    if commits.is_empty() {
        return Err(GitAiError::Generic(format!(
            "Incompatible lineage: head_sha ({}) and origin_base ({}) are the same commit",
            head_sha, origin_base
        )));
    }

    // Reverse to get chronological order (oldest first)
    commits.reverse();

    Ok(commits)
}

pub fn walk_commits_to_base(
    repository: &Repository,
    head: &str,
    base: &str,
) -> Result<Vec<String>, crate::error::GitAiError> {
    let mut commits = Vec::new();
    let mut current = repository.find_commit(head.to_string())?;
    let base_str = base.to_string();

    while current.id().to_string() != base_str {
        commits.push(current.id().to_string());
        current = current.parent(0)?;
    }

    Ok(commits)
}

/// Get all file paths changed between two commits
#[allow(dead_code)]
fn get_files_changed_between_commits(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--name-only".to_string());
    args.push(from_commit.to_string());
    args.push(to_commit.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    let files: Vec<String> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect();

    Ok(files)
}

/// Reconstruct working log after a reset that preserves working directory
///
/// This handles --soft, --mixed, and --merge resets where we move HEAD backward
/// but keep the working directory state. We need to create a working log that
/// captures AI authorship from the "unwound" commits plus any existing uncommitted changes.
///
/// Uses VirtualAttributions to merge AI authorship from old_head (with working log) and
/// target_commit, generating INITIAL checkpoints that seed the AI state on target_commit.
#[allow(dead_code)]
pub fn reconstruct_working_log_after_reset(
    repo: &Repository,
    target_commit_sha: &str, // Where we reset TO
    old_head_sha: &str,      // Where HEAD was BEFORE reset
    _human_author: &str,
    user_pathspecs: Option<&[String]>, // Optional user-specified pathspecs for partial reset
) -> Result<(), GitAiError> {
    debug_log(&format!(
        "Reconstructing working log after reset from {} to {}",
        old_head_sha, target_commit_sha
    ));

    // Step 1: Get all files changed between target and old_head
    let all_changed_files =
        get_files_changed_between_commits(repo, target_commit_sha, old_head_sha)?;

    // Filter to user pathspecs if provided
    let pathspecs: Vec<String> = if let Some(user_paths) = user_pathspecs {
        all_changed_files
            .into_iter()
            .filter(|f| user_paths.iter().any(|p| f == p || f.starts_with(p)))
            .collect()
    } else {
        all_changed_files
    };

    if pathspecs.is_empty() {
        debug_log("No files changed between commits, nothing to reconstruct");
        // Still delete old working log
        repo.storage
            .delete_working_log_for_base_commit(old_head_sha)?;
        return Ok(());
    }

    debug_log(&format!(
        "Processing {} files for reset authorship reconstruction",
        pathspecs.len()
    ));

    // Step 2: Build VirtualAttributions from old_head with working log applied
    // from_working_log_for_commit now runs blame (gets ALL prompts) AND applies working log
    let repo_clone = repo.clone();
    let old_head_clone = old_head_sha.to_string();
    let pathspecs_clone = pathspecs.clone();

    let old_head_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::from_working_log_for_commit(
            repo_clone,
            old_head_clone,
            &pathspecs_clone,
            None, // Don't need human_author for this step
        )
        .await
    })?;

    debug_log(&format!(
        "Built old_head VA with {} files, {} prompts",
        old_head_va.files().len(),
        old_head_va.prompts().len()
    ));

    // Step 3: Build VirtualAttributions from target_commit
    let repo_clone = repo.clone();
    let target_clone = target_commit_sha.to_string();
    let pathspecs_clone = pathspecs.clone();

    let target_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_clone,
            &pathspecs_clone,
        )
        .await
    })?;

    debug_log(&format!(
        "Built target VA with {} files, {} prompts",
        target_va.files().len(),
        target_va.prompts().len()
    ));

    // Step 4: Build final state from working directory
    use std::collections::HashMap;
    let mut final_state: HashMap<String, String> = HashMap::new();

    let workdir = repo.workdir()?;
    for file_path in &pathspecs {
        let abs_path = workdir.join(file_path);
        let content = if abs_path.exists() {
            std::fs::read_to_string(&abs_path).unwrap_or_default()
        } else {
            String::new()
        };
        final_state.insert(file_path.clone(), content);
    }

    debug_log(&format!(
        "Read {} files from working directory",
        final_state.len()
    ));

    // Step 5: Merge VAs favoring old_head to preserve uncommitted AI changes
    // old_head (with working log) wins overlaps, target fills gaps
    let merged_va = crate::authorship::virtual_attribution::merge_attributions_favoring_first(
        old_head_va,
        target_va,
        final_state.clone(),
    )?;

    debug_log(&format!(
        "Merged VAs, result has {} files",
        merged_va.files().len()
    ));

    // Step 6: Convert merged VA to AuthorshipLog
    let mut authorship_log = merged_va.to_authorship_log()?;
    authorship_log.metadata.base_commit_sha = target_commit_sha.to_string();

    debug_log(&format!(
        "Converted to authorship log with {} attestations, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    ));

    // Step 7: Convert to INITIAL (everything is uncommitted after reset)
    // Pass empty committed_files since nothing has been committed yet
    let empty_committed_files: HashMap<String, String> = HashMap::new();
    let (_authorship_log, initial_attributions) =
        merged_va.to_authorship_log_and_initial_working_log(empty_committed_files)?;

    debug_log(&format!(
        "Generated INITIAL attributions for {} files",
        initial_attributions.files.len()
    ));

    // Step 8: Write INITIAL file
    let new_working_log = repo.storage.working_log_for_base_commit(target_commit_sha);
    new_working_log.reset_working_log()?;

    if !initial_attributions.files.is_empty() {
        new_working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)?;
    }

    // Delete old working log
    repo.storage
        .delete_working_log_for_base_commit(old_head_sha)?;

    debug_log(&format!(
        "✓ Wrote INITIAL attributions to working log for {}",
        target_commit_sha
    ));

    Ok(())
}

/// Get all file paths modified across a list of commits
#[allow(dead_code)]
fn get_pathspecs_from_commits(
    repo: &Repository,
    commits: &[String],
) -> Result<Vec<String>, GitAiError> {
    let mut pathspecs = std::collections::HashSet::new();

    for commit_sha in commits {
        let files = repo.list_commit_files(commit_sha, None)?;
        pathspecs.extend(files);
    }

    Ok(pathspecs.into_iter().collect())
}

/// Transform VirtualAttributions to match a new final state (single-source variant)
#[allow(dead_code)]
fn transform_attributions_to_final_state(
    source_va: &crate::authorship::virtual_attribution::VirtualAttributions,
    final_state: HashMap<String, String>,
    fallback_va: Option<&crate::authorship::virtual_attribution::VirtualAttributions>,
) -> Result<crate::authorship::virtual_attribution::VirtualAttributions, GitAiError> {
    use crate::authorship::attribution_tracker::AttributionTracker;
    use crate::authorship::virtual_attribution::VirtualAttributions;

    let tracker = AttributionTracker::new();
    let ts = source_va.timestamp();
    let repo = source_va.repo().clone();
    let base_commit = source_va.base_commit().to_string();

    let mut attributions = HashMap::new();
    let mut file_contents = HashMap::new();

    // Process each file in the final state
    for (file_path, final_content) in final_state {
        // Skip empty files (they don't exist in this commit yet)
        // Keep the source attributions for when the file appears later
        if final_content.is_empty() {
            // Preserve original attributions and content for this file
            if let (Some(src_attrs), Some(src_content)) = (
                source_va.get_char_attributions(&file_path),
                source_va.get_file_content(&file_path),
            ) {
                if let Some(src_line_attrs) = source_va.get_line_attributions(&file_path) {
                    attributions.insert(
                        file_path.clone(),
                        (src_attrs.clone(), src_line_attrs.clone()),
                    );
                    file_contents.insert(file_path, src_content.clone());
                }
            }
            continue;
        }

        // Get source attributions and content
        let source_attrs = source_va.get_char_attributions(&file_path);
        let source_content = source_va.get_file_content(&file_path);

        // Transform to final state
        let mut transformed_attrs = if let (Some(attrs), Some(content)) =
            (source_attrs, source_content)
        {
            // Use a dummy author for new insertions
            let dummy_author = "__DUMMY__";

            let transformed =
                tracker.update_attributions(content, &final_content, attrs, dummy_author, ts)?;

            // Keep all attributions initially (including dummy ones)
            transformed
        } else {
            Vec::new()
        };

        // Try to restore attributions from fallback VA for "new" content that existed originally
        if let Some(fallback) = fallback_va {
            if let Some(fallback_content) = fallback.get_file_content(&file_path) {
                if fallback_content == &final_content {
                    // The final content matches the original content exactly!
                    // Use the original attributions
                    if let Some(fallback_attrs) = fallback.get_char_attributions(&file_path) {
                        transformed_attrs = fallback_attrs.clone();
                    }
                } else {
                    // Content doesn't match exactly, but we can still try to restore attributions
                    // for matching substrings (handles commit splitting)
                    let dummy_author = "__DUMMY__";
                    for attr in &mut transformed_attrs {
                        if attr.author_id == dummy_author {
                            // This is new content - check if it exists in fallback
                            let new_text =
                                &final_content[attr.start..attr.end.min(final_content.len())];

                            // Search for this text in the fallback content
                            if let Some(pos) = fallback_content.find(new_text) {
                                // Found matching text in original - check if we have attribution for it
                                if let Some(fallback_attrs) =
                                    fallback.get_char_attributions(&file_path)
                                {
                                    for fallback_attr in fallback_attrs {
                                        // Check if this fallback attribution covers the matched position
                                        if fallback_attr.start <= pos && pos < fallback_attr.end {
                                            // Restore the original author
                                            attr.author_id = fallback_attr.author_id.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Now filter out any remaining dummy attributions
        let dummy_author = "__DUMMY__";
        transformed_attrs = transformed_attrs
            .into_iter()
            .filter(|attr| attr.author_id != dummy_author)
            .collect();

        // Convert to line attributions
        let line_attrs = crate::authorship::attribution_tracker::attributions_to_line_attributions(
            &transformed_attrs,
            &final_content,
        );

        attributions.insert(file_path.clone(), (transformed_attrs, line_attrs));
        file_contents.insert(file_path, final_content);
    }

    // Preserve prompts from source VA
    let prompts = source_va.prompts().clone();

    Ok(VirtualAttributions::from_raw_data_with_prompts(
        repo,
        base_commit,
        attributions,
        file_contents,
        prompts,
        ts,
    ))
}
