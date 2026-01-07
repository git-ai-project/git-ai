use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::error::GitAiError;
use crate::git::authorship_traversal::load_ai_touched_files_for_commits;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{CommitRange, Repository};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;
use std::collections::{HashMap, HashSet};

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
pub fn prepare_working_log_after_squash(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    _human_author: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    // Step 1: Find merge base between source and target to optimize blame
    // We only need to look at commits after the merge base, not entire history
    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    // Step 2: Get list of changed files between the two branches
    let changed_files = repo.diff_changed_files(source_head_sha, target_branch_head_sha)?;

    if changed_files.is_empty() {
        // No files changed, nothing to do
        return Ok(());
    }

    // Step 3: Create VirtualAttributions for both branches
    // Use merge_base to limit blame range for performance
    let repo_clone = repo.clone();
    let merge_base_clone = merge_base.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
            merge_base_clone,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.to_string(),
            &changed_files,
            merge_base,
        )
        .await
    })?;

    // Step 3: Read staged files content (final state after squash)
    let staged_files = repo.get_all_staged_files_content(&changed_files)?;

    // Step 4: Merge VirtualAttributions, favoring target branch (HEAD)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, staged_files)?;

    // Step 5: Convert to INITIAL (everything is uncommitted in a squash)
    // Pass same SHA for parent and commit to get empty diff (no committed hunks)
    let (_authorship_log, initial_attributions) = merged_va
        .to_authorship_log_and_initial_working_log(
            repo,
            target_branch_head_sha,
            target_branch_head_sha,
            None,
        )?;

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

