use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::authorship_log_cache::AuthorshipLogCache;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{Commit, Repository};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;
use similar::{ChangeTag, TextDiff};
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

            // Prepare checkpoints from the squashed changes
            let checkpoints = prepare_working_log_after_squash(
                repo,
                &merge_squash.source_head,
                &merge_squash.base_head,
                &commit_author,
            )?;

            // Append checkpoints to the working log for the base commit
            let working_log = repo
                .storage
                .working_log_for_base_commit(&merge_squash.base_head);
            for checkpoint in checkpoints {
                working_log.append_checkpoint(&checkpoint)?;
            }

            debug_log(&format!(
                "✓ Prepared authorship checkpoints for merge --squash of {} into {}",
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

/// Rewrite authorship log after a squash merge or rebase
///
/// This function handles the complex case where multiple commits from a linear history
/// have been squashed into a single new commit (new_sha). It preserves AI authorship attribution
/// by analyzing the diff and applying blame logic to identify which lines were originally
/// authored by AI.
///
/// # Arguments
/// * `repo` - Git repository
/// * `head_sha` - SHA of the HEAD commit of the original history that was squashed
/// * `new_sha` - SHA of the new squash commit
///
/// # Returns
/// The authorship log for the new commit
pub fn rewrite_authorship_after_squash_or_rebase(
    repo: &Repository,
    _destination_branch: &str,
    head_sha: &str,
    new_sha: &str,
    dry_run: bool,
) -> Result<AuthorshipLog, GitAiError> {
    // Cache for foreign prompts to avoid repeated grepping
    let mut foreign_prompts_cache: HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    > = HashMap::new();

    // Step 1: Find the common origin base
    let origin_base = find_common_origin_base_from_head(repo, head_sha, new_sha)?;

    // Step 2: Build the old_shas path from head_sha to origin_base
    let _old_shas = build_commit_path_to_base(repo, head_sha, &origin_base)?;

    // Step 3: Get the parent of the new commit
    let new_commit = repo.find_commit(new_sha.to_string())?;
    let new_commit_parent = new_commit.parent(0)?;

    // Step 4: Compute a diff between origin_base and new_commit_parent. Sometimes it's the same
    // sha. that's ok
    let origin_base_commit = repo.find_commit(origin_base.to_string())?;
    let origin_base_tree = origin_base_commit.tree()?;
    let new_commit_parent_tree = new_commit_parent.tree()?;

    // TODO Is this diff necessary? The result is unused
    // Create diff between the two trees
    let _diff = repo.diff_tree_to_tree(
        Some(&origin_base_tree),
        Some(&new_commit_parent_tree),
        None,
        None,
    )?;

    // Step 5: Take this diff and apply it to the HEAD of the old shas history.
    // We want it to be a merge essentially, and Accept Theirs (OLD Head wins when there's conflicts)
    let hanging_commit_sha = apply_diff_as_merge_commit(
        repo,
        &origin_base,
        &new_commit_parent.id().to_string(),
        head_sha, // HEAD of old shas history
    )?;

    // Create a cache for authorship logs to avoid repeated lookups in the reconstruction process
    let mut authorship_log_cache = AuthorshipLogCache::new();

    // Step 5: Now get the diff between between new_commit and new_commit_parent.
    // We want just the changes between the two commits.
    // We will iterate each file / hunk and then, we will run @blame logic in the context of
    // hanging_commit_sha
    // That way we can get the authorship log pre-squash.
    // Aggregate the results in a variable, then we'll dump a new authorship log.
    let mut new_authorship_log = reconstruct_authorship_from_diff(
        repo,
        &new_commit,
        &new_commit_parent,
        &hanging_commit_sha,
        &mut authorship_log_cache,
        &mut foreign_prompts_cache,
    )?;

    // Set the base_commit_sha to the new commit
    new_authorship_log.metadata.base_commit_sha = new_sha.to_string();

    // Step 6: Delete the hanging commit

    delete_hanging_commit(repo, &hanging_commit_sha)?;
    // println!("Deleted hanging commit: {}", hanging_commit_sha);

    if !dry_run {
        // Step (Save): Save the authorship log with the new sha as its id
        let authorship_json = new_authorship_log
            .serialize_to_string()
            .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

        crate::git::refs::notes_add(repo, &new_sha, &authorship_json)?;

        println!("Authorship log saved to notes/ai/{}", new_sha);
    }

    Ok(new_authorship_log)
}

/// Prepare working log checkpoints after a merge --squash (before commit)
///
/// This handles the case where `git merge --squash` has staged changes but hasn't committed yet.
/// It works similarly to `rewrite_authorship_after_squash_or_rebase`, but:
/// 1. Compares against the working directory instead of a new commit
/// 2. Returns checkpoints that can be appended to the current working log
/// 3. Doesn't save anything - just prepares the checkpoints
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_head_sha` - SHA of the HEAD commit of the branch that was squashed
/// * `target_branch_head_sha` - SHA of the current HEAD (target branch)
/// * `human_author` - The human author identifier to use for human-authored lines
///
/// # Returns
/// Vector of checkpoints ready to be appended to the working log
pub fn prepare_working_log_after_squash(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    _human_author: &str,
) -> Result<Vec<crate::authorship::working_log::Checkpoint>, GitAiError> {
    // Step 1: Find the common origin base between source and target
    let origin_base =
        find_common_origin_base_from_head(repo, source_head_sha, target_branch_head_sha)?;

    // Step 2: Build the old_shas path from source_head_sha to origin_base
    let _old_shas = build_commit_path_to_base(repo, source_head_sha, &origin_base)?;

    // Step 3: Get the target branch head commit (this is where the squash is being merged into)
    let target_commit = repo.find_commit(target_branch_head_sha.to_string())?;

    // Step 4: Apply the diff from origin_base to target_commit onto source_head
    // This creates a hanging commit that represents "what would the source branch look like
    // if we applied the changes from origin_base to target on top of it"

    // Create hanging commit: merge origin_base -> target changes onto source_head
    let hanging_commit_sha = apply_diff_as_merge_commit(
        repo,
        &origin_base,
        &target_commit.id().to_string(),
        source_head_sha, // HEAD of old shas history
    )?;

    // Step 5: Get the working directory tree (staged changes from squash)
    // Use `git write-tree` to write the current index to a tree
    let mut args = repo.global_args_for_exec();
    args.push("write-tree".to_string());
    let output = crate::git::repository::exec_git(&args)?;
    let working_tree_oid = String::from_utf8(output.stdout)?.trim().to_string();
    let working_tree = repo.find_tree(working_tree_oid.clone())?;

    // Step 6: Create a temporary commit for the working directory state
    // Use origin_base as parent so the diff shows ALL changes from the feature branch
    let origin_base_commit = repo.find_commit(origin_base.clone())?;
    let temp_commit = repo.commit(
        None, // Don't update any refs
        &target_commit.author()?,
        &target_commit.committer()?,
        "Temporary commit for squash authorship reconstruction",
        &working_tree,
        &[&origin_base_commit], // Parent is the common base, not target!
    )?;

    // Step 7: Reconstruct authorship from the diff between temp_commit and origin_base
    // This shows ALL changes that came from the feature branch
    let temp_commit_obj = repo.find_commit(temp_commit.to_string())?;
    let mut foreign_prompts_cache: HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    > = HashMap::new();
    let new_authorship_log = reconstruct_authorship_from_diff(
        repo,
        &temp_commit_obj,
        &origin_base_commit,
        &hanging_commit_sha,
        &mut AuthorshipLogCache::new(),
        &mut foreign_prompts_cache,
    )?;

    // Step 8: Clean up temporary commits
    delete_hanging_commit(repo, &hanging_commit_sha)?;
    delete_hanging_commit(repo, &temp_commit.to_string())?;

    // Step 9: Build file contents map from staged files
    use std::collections::HashMap;
    let mut file_contents: HashMap<String, String> = HashMap::new();
    for file_attestation in &new_authorship_log.attestations {
        let file_path = &file_attestation.file_path;
        // Read the staged version of the file using git show :path
        let mut args = repo.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!(":{}", file_path));

        let output = crate::git::repository::exec_git(&args)?;
        let file_content = String::from_utf8(output.stdout).map_err(|_| {
            GitAiError::Generic(format!("Failed to read staged file: {}", file_path))
        })?;
        file_contents.insert(file_path.clone(), file_content);
    }

    // Step 10: Convert authorship log to checkpoints
    let mut checkpoints = new_authorship_log
        .convert_to_checkpoints_for_squash(&file_contents)
        .map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to convert authorship log to checkpoints: {}",
                e
            ))
        })?;

    // Filter to keep only AI checkpoints - we don't track human-only changes in working logs
    checkpoints.retain(|checkpoint| checkpoint.kind != CheckpointKind::Human);

    // Step 11: For each checkpoint, read the staged file content and save blobs
    let working_log = repo
        .storage
        .working_log_for_base_commit(target_branch_head_sha);

    for checkpoint in &mut checkpoints {
        use sha2::{Digest, Sha256};
        let mut file_hashes = Vec::new();

        for entry in &mut checkpoint.entries {
            // Read the staged version of the file using git show :path
            let mut args = repo.global_args_for_exec();
            args.push("show".to_string());
            args.push(format!(":{}", entry.file));

            let output = crate::git::repository::exec_git(&args)?;
            let file_content = String::from_utf8(output.stdout).map_err(|_| {
                GitAiError::Generic(format!("Failed to read staged file: {}", entry.file))
            })?;

            // Persist the blob and get its SHA
            let blob_sha = working_log.persist_file_version(&file_content)?;
            entry.blob_sha = blob_sha.clone();

            // Collect file path and hash for combined hash calculation
            file_hashes.push((entry.file.clone(), blob_sha));
        }

        // Compute combined hash for the checkpoint (same as normal checkpoint logic)
        file_hashes.sort_by(|a, b| a.0.cmp(&b.0));
        let mut combined_hasher = Sha256::new();
        for (file_path, hash) in &file_hashes {
            combined_hasher.update(file_path.as_bytes());
            combined_hasher.update(hash.as_bytes());
        }
        checkpoint.diff = format!("{:x}", combined_hasher.finalize());
    }

    Ok(checkpoints)
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

/// Get file contents at a specific commit for given file paths
fn get_file_contents_at_commit(
    repo: &Repository,
    commit_sha: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let mut file_contents = HashMap::new();

    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;

    for file_path in file_paths {
        let content = match tree.get_path(std::path::Path::new(file_path)) {
            Ok(entry) => {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let blob_content = blob.content().unwrap_or_default();
                    String::from_utf8_lossy(&blob_content).to_string()
                } else {
                    String::new()
                }
            }
            Err(_) => String::new(), // File doesn't exist in this commit
        };

        file_contents.insert(file_path.clone(), content);
    }

    Ok(file_contents)
}

/// Transform VirtualAttributions to match a new final state (single-source variant)
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
///
/// # Returns
/// Ok if all commits were processed successfully
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

/// Rewrite authorship logs after cherry-pick (OLD implementation, using hanging commit approach)
///
/// Cherry-pick is simpler than rebase: it creates new commits by applying patches from source commits.
/// The mapping is typically 1:1, or 1:0 if a commit becomes empty (already applied).
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_commits` - Vector of source commit SHAs (commits being cherry-picked), oldest first
/// * `new_commits` - Vector of new commit SHAs (after cherry-pick), oldest first
/// * `human_author` - The human author identifier
///
/// # Returns
/// Ok if all commits were processed successfully
#[allow(dead_code)]
pub fn rewrite_authorship_after_cherry_pick_old(
    repo: &Repository,
    source_commits: &[String],
    new_commits: &[String],
    human_author: &str,
) -> Result<(), GitAiError> {
    debug_log(&format!(
        "Rewriting authorship for cherry-pick: {} source -> {} new commits",
        source_commits.len(),
        new_commits.len()
    ));

    // Cherry-pick can result in fewer commits if some become empty
    // Match up commits by position (they're applied in order)
    let min_len = std::cmp::min(source_commits.len(), new_commits.len());

    for i in 0..min_len {
        let source_sha = &source_commits[i];
        let new_sha = &new_commits[i];

        debug_log(&format!(
            "Processing cherry-picked commit {} -> {}",
            source_sha, new_sha
        ));

        // Use the same logic as rebase for single commit rewriting
        if let Err(e) = rewrite_single_commit_authorship(repo, source_sha, new_sha, human_author) {
            debug_log(&format!(
                "Failed to rewrite authorship for {} -> {}: {}",
                source_sha, new_sha, e
            ));
            // Continue with other commits even if one fails
        }
    }

    // If there are fewer new commits than source commits, some were dropped (empty)
    if new_commits.len() < source_commits.len() {
        debug_log(&format!(
            "Note: {} source commits resulted in {} new commits (some became empty)",
            source_commits.len(),
            new_commits.len()
        ));
    }

    // If there are more new commits, this shouldn't normally happen with cherry-pick
    // but handle it gracefully
    if new_commits.len() > source_commits.len() {
        debug_log(&format!(
            "Warning: More new commits ({}) than source commits ({})",
            new_commits.len(),
            source_commits.len()
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
///
/// # Returns
/// Ok if all commits were processed successfully
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

/// Rewrite authorship for a single commit after rebase
///
/// Fast path: If trees are identical, just copy the authorship log
/// Slow path: If trees differ, reconstruct via blame in hanging commit context
fn rewrite_single_commit_authorship(
    repo: &Repository,
    old_sha: &str,
    new_sha: &str,
    _human_author: &str,
) -> Result<(), GitAiError> {
    let old_commit = repo.find_commit(old_sha.to_string())?;
    let new_commit = repo.find_commit(new_sha.to_string())?;

    // Fast path: Check if trees are identical
    if trees_identical(&old_commit, &new_commit)? {
        // Trees are the same, just copy the authorship log with new SHA
        copy_authorship_log(repo, old_sha, new_sha)?;
        debug_log(&format!(
            "Copied authorship log from {} to {} (trees identical)",
            old_sha, new_sha
        ));
        return Ok(());
    }

    // Slow path: Trees differ, need reconstruction
    debug_log(&format!(
        "Reconstructing authorship for {} -> {} (trees differ)",
        old_sha, new_sha
    ));

    let new_authorship_log = reconstruct_authorship_for_commit(repo, old_sha, new_sha)?;

    // Save the reconstructed log
    let authorship_json = new_authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::git::refs::notes_add(repo, new_sha, &authorship_json)?;

    Ok(())
}

/// Check if two commits have identical trees
fn trees_identical(commit1: &Commit, commit2: &Commit) -> Result<bool, GitAiError> {
    let tree1 = commit1.tree()?;
    let tree2 = commit2.tree()?;
    Ok(tree1.id() == tree2.id())
}

/// Copy authorship log from one commit to another
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

/// Reconstruct authorship for a single commit that changed during rebase
fn reconstruct_authorship_for_commit(
    repo: &Repository,
    old_sha: &str,
    new_sha: &str,
) -> Result<AuthorshipLog, GitAiError> {
    // Cache for foreign prompts to avoid repeated grepping
    let mut foreign_prompts_cache: HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    > = HashMap::new();
    // Get commits
    let old_commit = repo.find_commit(old_sha.to_string())?;
    let new_commit = repo.find_commit(new_sha.to_string())?;
    let new_parent = new_commit.parent(0)?;
    let old_parent = old_commit.parent(0)?;

    // Create "hanging commit" for blame context
    // This applies the changes from (old_parent -> new_parent) onto old_commit
    let hanging_commit_sha = apply_diff_as_merge_commit(
        repo,
        &old_parent.id().to_string(),
        &new_parent.id().to_string(),
        old_sha,
    )?;

    // Reconstruct authorship by running blame in hanging commit context
    let mut reconstructed_log = reconstruct_authorship_from_diff(
        repo,
        &new_commit,
        &new_parent,
        &hanging_commit_sha,
        &mut AuthorshipLogCache::new(),
        &mut foreign_prompts_cache,
    )?;

    // Set the base_commit_sha to the new commit
    reconstructed_log.metadata.base_commit_sha = new_sha.to_string();

    // Cleanup
    delete_hanging_commit(repo, &hanging_commit_sha)?;

    Ok(reconstructed_log)
}

#[allow(dead_code)]
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

    // Create VirtualAttributions from working log checkpoints and get authorship log
    let repo_clone = repo.clone();
    let (_working_va, mut authorship_log) = smol::block_on(async {
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

    // Update base commit SHA to the amended commit
    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Save the authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::git::refs::notes_add(repo, amended_commit, &authorship_json)?;

    // Clean up working log
    repo.storage
        .delete_working_log_for_base_commit(original_commit)?;

    Ok(authorship_log)
}

/// Apply a diff as a merge commit, creating a hanging commit that's not attached to any branch
///
/// This function takes the diff between origin_base and new_commit_parent and applies it
/// to the old_head_sha, creating a merge commit where conflicts are resolved by accepting
/// the old head's version (Accept Theirs strategy).
///
/// # Arguments
/// * `repo` - Git repository
/// * `origin_base` - The common base commit SHA
/// * `new_commit_parent` - The new commit's parent SHA
/// * `old_head_sha` - The HEAD of the old shas history
///
/// # Returns
/// The SHA of the created hanging commit
fn apply_diff_as_merge_commit(
    repo: &Repository,
    origin_base: &str,
    new_commit_parent: &str,
    old_head_sha: &str,
) -> Result<String, GitAiError> {
    // Resolve the merge as a real three-way merge of trees
    // base: origin_base, ours: old_head_sha, theirs: new_commit_parent
    // Favor OURS (old_head) on conflicts per comment "OLD Head wins when there's conflicts"
    let base_commit = repo.find_commit(origin_base.to_string())?;
    let ours_commit = repo.find_commit(old_head_sha.to_string())?;
    let theirs_commit = repo.find_commit(new_commit_parent.to_string())?;

    let base_tree = base_commit.tree()?;
    let ours_tree = ours_commit.tree()?;
    let theirs_tree = theirs_commit.tree()?;

    // TODO Verify new version is correct (we should be getting a tree oid straight back from merge_trees_favor_ours)
    let tree_oid = repo.merge_trees_favor_ours(&base_tree, &ours_tree, &theirs_tree)?;
    let merged_tree = repo.find_tree(tree_oid)?;

    // Create the hanging commit with ONLY the feature branch (ours) as parent
    // This is critical: by having only one parent, git blame will trace through
    // the feature branch history where AI authorship logs exist, rather than
    // potentially tracing through the target branch lineage
    let merge_commit = repo.commit(
        None,
        &ours_commit.author()?,
        &ours_commit.committer()?,
        &format!(
            "Merge diff from {} to {} onto {}",
            origin_base, new_commit_parent, old_head_sha
        ),
        &merged_tree,
        &[&ours_commit], // Only feature branch as parent!
    )?;

    Ok(merge_commit.to_string())
}

/// Delete a hanging commit that's not attached to any branch
///
/// This function removes a commit from the git object database. Since the commit
/// is hanging (not referenced by any branch or tag), it will be garbage collected
/// by git during the next gc operation.
///
/// # Arguments
/// * `repo` - Git repository
/// * `commit_sha` - SHA of the commit to delete
fn delete_hanging_commit(repo: &Repository, commit_sha: &str) -> Result<(), GitAiError> {
    // Find the commit to verify it exists
    let _commit = repo.find_commit(commit_sha.to_string())?;

    // Delete the commit using git command
    let _output = std::process::Command::new(crate::config::Config::get().git_cmd())
        .arg("update-ref")
        .arg("-d")
        .arg(format!("refs/heads/temp-{}", commit_sha))
        .current_dir(repo.path().parent().unwrap())
        .output()?;

    Ok(())
}

/// Reconstruct authorship history from a diff by running blame in the context of a hanging commit
///
/// This is the core logic that takes the diff between new_commit and new_commit_parent,
/// iterates through each file and hunk, and runs blame in the context of the hanging_commit_sha
/// to reconstruct the pre-squash authorship information.
///
/// # Arguments
/// * `repo` - Git repository
/// * `new_commit` - The new squashed commit
/// * `new_commit_parent` - The parent of the new commit
/// * `hanging_commit_sha` - The hanging commit that contains the pre-squash history
///
/// # Returns
/// A new AuthorshipLog with reconstructed authorship information
fn reconstruct_authorship_from_diff(
    repo: &Repository,
    new_commit: &Commit,
    new_commit_parent: &Commit,
    hanging_commit_sha: &str,
    authorship_log_cache: &mut AuthorshipLogCache,
    foreign_prompts_cache: &mut HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    >,
) -> Result<AuthorshipLog, GitAiError> {
    use std::collections::{HashMap, HashSet};

    // Get the trees for the diff
    let new_tree = new_commit.tree()?;
    let parent_tree = new_commit_parent.tree()?;

    // Create diff between new_commit and new_commit_parent using Git CLI
    let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&new_tree), None, None)?;

    let mut authorship_entries = Vec::new();

    let deltas: Vec<_> = diff.deltas().collect();
    debug_log(&format!("Diff has {} deltas", deltas.len()));

    // Create blame cache to avoid running git blame multiple times per file
    // OPTIMIZATION: Instead of running "git blame file.rs -L 42,42" for each inserted line,
    // we run "git blame file.rs" ONCE per file and cache all hunks. This reduces subprocess
    // calls from O(total_inserted_lines) to O(changed_files), a ~100x improvement.
    // Key: file_path, Value: Vec<BlameHunk> for entire file
    let mut blame_cache: HashMap<String, Vec<crate::commands::blame::BlameHunk>> = HashMap::new();

    // Iterate through each file in the diff
    for delta in deltas {
        let old_file_path = delta.old_file().path();
        let new_file_path = delta.new_file().path();

        // Use the new file path if available, otherwise old file path
        let file_path = new_file_path
            .or(old_file_path)
            .ok_or_else(|| GitAiError::Generic("File path not available".to_string()))?;

        let file_path_str = file_path.to_string_lossy().to_string();

        // Get the content of the file from both trees
        let old_content =
            if let Ok(entry) = parent_tree.get_path(std::path::Path::new(&file_path_str)) {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let content = blob.content()?;
                    String::from_utf8_lossy(&content).to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

        let new_content = if let Ok(entry) = new_tree.get_path(std::path::Path::new(&file_path_str))
        {
            if let Ok(blob) = repo.find_blob(entry.id()) {
                let content = blob.content()?;
                String::from_utf8_lossy(&content).to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Pull the file content from the hanging commit to map inserted text to historical lines
        let hanging_commit = repo.find_commit(hanging_commit_sha.to_string())?;
        let hanging_tree = hanging_commit.tree()?;
        let hanging_content =
            if let Ok(entry) = hanging_tree.get_path(std::path::Path::new(&file_path_str)) {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let content = blob.content()?;
                    String::from_utf8_lossy(&content).to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

        // Create a text diff between the old and new content
        let diff = TextDiff::from_lines(&old_content, &new_content);
        let mut _old_line = 1u32;
        let mut new_line = 1u32;
        let hanging_lines: Vec<&str> = hanging_content.lines().collect();
        let mut used_hanging_line_numbers: HashSet<u32> = HashSet::new();

        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Equal => {
                    // Equal lines are unchanged - skip them for now
                    // TODO: May need to handle moved lines in the future
                    // Count newlines instead of lines() to handle trailing newlines correctly
                    let line_count = change.value().matches('\n').count() as u32;
                    _old_line += line_count;
                    new_line += line_count;
                }
                ChangeTag::Delete => {
                    // Deleted lines only advance the old line counter
                    // Count newlines instead of lines() to handle trailing newlines correctly
                    _old_line += change.value().matches('\n').count() as u32;
                }
                ChangeTag::Insert => {
                    let change_value = change.value();
                    let inserted: Vec<&str> = change_value.lines().collect();
                    // Count actual newlines for accurate line tracking
                    let actual_line_count = change_value.matches('\n').count() as u32;

                    debug_log(&format!(
                        "Found {} inserted lines in file {}",
                        inserted.len(),
                        file_path_str
                    ));

                    // For each inserted line, try to find the same content in the hanging commit
                    for (_i, inserted_line) in inserted.iter().enumerate() {
                        // Find a matching line number in hanging content, prefer the first not yet used
                        let mut matched_hanging_line: Option<u32> = None;
                        for (idx, h_line) in hanging_lines.iter().enumerate() {
                            if h_line == inserted_line {
                                let candidate = (idx as u32) + 1; // 1-indexed
                                if !used_hanging_line_numbers.contains(&candidate) {
                                    matched_hanging_line = Some(candidate);
                                    break;
                                }
                            }
                        }

                        // Only try to blame lines that exist in the hanging commit
                        // If we can't find a match, the line is new and has no historical authorship
                        if let Some(h_line_no) = matched_hanging_line {
                            used_hanging_line_numbers.insert(h_line_no);

                            let blame_result = run_blame_in_context(
                                repo,
                                &file_path_str,
                                h_line_no,
                                hanging_commit_sha,
                                authorship_log_cache,
                                foreign_prompts_cache,
                                &mut blame_cache,
                            );

                            // Handle blame errors gracefully (e.g., file doesn't exist in hanging commit)
                            match blame_result {
                                Ok(Some((author, prompt))) => {
                                    authorship_entries.push((
                                        file_path_str.clone(),
                                        h_line_no,
                                        author,
                                        prompt,
                                    ));
                                }
                                Ok(None) => {
                                    // No authorship found, that's ok
                                }
                                Err(e) => {
                                    // Log the error but continue processing other lines
                                    debug_log(&format!(
                                        "Failed to blame line {} in {}: {}",
                                        h_line_no, file_path_str, e
                                    ));
                                }
                            }
                        }
                        // else: Line doesn't exist in hanging commit, skip it (no historical authorship)
                    }

                    // Use actual newline count for accurate line tracking
                    new_line += actual_line_count;
                }
            }
        }
    }

    // Convert the collected entries into an AuthorshipLog
    let mut authorship_log = AuthorshipLog::new();

    // Group entries by file and prompt session ID for efficiency
    let mut file_attestations: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
    let mut prompt_records: HashMap<String, crate::authorship::authorship_log::PromptRecord> =
        HashMap::new();

    for (file_path, line_number, _author, prompt) in authorship_entries {
        // Only process AI-generated content (entries with prompt)
        if let Some((prompt_record, _turn)) = prompt {
            let prompt_session_id = prompt_record.agent_id.id.clone();

            // Store prompt record (preserving total_additions and total_deletions from original)
            prompt_records.insert(prompt_session_id.clone(), prompt_record);

            file_attestations
                .entry(file_path)
                .or_insert_with(HashMap::new)
                .entry(prompt_session_id)
                .or_insert_with(Vec::new)
                .push(line_number);
        }
    }

    // Convert grouped entries to AuthorshipLog format
    for (file_path, prompt_session_lines) in file_attestations {
        for (prompt_session_id, mut lines) in prompt_session_lines {
            // Sort lines and create ranges
            lines.sort();
            let mut ranges = Vec::new();
            let mut current_start = lines[0];
            let mut current_end = lines[0];

            for &line in &lines[1..] {
                if line == current_end + 1 {
                    // Extend current range
                    current_end = line;
                } else {
                    // Start new range
                    if current_start == current_end {
                        ranges.push(crate::authorship::authorship_log::LineRange::Single(
                            current_start,
                        ));
                    } else {
                        ranges.push(crate::authorship::authorship_log::LineRange::Range(
                            current_start,
                            current_end,
                        ));
                    }
                    current_start = line;
                    current_end = line;
                }
            }

            // Add the last range
            if current_start == current_end {
                ranges.push(crate::authorship::authorship_log::LineRange::Single(
                    current_start,
                ));
            } else {
                ranges.push(crate::authorship::authorship_log::LineRange::Range(
                    current_start,
                    current_end,
                ));
            }

            // Create attestation entry with the prompt session ID
            let attestation_entry =
                crate::authorship::authorship_log_serialization::AttestationEntry::new(
                    prompt_session_id.clone(),
                    ranges,
                );

            // Add to authorship log
            let file_attestation = authorship_log.get_or_create_file(&file_path);
            file_attestation.add_entry(attestation_entry);
        }
    }

    // Store prompt records in metadata (preserving total_additions and total_deletions)
    for (prompt_session_id, prompt_record) in prompt_records {
        authorship_log
            .metadata
            .prompts
            .insert(prompt_session_id, prompt_record);
    }

    // Sort attestation entries by hash for deterministic ordering
    for file_attestation in &mut authorship_log.attestations {
        file_attestation.entries.sort_by(|a, b| a.hash.cmp(&b.hash));
    }

    // Calculate accepted_lines for each prompt based on final attestation log
    let mut session_accepted_lines: HashMap<String, u32> = HashMap::new();
    for file_attestation in &authorship_log.attestations {
        for attestation_entry in &file_attestation.entries {
            let accepted_count: u32 = attestation_entry
                .line_ranges
                .iter()
                .map(|range| match range {
                    crate::authorship::authorship_log::LineRange::Single(_) => 1,
                    crate::authorship::authorship_log::LineRange::Range(start, end) => {
                        end - start + 1
                    }
                })
                .sum();
            *session_accepted_lines
                .entry(attestation_entry.hash.clone())
                .or_insert(0) += accepted_count;
        }
    }

    // Update accepted_lines for all PromptRecords
    // Note: total_additions and total_deletions are preserved from the original prompt records
    for (session_id, prompt_record) in authorship_log.metadata.prompts.iter_mut() {
        prompt_record.accepted_lines = *session_accepted_lines.get(session_id).unwrap_or(&0);
    }

    Ok(authorship_log)
}

/// Run blame on a specific line in the context of a hanging commit and return AI authorship info
///
/// This function runs blame on a specific line number in a file, then looks up the AI authorship
/// log for the blamed commit to get the full authorship information including prompt details.
///
/// # Arguments
/// * `repo` - Git repository
/// * `file_path` - Path to the file
/// * `line_number` - Line number to blame (1-indexed)
/// * `hanging_commit_sha` - SHA of the hanging commit to use as context
///
/// # Returns
/// The AI authorship information (author and prompt) for the line, or None if not found
fn run_blame_in_context(
    repo: &Repository,
    file_path: &str,
    line_number: u32,
    hanging_commit_sha: &str,
    authorship_log_cache: &mut AuthorshipLogCache,
    foreign_prompts_cache: &mut HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    >,
    blame_cache: &mut HashMap<String, Vec<crate::commands::blame::BlameHunk>>,
) -> Result<
    Option<(
        crate::authorship::authorship_log::Author,
        Option<(crate::authorship::authorship_log::PromptRecord, u32)>,
    )>,
    GitAiError,
> {
    // Get or compute blame for entire file (cached)
    let blame_hunks = blame_cache.entry(file_path.to_string()).or_insert_with(|| {
        // Find the hanging commit
        let hanging_commit = match repo.find_commit(hanging_commit_sha.to_string()) {
            Ok(commit) => commit,
            Err(_) => return Vec::new(),
        };

        // Create blame options for the entire file
        let mut blame_opts = GitAiBlameOptions::default();
        blame_opts.newest_commit = Some(hanging_commit.id().to_string());

        // Run blame on the ENTIRE file in the context of the hanging commit
        match repo.blame_hunks(file_path, 1, u32::MAX, &blame_opts) {
            Ok(hunks) => hunks,
            Err(_) => Vec::new(),
        }
    });

    // Find the hunk that contains the requested line number
    let hunk = blame_hunks
        .iter()
        .find(|h| line_number >= h.range.0 && line_number <= h.range.1);

    if let Some(hunk) = hunk {
        let commit_sha = &hunk.commit_sha;

        // Look up the AI authorship log for this commit using the cache
        let authorship_log = match authorship_log_cache.get_or_fetch(repo, commit_sha) {
            Ok(log) => log,
            Err(_) => {
                // No AI authorship data for this commit, fall back to git author
                let commit = repo.find_commit(commit_sha.to_string())?;
                let author = commit.author()?;
                let author_name = author.name().unwrap_or("unknown");
                let author_email = author.email().unwrap_or("");

                let author_info = crate::authorship::authorship_log::Author {
                    username: author_name.to_string(),
                    email: author_email.to_string(),
                };

                return Ok(Some((author_info, None)));
            }
        };

        // Get the line attribution from the AI authorship log
        // Use the ORIGINAL line number from the blamed commit, not the current line number
        let orig_line_to_lookup = hunk.orig_range.0;

        if let Some((author, _, prompt)) = authorship_log.get_line_attribution(
            repo,
            file_path,
            orig_line_to_lookup,
            foreign_prompts_cache,
        ) {
            Ok(Some((author.clone(), prompt.map(|p| (p.clone(), 0)))))
        } else {
            // Line not found in authorship log, fall back to git author
            let commit = repo.find_commit(commit_sha.to_string())?;
            let author = commit.author()?;
            let author_name = author.name().unwrap_or("unknown");
            let author_email = author.email().unwrap_or("");

            let author_info = crate::authorship::authorship_log::Author {
                username: author_name.to_string(),
                email: author_email.to_string(),
            };

            Ok(Some((author_info, None)))
        }
    } else {
        Ok(None)
    }
}

/// Find the common origin base between the head commit and the new commit's branch
fn find_common_origin_base_from_head(
    repo: &Repository,
    head_sha: &str,
    new_sha: &str,
) -> Result<String, GitAiError> {
    let new_commit = repo.find_commit(new_sha.to_string())?;
    let head_commit = repo.find_commit(head_sha.to_string())?;

    // Find the merge base between the head commit and the new commit
    let merge_base = repo.merge_base(head_commit.id(), new_commit.id())?;

    Ok(merge_base.to_string())
}

/// Build a path of commit SHAs from head_sha to the origin base
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

/// Reconstruct working log after a reset that preserves working directory
///
/// This handles --soft, --mixed, and --merge resets where we move HEAD backward
/// but keep the working directory state. We need to create a working log that
/// captures AI authorship from the "unwound" commits plus any existing uncommitted changes.
pub fn reconstruct_working_log_after_reset(
    repo: &Repository,
    target_commit_sha: &str, // Where we reset TO
    old_head_sha: &str,      // Where HEAD was BEFORE reset
    human_author: &str,
) -> Result<(), GitAiError> {
    // Cache for foreign prompts to avoid repeated grepping
    let mut foreign_prompts_cache: HashMap<
        String,
        Option<crate::authorship::authorship_log::PromptRecord>,
    > = HashMap::new();
    debug_log(&format!(
        "Reconstructing working log after reset from {} to {}",
        old_head_sha, target_commit_sha
    ));

    // Step 1: Create a temporary commit representing the working directory (staged + unstaged)
    // This has the target commit as parent
    let working_tree_oid = capture_working_directory_as_tree(repo)?;
    let working_tree = repo.find_tree(working_tree_oid)?;

    let target_commit = repo.find_commit(target_commit_sha.to_string())?;
    let temp_working_commit = repo.commit(
        None,
        &target_commit.author()?,
        &target_commit.committer()?,
        "Temporary commit representing working directory after reset",
        &working_tree,
        &[&target_commit],
    )?;

    // Step 2: Create hanging commit with the working directory tree but old_head as parent
    // This preserves AI authorship from old_head while having working directory content
    let old_head_commit = repo.find_commit(old_head_sha.to_string())?;
    let hanging_commit_sha = repo.commit(
        None,
        &old_head_commit.author()?,
        &old_head_commit.committer()?,
        &format!(
            "Hanging commit for reset: working dir with parent {}",
            old_head_sha
        ),
        &working_tree,       // Same tree as working directory
        &[&old_head_commit], // But parent is old_head (preserves AI context)
    )?;

    // Step 3: Attach authorship log from old HEAD to hanging commit
    // This is essential so that blame can find the AI authorship
    create_authorship_log_for_hanging_commit(
        repo,
        old_head_sha,
        &hanging_commit_sha.to_string(),
        human_author,
    )?;

    // Step 4: Reconstruct authorship via blame
    // Compare: working directory (temp) vs target
    // Context: hanging commit (which has old_head's AI authorship)
    let temp_commit_obj = repo.find_commit(temp_working_commit.to_string())?;
    let target_commit_obj = repo.find_commit(target_commit_sha.to_string())?;

    debug_log(&format!(
        "Running reconstruct_authorship_from_diff: temp={}, target={}, hanging={}",
        temp_working_commit, target_commit_sha, hanging_commit_sha
    ));

    let new_authorship_log = reconstruct_authorship_from_diff(
        repo,
        &temp_commit_obj,
        &target_commit_obj,
        &hanging_commit_sha.to_string(),
        &mut AuthorshipLogCache::new(),
        &mut foreign_prompts_cache,
    )?;

    debug_log(&format!(
        "Reconstructed authorship log has {} file attestations, {} prompts",
        new_authorship_log.attestations.len(),
        new_authorship_log.metadata.prompts.len()
    ));
    eprintln!("new_authorship_log: {:?}", new_authorship_log);

    // Step 4: Build file contents map from index
    use std::collections::HashMap;
    let mut file_contents: HashMap<String, String> = HashMap::new();
    for file_attestation in &new_authorship_log.attestations {
        // Read from index using git show :path
        let mut args = repo.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!(":{}", file_attestation.file_path));

        let output = crate::git::repository::exec_git(&args)?;
        let file_content = String::from_utf8(output.stdout).map_err(|_| {
            GitAiError::Generic(format!(
                "Failed to read file: {}",
                file_attestation.file_path
            ))
        })?;
        file_contents.insert(file_attestation.file_path.clone(), file_content);
    }

    // Step 5: Convert to checkpoints
    let mut checkpoints = new_authorship_log
        .convert_to_checkpoints_for_squash(&file_contents)
        .map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to convert authorship log to checkpoints: {}",
                e
            ))
        })?;

    // Filter to keep only AI checkpoints - we don't track human-only changes in working logs
    checkpoints.retain(|checkpoint| checkpoint.kind != CheckpointKind::Human);

    // Step 6: Persist blobs and write working log
    let new_working_log = repo.storage.working_log_for_base_commit(target_commit_sha);
    // reset first (can have stuff in it from before if you're operating on HEAD)
    new_working_log.reset_working_log()?;

    for checkpoint in &mut checkpoints {
        persist_checkpoint_blobs(repo, checkpoint, target_commit_sha)?;
        new_working_log.append_checkpoint(checkpoint)?;
    }

    // Step 7: Cleanup hanging commits
    delete_hanging_commit(repo, &hanging_commit_sha.to_string())?;
    delete_hanging_commit(repo, &temp_working_commit.to_string())?;

    // Delete old working log
    repo.storage
        .delete_working_log_for_base_commit(old_head_sha)?;

    debug_log(&format!(
        "Created {} checkpoints in working log for {}",
        checkpoints.len(),
        target_commit_sha
    ));

    Ok(())
}

/// Helper: Capture current working directory + index as a tree
fn capture_working_directory_as_tree(repo: &Repository) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("write-tree".to_string());
    let output = crate::git::repository::exec_git(&args)?;
    let tree_oid = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(tree_oid)
}

/// Helper: Persist file blobs for a checkpoint (reuse from prepare_working_log_after_squash)
fn persist_checkpoint_blobs(
    repo: &Repository,
    checkpoint: &mut crate::authorship::working_log::Checkpoint,
    base_commit_sha: &str,
) -> Result<(), GitAiError> {
    use sha2::{Digest, Sha256};

    let working_log = repo.storage.working_log_for_base_commit(base_commit_sha);
    let mut file_hashes = Vec::new();

    for entry in &mut checkpoint.entries {
        // Read from index using git show :path
        let mut args = repo.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!(":{}", entry.file));

        let output = crate::git::repository::exec_git(&args)?;
        let file_content = String::from_utf8(output.stdout)
            .map_err(|_| GitAiError::Generic(format!("Failed to read file: {}", entry.file)))?;

        let blob_sha = working_log.persist_file_version(&file_content)?;
        entry.blob_sha = blob_sha.clone();
        file_hashes.push((entry.file.clone(), blob_sha));
    }

    // Compute combined hash
    file_hashes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut combined_hasher = Sha256::new();
    for (file_path, hash) in &file_hashes {
        combined_hasher.update(file_path.as_bytes());
        combined_hasher.update(hash.as_bytes());
    }
    checkpoint.diff = format!("{:x}", combined_hasher.finalize());

    Ok(())
}