/// Rewrite authorship after a squash or rebase merge performed in CI/GUI
///
/// This handles the case where a squash merge or rebase merge was performed via SCM GUI,
/// and we need to reconstruct authorship after the fact. Unlike `prepare_working_log_after_squash`,
/// this writes directly to the authorship log (git notes) since the merge is already committed.
///
/// # Arguments
/// * `repo` - Git repository
/// * `_head_ref` - Reference name of the source branch (e.g., "feature/123")
/// * `merge_ref` - Reference name of the target/base branch (e.g., "main")
/// * `source_head_sha` - SHA of the source branch head that was merged
/// * `merge_commit_sha` - SHA of the final merge commit
/// * `_suppress_output` - Whether to suppress output (unused, kept for API compatibility)
pub fn rewrite_authorship_after_squash_or_rebase(
    repo: &Repository,
    _head_ref: &str,
    merge_ref: &str,
    source_head_sha: &str,
    merge_commit_sha: &str,
    _suppress_output: bool,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    // Step 1: Get target branch head (first parent on merge_ref)
    // This is more correct than just parent(0) in cases with complex back-and-forth merge history
    let merge_commit = repo.find_commit(merge_commit_sha.to_string())?;
    let target_branch_head = merge_commit.parent_on_refname(merge_ref)?;
    let target_branch_head_sha = target_branch_head.id().to_string();

    debug_log(&format!(
        "Rewriting authorship for squash/rebase merge: {} -> {}",
        source_head_sha, merge_commit_sha
    ));

    // Step 2: Find merge base between source and target to optimize blame
    // We only need to look at commits after the merge base, not entire history
    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    // Step 3: Get list of changed files between the two branches
    let changed_files = repo.diff_changed_files(source_head_sha, &target_branch_head_sha)?;

    // Get commits from source branch (from source_head back to merge_base)
    // Uses git rev-list which safely handles the range without infinite walking
    let source_commits = if let Some(ref base) = merge_base {
        let range =
            CommitRange::new_infer_refname(repo, base.clone(), source_head_sha.to_string(), None)?;
        range.all_commits()
    } else {
        vec![source_head_sha.to_string()]
    };
    let changed_files =
        filter_pathspecs_to_ai_touched_files(repo, &source_commits, &changed_files)?;

    if changed_files.is_empty() {
        // No files changed, nothing to do
        debug_log("No files changed in merge, skipping authorship rewrite");
        return Ok(());
    }

    debug_log(&format!(
        "Processing {} changed files for merge authorship",
        changed_files.len()
    ));

    // Step 4: Create VirtualAttributions for both branches
    // Use merge_base to limit blame range for performance
    let repo_clone = repo.clone();
    let merge_base_clone = merge_base.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
            merge_base_clone,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.clone(),
            &changed_files,
            merge_base,
        )
        .await
    })?;

    // Step 4: Read committed files from merge commit (captures final state with conflict resolutions)
    let committed_files = get_committed_files_content(repo, merge_commit_sha, &changed_files)?;

    debug_log(&format!(
        "Read {} committed files from merge commit",
        committed_files.len()
    ));

    // Step 5: Merge VirtualAttributions, favoring target branch (base)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, committed_files)?;

    // Step 6: Convert to AuthorshipLog (everything is committed in CI merge)
    let mut authorship_log = merged_va.to_authorship_log()?;
    authorship_log.metadata.base_commit_sha = merge_commit_sha.to_string();

    debug_log(&format!(
        "Created authorship log with {} attestations, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    ));

    // Step 7: Save authorship log to git notes
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::git::refs::notes_add(repo, merge_commit_sha, &authorship_json)?;

    debug_log(&format!(
        "✓ Saved authorship log for merge commit {}",
        merge_commit_sha
    ));

    Ok(())
}

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
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, original_commits, &pathspecs)?;

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

    // Filter out commits that already have authorship logs (these are commits from the target branch)
    // Only process newly created rebased commits
    let commits_to_process: Vec<String> = new_commits
        .iter()
        .filter(|commit| {
            let has_log = get_reference_as_authorship_log_v3(repo, commit).is_ok();
            if has_log {
                debug_log(&format!(
                    "Skipping commit {} (already has authorship log)",
                    commit
                ));
            }
            !has_log
        })
        .cloned()
        .collect();

    if commits_to_process.is_empty() {
        debug_log("No new commits to process (all commits already have authorship logs)");
        return Ok(());
    }

    debug_log(&format!(
        "Processing {} newly created commits (skipped {} existing commits)",
        commits_to_process.len(),
        new_commits.len() - commits_to_process.len()
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
            None,
        )
        .await
    })?;

    // Clone the original VA to use for restoring attributions when content reappears
    // This handles commit splitting where content from original_head gets re-applied
    let original_head_state_va = {
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
        crate::authorship::virtual_attribution::VirtualAttributions::new(
            current_va.repo().clone(),
            current_va.base_commit().to_string(),
            attrs,
            contents,
            current_va.timestamp(),
        )
    };

    // Step 3: Process each new commit in order (oldest to newest)
    for (idx, new_commit) in commits_to_process.iter().enumerate() {
        debug_log(&format!(
            "Processing commit {}/{}: {}",
            idx + 1,
            commits_to_process.len(),
            new_commit
        ));

        // Get the DIFF for this commit (what actually changed)
        let commit_obj = repo.find_commit(new_commit.clone())?;
        let parent_obj = commit_obj.parent(0)?;

        let commit_tree = commit_obj.tree()?;
        let parent_tree = parent_obj.tree()?;

        let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), None, None)?;

        // Identify which tracked files actually changed in this commit
        let mut changed_files_in_commit = std::collections::HashSet::new();
        let mut new_content_for_changed_files = HashMap::new();

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

            changed_files_in_commit.insert(file_path_str.clone());

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

            new_content_for_changed_files.insert(file_path_str, new_content);
        }

        // Only transform attributions for files that actually changed
        // For unchanged files, we'll preserve them as-is
        if !changed_files_in_commit.is_empty() {
            current_va = transform_attributions_to_final_state(
                &current_va,
                new_content_for_changed_files.clone(),
                Some(&original_head_state_va),
            )?;
        }

        // Build complete content state for authorship log (all tracked files)
        let mut new_content_state = HashMap::new();
        for file in current_va.files() {
            if let Some(content) = current_va.get_file_content(&file) {
                new_content_state.insert(file, content.clone());
            }
        }
        // Update with any changed files
        new_content_state.extend(new_content_for_changed_files);

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
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, source_commits, &pathspecs)?;

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
            None,
        )
        .await
    })?;

    // Clone the source VA to use for restoring attributions when content reappears
    // This handles commit splitting where content from source gets re-applied
    let source_head_state_va = {
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
        crate::authorship::virtual_attribution::VirtualAttributions::new(
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
        // Pass source_head state to restore attributions for content that existed before cherry-pick
        current_va = transform_attributions_to_final_state(
            &current_va,
            new_content_state.clone(),
            Some(&source_head_state_va),
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

/// Get file contents from a commit tree for specified pathspecs
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
    let mut pathspecs: HashSet<String> = changed_files.into_iter().collect();

    let working_log = repo.storage.working_log_for_base_commit(original_commit);
    let touched_files = working_log.all_touched_files()?;
    pathspecs.extend(touched_files);

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
    let pathspecs_vec: Vec<String> = pathspecs.iter().cloned().collect();
    let working_va = smol::block_on(async {
        VirtualAttributions::from_working_log_for_commit(
            repo_clone,
            original_commit.to_string(),
            &pathspecs_vec,
            if has_existing_prompts {
                None
            } else {
                Some(_human_author.clone())
            },
            None,
        )
        .await
    })?;

    // Phase 2: Get parent of amended commit for diff calculation
    let amended_commit_obj = repo.find_commit(amended_commit.to_string())?;
    let parent_sha = if amended_commit_obj.parent_count()? > 0 {
        amended_commit_obj.parent(0)?.id().to_string()
    } else {
        "initial".to_string()
    };

    // pathspecs is already a HashSet
    let pathspecs_set = pathspecs;

    // Phase 3: Split into committed (authorship log) vs uncommitted (INITIAL)
    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            amended_commit,
            Some(&pathspecs_set),
        )?;

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

pub fn walk_commits_to_base(
    repository: &Repository,
    head: &str,
    base: &str,
) -> Result<Vec<String>, crate::error::GitAiError> {
    use std::collections::{HashSet, VecDeque};

    let mut commits = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    let base_str = base.to_string();
    let head_commit = repository.find_commit(head.to_string())?;

    queue.push_back(head_commit);

    while let Some(current) = queue.pop_front() {
        let commit_id = current.id().to_string();

        // Skip if we've reached the base
        if commit_id == base_str {
            continue;
        }

        // Skip if already visited
        if visited.contains(&commit_id) {
            continue;
        }

        visited.insert(commit_id.clone());
        commits.push(commit_id);

        // Add all parents to the queue (handles merge commits)
        let parent_count = current.parent_count().unwrap_or(0);
        for i in 0..parent_count {
            if let Ok(parent) = current.parent(i) {
                queue.push_back(parent);
            }
        }
    }

    Ok(commits)
}

/// Get all file paths changed between two commits
fn get_files_changed_between_commits(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
) -> Result<Vec<String>, GitAiError> {
    repo.diff_changed_files(from_commit, to_commit)
}

/// Reconstruct working log after a reset that preserves working directory
///
/// This handles --soft, --mixed, and --merge resets where we move HEAD backward
/// but keep the working directory state. We need to create a working log that
/// captures AI authorship from the "unwound" commits plus any existing uncommitted changes.
///
/// Uses VirtualAttributions to merge AI authorship from old_head (with working log) and
/// target_commit, generating INITIAL checkpoints that seed the AI state on target_commit.
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

    // Get all commits in the range from old_head back to target (exclusive of target)
    // Uses git rev-list which safely handles the range without infinite walking
    let range = CommitRange::new_infer_refname(
        repo,
        target_commit_sha.to_string(),
        old_head_sha.to_string(),
        None,
    )?;
    let commits_in_range = range.all_commits();
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, &commits_in_range, &pathspecs)?;

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
            Some(target_commit_sha.to_string()),
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
            Some(target_commit_sha.to_string()),
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

    // Step 6: Convert to INITIAL (everything is uncommitted after reset)
    // Pass same SHA for parent and commit to get empty diff (no committed hunks)
    // IMPORTANT: Pass pathspecs to limit diff to only changed files (major performance optimization)
    let pathspecs_set: std::collections::HashSet<String> = pathspecs.iter().cloned().collect();
    let (authorship_log, initial_attributions) = merged_va
        .to_authorship_log_and_initial_working_log(
            repo,
            target_commit_sha,
            target_commit_sha,
            Some(&pathspecs_set),
        )?;

    debug_log(&format!(
        "Generated INITIAL attributions for {} files, {} attestations, {} prompts",
        initial_attributions.files.len(),
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    ));

    // Step 7: Write INITIAL file
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

pub fn filter_pathspecs_to_ai_touched_files(
    repo: &Repository,
    commit_shas: &[String],
    pathspecs: &[String],
) -> Result<Vec<String>, GitAiError> {
    let touched_files = smol::block_on(load_ai_touched_files_for_commits(
        repo,
        commit_shas.to_vec(),
    ))?;
    Ok(pathspecs
        .iter()
        .filter(|p| touched_files.contains(p.as_str()))
        .cloned()
        .collect())
}

/// Transform VirtualAttributions to match a new final state (single-source variant)
fn transform_attributions_to_final_state(
    source_va: &crate::authorship::virtual_attribution::VirtualAttributions,
    final_state: HashMap<String, String>,
    original_head_state: Option<&crate::authorship::virtual_attribution::VirtualAttributions>,
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

        // Try to restore attributions from original_head_state using line-content matching
        // This handles commit splitting where content from original_head gets re-applied
        if let Some(original_state) = original_head_state {
            if let Some(original_content) = original_state.get_file_content(&file_path) {
                if original_content == &final_content {
                    // The final content matches the original content exactly!
                    // Use the original attributions
                    if let Some(original_attrs) = original_state.get_char_attributions(&file_path) {
                        transformed_attrs = original_attrs.clone();
                    }
                } else {
                    // Use line-content matching to restore attributions for lines that existed before
                    // Build a map of line content -> author from original state
                    let mut original_line_to_author: HashMap<String, String> = HashMap::new();

                    if let Some(original_line_attrs) =
                        original_state.get_line_attributions(&file_path)
                    {
                        let original_lines: Vec<&str> = original_content.lines().collect();

                        for line_attr in original_line_attrs {
                            // LineAttribution is 1-indexed
                            for line_num in line_attr.start_line..=line_attr.end_line {
                                let line_idx = (line_num as usize).saturating_sub(1);
                                if line_idx < original_lines.len() {
                                    let line_content = original_lines[line_idx].to_string();
                                    // Store all non-human attributions (AI attributions)
                                    // VirtualAttributions normalizes humans to "human" via return_human_authors_as_human flag
                                    // AI authors keep their tool names (mock_ai, Claude, GPT, etc.) or prompt hashes
                                    if line_attr.author_id != "human" {
                                        original_line_to_author
                                            .insert(line_content, line_attr.author_id.clone());
                                    }
                                }
                            }
                        }
                    }

                    // Now update char attributions based on line content matching
                    let dummy_author = "__DUMMY__";
                    let final_lines: Vec<&str> = final_content.lines().collect();

                    // Convert char attributions to line attributions to process line by line
                    let temp_line_attrs =
                        crate::authorship::attribution_tracker::attributions_to_line_attributions(
                            &transformed_attrs,
                            &final_content,
                        );

                    // For each line with dummy attribution, try to restore from original
                    for (line_idx, line_content) in final_lines.iter().enumerate() {
                        // Check if this line has a dummy attribution
                        let line_num = (line_idx + 1) as u32; // LineAttribution is 1-indexed
                        let has_dummy = temp_line_attrs.iter().any(|la| {
                            la.start_line <= line_num
                                && la.end_line >= line_num
                                && la.author_id == dummy_author
                        });

                        if has_dummy {
                            // Try to find this line content in original state
                            if let Some(original_author) =
                                original_line_to_author.get(*line_content)
                            {
                                // Update all char attributions on this line
                                // Find the char range for this line
                                let line_start_char: usize = final_lines[..line_idx]
                                    .iter()
                                    .map(|l| l.len() + 1) // +1 for newline
                                    .sum();
                                let line_end_char = line_start_char + line_content.len();

                                // Update attributions that overlap with this line
                                for attr in &mut transformed_attrs {
                                    if attr.author_id == dummy_author
                                        && attr.start < line_end_char
                                        && attr.end > line_start_char
                                    {
                                        attr.author_id = original_author.clone();
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

    // Merge prompts from source VA and original_head_state, picking the newest version of each
    let mut prompts = if let Some(original_state) = original_head_state {
        crate::authorship::virtual_attribution::VirtualAttributions::merge_prompts_picking_newest(
            &[source_va.prompts(), original_state.prompts()],
        )
    } else {
        source_va.prompts().clone()
    };

    // Save total_additions and total_deletions from the merged prompts
    let mut saved_totals: HashMap<String, (u32, u32)> = HashMap::new();
    for (prompt_id, commits) in &prompts {
        for prompt_record in commits.values() {
            saved_totals.insert(
                prompt_id.clone(),
                (prompt_record.total_additions, prompt_record.total_deletions),
            );
        }
    }

    // Calculate and update prompt metrics based on transformed attributions
    crate::authorship::virtual_attribution::VirtualAttributions::calculate_and_update_prompt_metrics(
        &mut prompts,
        &attributions,
        &HashMap::new(), // Empty - will result in total_additions = 0
        &HashMap::new(), // Empty - will result in total_deletions = 0
    );

    // Restore the saved total_additions and total_deletions
    for (prompt_id, commits) in prompts.iter_mut() {
        if let Some(&(additions, deletions)) = saved_totals.get(prompt_id) {
            for prompt_record in commits.values_mut() {
                prompt_record.total_additions = additions;
                prompt_record.total_deletions = deletions;
            }
        }
    }

    Ok(VirtualAttributions::new_with_prompts(
        repo,
        base_commit,
        attributions,
        file_contents,
        prompts,
        ts,
    ))
}