/// Create an authorship log for the hanging commit by combining the existing
/// authorship log and working log from the old HEAD commit
fn create_authorship_log_for_hanging_commit(
    repo: &Repository,
    old_head_sha: &str,
    hanging_commit_sha: &str,
    human_author: &str,
) -> Result<(), GitAiError> {
    debug_log(&format!(
        "Creating authorship log for hanging commit {} from old HEAD {}",
        hanging_commit_sha, old_head_sha
    ));

    // Step 1: Try to get existing authorship log from old HEAD
    let mut authorship_log = match get_reference_as_authorship_log_v3(repo, old_head_sha) {
        Ok(log) => {
            debug_log(&format!(
                "Found existing authorship log for {} with {} attestations",
                old_head_sha,
                log.attestations.len()
            ));
            log
        }
        Err(_) => {
            debug_log(&format!("No existing authorship log for {}", old_head_sha));
            AuthorshipLog::new()
        }
    };

    // Step 2: Get working log for old HEAD (if exists)
    let working_log = repo.storage.working_log_for_base_commit(old_head_sha);
    let checkpoints = match working_log.read_all_checkpoints() {
        Ok(checkpoints) if !checkpoints.is_empty() => {
            debug_log(&format!(
                "Found {} checkpoints in working log for {}",
                checkpoints.len(),
                old_head_sha
            ));
            checkpoints
        }
        _ => {
            debug_log(&format!("No working log for {}", old_head_sha));
            Vec::new()
        }
    };

    // Step 3: Apply checkpoints to authorship log (like post_commit does)
    if !checkpoints.is_empty() {
        let mut session_additions = std::collections::HashMap::new();
        let mut session_deletions = std::collections::HashMap::new();

        for checkpoint in &checkpoints {
            authorship_log.apply_checkpoint(
                checkpoint,
                Some(human_author),
                &mut session_additions,
                &mut session_deletions,
            );
        }

        authorship_log.finalize(&session_additions, &session_deletions);

        debug_log(&format!(
            "Applied working log checkpoints, now have {} attestations, {} prompts",
            authorship_log.attestations.len(),
            authorship_log.metadata.prompts.len()
        ));
    }

    // Step 4: Set the base_commit_sha to the hanging commit
    authorship_log.metadata.base_commit_sha = hanging_commit_sha.to_string();

    // Step 5: Save the authorship log to the hanging commit
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::git::refs::notes_add(repo, hanging_commit_sha, &authorship_json)?;

    debug_log(&format!(
        "Attached authorship log to hanging commit {} with {} attestations",
        hanging_commit_sha,
        authorship_log.attestations.len()
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{find_repository_in_path, test_utils::TmpRepo};
    use insta::assert_debug_snapshot;

    // Test amending a commit by adding AI-authored lines at the top of the file.
    ///
    /// Note: The snapshot's `base_commit_sha` will differ on each run since we create
    /// new commits. The important parts to verify are:
    /// - Line ranges are correct (lines 1-2 for AI additions)
    /// - Metrics are accurate (total_additions, accepted_lines)
    /// - Prompts and agent info are preserved
    ///
    #[test]
    fn test_amend_add_lines_at_top() {
        // Create a repo with an initial commit containing human-authored content
        let tmp_repo = TmpRepo::new().unwrap();

        // Initial file with human content
        let initial_content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        tmp_repo
            .write_file("test.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        let initial_log = tmp_repo.commit_with_message("Initial commit").unwrap();

        // Get the original commit SHA
        let original_commit = tmp_repo.get_head_commit_sha().unwrap();

        // Now make AI changes - add lines at the top
        let amended_content =
            "// AI added line 1\n// AI added line 2\nline 1\nline 2\nline 3\nline 4\nline 5\n";
        tmp_repo
            .write_file("test.txt", amended_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Amend the commit
        let amended_commit = tmp_repo.amend_commit("Initial commit (amended)").unwrap();

        // Run the rewrite function
        let mut authorship_log = rewrite_authorship_after_commit_amend(
            &tmp_repo.gitai_repo(),
            &original_commit,
            &amended_commit,
            "Test User <test@example.com>".to_string(),
        )
        .unwrap();

        // Clear commit SHA for stable snapshots
        authorship_log.metadata.base_commit_sha = "".to_string();
        assert_debug_snapshot!(authorship_log);
    }

    #[test]
    fn test_amend_add_lines_in_middle() {
        // Create a repo with an initial commit containing human-authored content
        let tmp_repo = TmpRepo::new().unwrap();

        // Initial file with human content
        let initial_content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        tmp_repo
            .write_file("test.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();

        // Get the original commit SHA
        let original_commit = tmp_repo.get_head_commit_sha().unwrap();

        // Now make AI changes - add lines in the middle
        let amended_content = "line 1\nline 2\n// AI inserted line 1\n// AI inserted line 2\nline 3\nline 4\nline 5\n";
        tmp_repo
            .write_file("test.txt", amended_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Amend the commit
        let amended_commit = tmp_repo.amend_commit("Initial commit (amended)").unwrap();

        // Run the rewrite function
        let mut authorship_log = rewrite_authorship_after_commit_amend(
            &tmp_repo.gitai_repo(),
            &original_commit,
            &amended_commit,
            "Test User <test@example.com>".to_string(),
        )
        .unwrap();

        // Clear commit SHA for stable snapshots
        authorship_log.metadata.base_commit_sha = "".to_string();
        assert_debug_snapshot!(authorship_log);
    }

    #[test]
    fn test_amend_add_lines_at_bottom() {
        // Create a repo with an initial commit containing human-authored content
        let tmp_repo = TmpRepo::new().unwrap();

        // Initial file with human content
        let initial_content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        tmp_repo
            .write_file("test.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();

        // Get the original commit SHA
        let original_commit = tmp_repo.get_head_commit_sha().unwrap();

        // Now make AI changes - add lines at the bottom
        let amended_content = "line 1\nline 2\nline 3\nline 4\nline 5\n// AI appended line 1\n// AI appended line 2\n";
        tmp_repo
            .write_file("test.txt", amended_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Amend the commit
        let amended_commit = tmp_repo.amend_commit("Initial commit (amended)").unwrap();

        // Run the rewrite function
        let mut authorship_log = rewrite_authorship_after_commit_amend(
            &tmp_repo.gitai_repo(),
            &original_commit,
            &amended_commit,
            "Test User <test@example.com>".to_string(),
        )
        .unwrap();

        // Clear commit SHA for stable snapshots
        authorship_log.metadata.base_commit_sha = "".to_string();
        assert_debug_snapshot!(authorship_log);
    }

    #[test]
    #[ignore]
    fn test_amend_multiple_changes() {
        // Create a repo with an initial commit containing AI-authored content
        let tmp_repo = TmpRepo::new().unwrap();

        // Initial file with AI content
        let initial_content = "function example() {\n  return 42;\n}\n";
        tmp_repo
            .write_file("code.js", initial_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent_1", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo
            .commit_with_message("Add example function")
            .unwrap();

        // Get the original commit SHA
        let original_commit = tmp_repo.get_head_commit_sha().unwrap();

        // First amendment - add at top
        let content_v2 = "// Header comment\nfunction example() {\n  return 42;\n}\n";
        tmp_repo.write_file("code.js", content_v2, true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent_2", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Second amendment - add in middle
        let content_v3 =
            "// Header comment\nfunction example() {\n  // Added documentation\n  return 42;\n}\n";
        tmp_repo.write_file("code.js", content_v3, true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent_3", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Third amendment - add at bottom
        let content_v4 = "// Header comment\nfunction example() {\n  // Added documentation\n  return 42;\n}\n\n// Footer\n";
        tmp_repo.write_file("code.js", content_v4, true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent_4", Some("gpt-4"), Some("cursor"))
            .unwrap();

        // Amend the commit
        let amended_commit = tmp_repo
            .amend_commit("Add example function (amended)")
            .unwrap();

        // Run the rewrite function
        let mut authorship_log = rewrite_authorship_after_commit_amend(
            &tmp_repo.gitai_repo(),
            &original_commit,
            &amended_commit,
            "Test User <test@example.com>".to_string(),
        )
        .unwrap();

        // Clear commit SHA for stable snapshots
        authorship_log.metadata.base_commit_sha = "".to_string();
        assert_debug_snapshot!(authorship_log);
    }

    /// Test merge --squash with a simple feature branch containing AI and human edits
    #[test]
    fn test_prepare_working_log_simple_squash() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create master branch with initial content
        let initial_content = "line 1\nline 2\nline 3\n";
        tmp_repo
            .write_file("main.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo
            .commit_with_message("Initial commit on master")
            .unwrap();
        let master_head = tmp_repo.get_head_commit_sha().unwrap();

        // Create feature branch
        tmp_repo.create_branch("feature").unwrap();

        // Add AI changes on feature branch
        let feature_content = "line 1\nline 2\nline 3\n// AI added feature\n";
        tmp_repo
            .write_file("main.txt", feature_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("Add AI feature").unwrap();

        // Add human changes on feature branch
        let feature_content_v2 =
            "line 1\nline 2\nline 3\n// AI added feature\n// Human refinement\n";
        tmp_repo
            .write_file("main.txt", feature_content_v2, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Human refinement").unwrap();
        let feature_head = tmp_repo.get_head_commit_sha().unwrap();

        // Go back to master and squash merge
        tmp_repo.checkout_branch("master").unwrap();
        tmp_repo.merge_squash("feature").unwrap();

        // Test prepare_working_log_after_squash
        let checkpoints = prepare_working_log_after_squash(
            &tmp_repo.gitai_repo(),
            &feature_head,
            &master_head,
            "Test User <test@example.com>",
        )
        .unwrap();

        // Should have 1 checkpoint: 1 AI only (no human checkpoint)
        assert_eq!(checkpoints.len(), 1);

        // Checkpoint should be AI
        assert_eq!(checkpoints[0].author, "ai");
        assert!(checkpoints[0].agent_id.is_some());
        assert!(checkpoints[0].transcript.is_some());

        // Verify checkpoint has entries
        assert!(!checkpoints[0].entries.is_empty());

        // Verify blob is saved
        assert!(!checkpoints[0].entries[0].blob_sha.is_empty());
    }

    /// Test merge --squash with out-of-band changes on master (handles 3-way merge)
    /// This tests the scenario where commits are made on master AFTER the feature branch diverges
    #[test]
    fn test_prepare_working_log_squash_with_main_changes() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create master branch with initial content (common base)
        let initial_content = "section 1\nsection 2\nsection 3\n";
        tmp_repo
            .write_file("document.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let _common_base = tmp_repo.get_head_commit_sha().unwrap();

        // Create feature branch and add AI changes
        tmp_repo.create_branch("feature").unwrap();

        // AI adds content at the END (non-conflicting with master changes)
        let feature_content = "section 1\nsection 2\nsection 3\n// AI feature addition at end\n";
        tmp_repo
            .write_file("document.txt", feature_content, true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI adds feature").unwrap();
        let feature_head = tmp_repo.get_head_commit_sha().unwrap();

        // Switch back to master and make out-of-band changes
        // These happen AFTER feature branch diverged but BEFORE we decide to merge
        tmp_repo.checkout_branch("master").unwrap();
        let master_content = "// Master update at top\nsection 1\nsection 2\nsection 3\n";
        tmp_repo
            .write_file("document.txt", master_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo
            .commit_with_message("Out-of-band update on master")
            .unwrap();
        let master_head = tmp_repo.get_head_commit_sha().unwrap();

        // Now squash merge feature into master
        // The squashed result should have BOTH changes:
        // - Master's line at top
        // - Feature's AI line at bottom
        tmp_repo.merge_squash("feature").unwrap();

        // Test prepare_working_log_after_squash
        let checkpoints = prepare_working_log_after_squash(
            &tmp_repo.gitai_repo(),
            &feature_head,
            &master_head,
            "Test User <test@example.com>",
        )
        .unwrap();

        // The key thing we're testing is that it doesn't crash with out-of-band changes
        // and properly handles the 3-way merge scenario
        println!("Checkpoints generated: {}", checkpoints.len());
        for (i, checkpoint) in checkpoints.iter().enumerate() {
            println!(
                "Checkpoint {}: author={}, has_agent={}, entries={}",
                i,
                checkpoint.author,
                checkpoint.agent_id.is_some(),
                checkpoint.entries.len()
            );
        }

        // Should have at least some checkpoints
        assert!(
            !checkpoints.is_empty(),
            "Should generate at least one checkpoint from squash merge"
        );

        // Verify at least one checkpoint has content
        let has_content = checkpoints.iter().any(|c| !c.entries.is_empty());
        assert!(has_content, "At least one checkpoint should have entries");
    }

    /// Test merge --squash with multiple AI sessions and human edits
    #[test]
    fn test_prepare_working_log_squash_multiple_sessions() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create master branch
        let initial_content = "header\nbody\nfooter\n";
        tmp_repo
            .write_file("file.txt", initial_content, true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Initial").unwrap();
        let master_head = tmp_repo.get_head_commit_sha().unwrap();

        // Create feature branch
        tmp_repo.create_branch("feature").unwrap();

        // First AI session
        let content_v2 = "header\n// AI session 1\nbody\nfooter\n";
        tmp_repo.write_file("file.txt", content_v2, true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_session_1", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI session 1").unwrap();

        // Human edit
        let content_v3 = "header\n// AI session 1\nbody\n// Human addition\nfooter\n";
        tmp_repo.write_file("file.txt", content_v3, true).unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Human edit").unwrap();

        // Second AI session
        let content_v4 =
            "header\n// AI session 1\nbody\n// Human addition\nfooter\n// AI session 2\n";
        tmp_repo.write_file("file.txt", content_v4, true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_session_2", Some("claude"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI session 2").unwrap();
        let feature_head = tmp_repo.get_head_commit_sha().unwrap();

        // Squash merge into master
        tmp_repo.checkout_branch("master").unwrap();
        tmp_repo.merge_squash("feature").unwrap();

        // Test prepare_working_log_after_squash
        let checkpoints = prepare_working_log_after_squash(
            &tmp_repo.gitai_repo(),
            &feature_head,
            &master_head,
            "Test User <test@example.com>",
        )
        .unwrap();

        // Should have 2 checkpoints: 2 AI sessions (no human checkpoint)
        assert_eq!(checkpoints.len(), 2);

        // All checkpoints should be AI
        let ai_checkpoints: Vec<_> = checkpoints
            .iter()
            .filter(|c| c.agent_id.is_some())
            .collect();
        assert_eq!(ai_checkpoints.len(), 2);

        // No human checkpoints
        let human_checkpoints: Vec<_> = checkpoints
            .iter()
            .filter(|c| c.agent_id.is_none())
            .collect();
        assert_eq!(human_checkpoints.len(), 0);

        // Verify AI checkpoints have distinct session IDs
        assert_ne!(
            ai_checkpoints[0].agent_id.as_ref().unwrap().id,
            ai_checkpoints[1].agent_id.as_ref().unwrap().id
        );
    }

    /// Test merge --squash with multiple files modified by different AI sessions
    #[test]
    fn test_prepare_working_log_squash_multiple_files() {
        let tmp_repo = TmpRepo::new().unwrap();

        // Create master branch with multiple files
        tmp_repo
            .write_file(
                "src/main.rs",
                "fn main() {\n    println!(\"Hello\");\n}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .write_file(
                "src/lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .write_file("README.md", "# My Project\n\nA simple project.\n", true)
            .unwrap();
        tmp_repo.trigger_checkpoint_with_author("human").unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();
        let master_head = tmp_repo.get_head_commit_sha().unwrap();

        // Create feature branch
        tmp_repo.create_branch("feature").unwrap();

        // First AI session modifies main.rs and lib.rs
        tmp_repo
            .write_file(
                "src/main.rs",
                "fn main() {\n    println!(\"Hello\");\n    // AI: Added logging\n    log::info!(\"Started\");\n}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .write_file(
                "src/lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 {\n    // AI: Added validation\n    a + b\n}\n",
                true,
            )
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_session_1", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo
            .commit_with_message("AI: Add logging and validation")
            .unwrap();

        // Second AI session modifies README.md only
        tmp_repo
            .write_file(
                "README.md",
                "# My Project\n\nA simple project.\n\n## AI Generated Features\n- Logging\n- Validation\n",
                true,
            )
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_session_2", Some("claude"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI: Update README").unwrap();

        // Third AI session adds a new file
        tmp_repo
            .write_file("src/utils.rs", "// AI: Utility functions\npub fn log_message(msg: &str) {\n    println!(\"{}\", msg);\n}\n", true)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("ai_session_3", Some("gpt-4"), Some("cursor"))
            .unwrap();
        tmp_repo.commit_with_message("AI: Add utils").unwrap();
        let feature_head = tmp_repo.get_head_commit_sha().unwrap();

        // Squash merge into master
        tmp_repo.checkout_branch("master").unwrap();
        tmp_repo.merge_squash("feature").unwrap();

        // Test prepare_working_log_after_squash
        let checkpoints = prepare_working_log_after_squash(
            &tmp_repo.gitai_repo(),
            &feature_head,
            &master_head,
            "Test User <test@example.com>",
        )
        .unwrap();

        // We should have checkpoints for each file modified by each session
        // Session 1 touched 2 files (main.rs, lib.rs) = 2 checkpoints
        // Session 2 touched 1 file (README.md) = 1 checkpoint
        // Session 3 touched 1 file (utils.rs) = 1 checkpoint
        // Total: 4 checkpoints
        assert_eq!(
            checkpoints.len(),
            4,
            "Should have 4 checkpoints (one per file per session)"
        );

        // All checkpoints should be AI
        let ai_checkpoints: Vec<_> = checkpoints
            .iter()
            .filter(|c| c.agent_id.is_some())
            .collect();
        assert_eq!(ai_checkpoints.len(), 4, "All checkpoints should be AI");

        // Each checkpoint should have exactly one entry
        for checkpoint in &checkpoints {
            assert_eq!(
                checkpoint.entries.len(),
                1,
                "Each checkpoint should have exactly one file entry"
            );
        }

        // Verify all checkpoints have non-empty blob_sha
        for checkpoint in &checkpoints {
            for entry in &checkpoint.entries {
                assert!(
                    !entry.blob_sha.is_empty(),
                    "Blob SHA should be set for file: {}",
                    entry.file
                );
            }
        }

        // Verify all checkpoints have non-empty diff hash
        for checkpoint in &checkpoints {
            assert!(
                !checkpoint.diff.is_empty(),
                "Diff hash should be set for checkpoint"
            );
        }

        // Collect all modified files
        let mut modified_files: Vec<String> = checkpoints
            .iter()
            .flat_map(|c| c.entries.iter().map(|e| e.file.clone()))
            .collect();
        modified_files.sort();
        modified_files.dedup();

        // Should have 4 unique files
        assert_eq!(
            modified_files.len(),
            4,
            "Should have 4 unique modified files"
        );
        assert!(modified_files.contains(&"src/main.rs".to_string()));
        assert!(modified_files.contains(&"src/lib.rs".to_string()));
        assert!(modified_files.contains(&"README.md".to_string()));
        assert!(modified_files.contains(&"src/utils.rs".to_string()));

        // Verify we have exactly 3 unique AI sessions
        let mut session_ids: Vec<String> = checkpoints
            .iter()
            .filter_map(|c| c.agent_id.as_ref().map(|id| id.id.clone()))
            .collect();
        session_ids.sort();
        session_ids.dedup();
        assert_eq!(session_ids.len(), 3, "Should have 3 unique AI sessions");
    }
}
