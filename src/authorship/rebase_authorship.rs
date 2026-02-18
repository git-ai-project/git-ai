use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::error::GitAiError;
use crate::git::authorship_traversal::{
    commits_have_authorship_notes, load_ai_touched_files_for_commits,
};
use crate::git::refs::{
    commits_with_authorship_notes, get_reference_as_authorship_log_v3, note_blob_oids_for_commits,
};
use crate::git::repository::{CommitRange, Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::{debug_log, debug_performance_log};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Default)]
struct PromptLineMetrics {
    accepted_lines: u32,
    overridden_lines: u32,
}

#[derive(Debug, Default, Clone)]
struct CommitTrackedDelta {
    changed_files: HashSet<String>,
    file_to_blob_oid: HashMap<String, Option<String>>,
}

#[derive(Debug, Default, Clone)]
struct CommitObjectMetadata {
    tree_oid: String,
    first_parent: Option<String>,
}

type ChangedFileContents = (HashSet<String>, HashMap<String, String>);
type ChangedFileContentsByCommit = HashMap<String, ChangedFileContents>;

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
        if commits_have_authorship_notes(repo, &source_commits)? {
            debug_log(
                "No AI-touched files in merge, but notes exist in source commits; writing empty authorship log",
            );
            if let Some(authorship_log) = build_metadata_only_authorship_log_from_source_notes(
                repo,
                &source_commits,
                merge_commit_sha,
            )? {
                let authorship_json = authorship_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize authorship log".to_string())
                })?;
                crate::git::refs::notes_add(repo, merge_commit_sha, &authorship_json)?;
            }
        } else {
            // No files changed, nothing to do
            debug_log("No files changed in merge, skipping authorship rewrite");
        }
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

    // Preserve accumulated totals from source commits (squash/rebase should not drop session totals).
    let mut summed_totals: HashMap<String, (u32, u32)> = HashMap::new();
    for commit_sha in &source_commits {
        if let Ok(log) = get_reference_as_authorship_log_v3(repo, commit_sha) {
            for (prompt_id, record) in log.metadata.prompts {
                let entry = summed_totals.entry(prompt_id).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(record.total_additions);
                entry.1 = entry.1.saturating_add(record.total_deletions);
            }
        }
    }

    for (prompt_id, record) in authorship_log.metadata.prompts.iter_mut() {
        if let Some((additions, deletions)) = summed_totals.get(prompt_id) {
            record.total_additions = *additions;
            record.total_deletions = *deletions;
        }
    }

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

    // Filter out commits that already have authorship logs (these are commits from the target branch).
    // Only process newly created rebased commits.
    let commits_with_logs = commits_with_authorship_notes(repo, new_commits)?;
    let commits_to_process: Vec<String> = new_commits
        .iter()
        .filter(|commit| {
            let has_log = commits_with_logs.contains(commit.as_str());
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
    let commits_to_process_lookup: HashSet<&str> =
        commits_to_process.iter().map(String::as_str).collect();
    let commit_pairs_to_process: Vec<(String, String)> = original_commits
        .iter()
        .zip(new_commits.iter())
        .filter(|(_original_commit, new_commit)| {
            commits_to_process_lookup.contains(new_commit.as_str())
        })
        .map(|(original_commit, new_commit)| (original_commit.clone(), new_commit.clone()))
        .collect();
    let original_commits_for_processing: Vec<String> = commit_pairs_to_process
        .iter()
        .map(|(original_commit, _new_commit)| original_commit.clone())
        .collect();

    // Step 1: Extract pathspecs from all original commits and narrow to AI-touched files.
    let pathspecs = get_pathspecs_from_commits(repo, original_commits)?;
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, original_commits, &pathspecs)?;

    if pathspecs.is_empty() {
        // No AI-touched files were rewritten. Preserve metadata-only / prompt-only notes by remapping
        // existing source notes to their corresponding rebased commits.
        let original_note_contents =
            load_note_contents_for_commits(repo, &original_commits_for_processing)?;
        let remapped_count =
            remap_notes_for_commit_pairs(repo, &commit_pairs_to_process, &original_note_contents)?;
        if remapped_count > 0 {
            debug_log(&format!(
                "Remapped {} metadata-only authorship notes for rebase commits",
                remapped_count
            ));
        } else {
            debug_log("No AI-touched files and no source notes to remap during rebase");
        }
        return Ok(());
    }
    let pathspecs_lookup: HashSet<&str> = pathspecs.iter().map(String::as_str).collect();

    debug_log(&format!(
        "Processing rebase: {} files modified across {} original commits -> {} new commits",
        pathspecs.len(),
        original_commits.len(),
        new_commits.len()
    ));

    if try_fast_path_rebase_note_remap(
        repo,
        original_commits,
        new_commits,
        &commits_to_process_lookup,
        &pathspecs,
    )? {
        return Ok(());
    }

    // Step 2: Create VirtualAttributions from original_head (before rebase)
    // Compute merge base to bound blame depth — without this, blame walks entire file history
    let new_head = new_commits.last().unwrap();
    let merge_base = repo
        .merge_base(original_head.to_string(), new_head.to_string())
        .ok();

    let repo_clone = repo.clone();
    let original_head_clone = original_head.to_string();
    let pathspecs_clone = pathspecs.clone();

    let current_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::new_for_base_commit(
            repo_clone,
            original_head_clone,
            &pathspecs_clone,
            merge_base,
        )
        .await
    })?;

    // Clone the original VA to use for restoring attributions when content reappears
    // This handles commit splitting where content from original_head gets re-applied
    let original_head_state_va = {
        let mut attrs = HashMap::new();
        let mut contents = HashMap::new();
        for file in current_va.files() {
            if let Some(char_attrs) = current_va.get_char_attributions(&file)
                && let Some(line_attrs) = current_va.get_line_attributions(&file)
            {
                attrs.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
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
    let original_head_line_to_author =
        build_original_head_line_author_maps(&original_head_state_va);

    // Materialize current state once, then update only changed files per commit.
    let mut current_attributions: HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    > = HashMap::new();
    let mut current_file_contents: HashMap<String, String> = HashMap::new();
    for file in current_va.files() {
        if let Some(char_attrs) = current_va.get_char_attributions(&file)
            && let Some(line_attrs) = current_va.get_line_attributions(&file)
        {
            current_attributions.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
        }
        if let Some(content) = current_va.get_file_content(&file) {
            current_file_contents.insert(file, content.clone());
        }
    }

    let mut current_prompts =
        crate::authorship::virtual_attribution::VirtualAttributions::merge_prompts_picking_newest(
            &[current_va.prompts(), original_head_state_va.prompts()],
        );
    let mut prompt_line_metrics =
        build_prompt_line_metrics_from_attributions(&current_attributions);
    apply_prompt_line_metrics_to_prompts(&mut current_prompts, &prompt_line_metrics);

    // Track which files actually exist in each rebased commit.
    let mut existing_files: HashSet<String> = current_file_contents
        .iter()
        .filter_map(|(file, content)| {
            if content.is_empty() {
                None
            } else {
                Some(file.clone())
            }
        })
        .collect();

    let mut current_authorship_log = build_authorship_log_from_state(
        current_va.base_commit(),
        &current_prompts,
        &current_attributions,
        &existing_files,
    );
    let rebase_ts = current_va.timestamp();

    let commit_tree_pairs = build_first_parent_tree_pairs(repo, &commits_to_process)?;
    let mut changed_contents_by_commit = collect_changed_file_contents_for_commit_pairs(
        repo,
        &commit_tree_pairs,
        &pathspecs_lookup,
        &pathspecs,
    )?;
    let mut pending_note_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_process.len());
    let mut pending_note_debug: Vec<(String, usize)> = Vec::with_capacity(commits_to_process.len());
    let mut original_note_content_by_new_commit: HashMap<String, String> = HashMap::new();
    let mut original_note_content_loaded = false;

    // Step 3: Process each new commit in order (oldest to newest)
    for (idx, new_commit) in commits_to_process.iter().enumerate() {
        debug_log(&format!(
            "Processing commit {}/{}: {}",
            idx + 1,
            commits_to_process.len(),
            new_commit
        ));

        let (changed_files_in_commit, new_content_for_changed_files) = changed_contents_by_commit
            .remove(new_commit)
            .unwrap_or_else(|| (HashSet::new(), HashMap::new()));

        // Only transform attributions for files that actually changed.
        if !changed_files_in_commit.is_empty() {
            let mut previous_line_attrs_by_file: HashMap<
                String,
                Vec<crate::authorship::attribution_tracker::LineAttribution>,
            > = HashMap::new();
            for file_path in &changed_files_in_commit {
                if let Some((_, line_attrs)) = current_attributions.get(file_path) {
                    previous_line_attrs_by_file.insert(file_path.clone(), line_attrs.clone());
                }
            }

            // Update file existence from this commit's deltas before transformation.
            // Empty content means the file is absent at this commit (deleted or missing).
            for (file_path, content) in &new_content_for_changed_files {
                if content.is_empty() {
                    existing_files.remove(file_path);
                } else {
                    existing_files.insert(file_path.clone());
                }
            }

            transform_changed_files_to_final_state(
                &mut current_attributions,
                &mut current_file_contents,
                new_content_for_changed_files,
                Some(&original_head_state_va),
                Some(&original_head_line_to_author),
                rebase_ts,
            )?;
            for line_attrs in previous_line_attrs_by_file.values() {
                subtract_prompt_line_metrics_for_line_attributions(
                    &mut prompt_line_metrics,
                    line_attrs,
                );
            }
            for file_path in &changed_files_in_commit {
                if let Some((_, line_attrs)) = current_attributions.get(file_path) {
                    add_prompt_line_metrics_for_line_attributions(
                        &mut prompt_line_metrics,
                        line_attrs,
                    );
                }
            }
            apply_prompt_line_metrics_to_prompts(&mut current_prompts, &prompt_line_metrics);

            // Update only files touched by this commit.
            for file_path in &changed_files_in_commit {
                upsert_file_attestation(
                    &mut current_authorship_log,
                    file_path,
                    current_attributions
                        .get(file_path)
                        .map(|(_, line_attrs)| line_attrs.as_slice())
                        .unwrap_or(&[]),
                    existing_files.contains(file_path),
                );
            }
        }

        // Ensure stale deleted files are never emitted.
        current_authorship_log
            .attestations
            .retain(|attestation| existing_files.contains(&attestation.file_path));

        current_authorship_log.metadata.base_commit_sha = new_commit.clone();
        current_authorship_log.metadata.prompts = flatten_prompts_for_metadata(&current_prompts);

        let computed_note_has_payload = !current_authorship_log.attestations.is_empty()
            || !current_authorship_log.metadata.prompts.is_empty();
        let authorship_json = if computed_note_has_payload {
            Some(current_authorship_log.serialize_to_string().map_err(|_| {
                GitAiError::Generic("Failed to serialize authorship log".to_string())
            })?)
        } else {
            if !original_note_content_loaded {
                original_note_content_by_new_commit =
                    load_note_contents_for_commit_pairs(repo, &commit_pairs_to_process)?;
                original_note_content_loaded = true;
            }
            original_note_content_by_new_commit
                .get(new_commit)
                .map(|raw_note| remap_note_content_for_target_commit(raw_note, new_commit))
        };
        if let Some(authorship_json) = authorship_json {
            pending_note_entries.push((new_commit.clone(), authorship_json));
            pending_note_debug.push((
                new_commit.clone(),
                current_authorship_log.attestations.len(),
            ));
        }
    }

    if !pending_note_entries.is_empty() {
        crate::git::refs::notes_add_batch(repo, &pending_note_entries)?;
    }

    for (commit_sha, file_count) in pending_note_debug {
        debug_log(&format!(
            "Saved authorship log for commit {} ({} files)",
            commit_sha, file_count
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

    let commit_pairs: Vec<(String, String)> = source_commits
        .iter()
        .zip(new_commits.iter())
        .map(|(source_commit, new_commit)| (source_commit.clone(), new_commit.clone()))
        .collect();
    let source_commits_for_pairs: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _new_commit)| source_commit.clone())
        .collect();

    // Step 1: Extract pathspecs from all source commits
    let pathspecs = get_pathspecs_from_commits(repo, source_commits)?;
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, source_commits, &pathspecs)?;

    if pathspecs.is_empty() {
        let source_note_contents = load_note_contents_for_commits(repo, &source_commits_for_pairs)?;
        let remapped_count =
            remap_notes_for_commit_pairs(repo, &commit_pairs, &source_note_contents)?;
        if remapped_count > 0 {
            debug_log(&format!(
                "Remapped {} metadata-only authorship notes for cherry-picked commits",
                remapped_count
            ));
        } else {
            debug_log("No files modified in source commits");
        }
        return Ok(());
    }
    let pathspecs_lookup: HashSet<&str> = pathspecs.iter().map(String::as_str).collect();
    let mut source_note_content_by_new_commit: HashMap<String, String> = HashMap::new();
    let mut source_note_content_loaded = false;

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
            if let Some(char_attrs) = current_va.get_char_attributions(&file)
                && let Some(line_attrs) = current_va.get_line_attributions(&file)
            {
                attrs.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
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

        // Apply changes from this commit's diff using one batched blob read.
        let (_changed_files, new_content_for_changed_files) =
            collect_changed_file_contents_from_diff(repo, &diff, &pathspecs_lookup)?;
        new_content_state.extend(new_content_for_changed_files);

        // Transform attributions based on the new content state
        // Pass source_head state to restore attributions for content that existed before cherry-pick
        current_va = transform_attributions_to_final_state(
            &current_va,
            new_content_state,
            Some(&source_head_state_va),
        )?;

        // Convert to AuthorshipLog, but filter to only files that exist in this commit
        let mut authorship_log = current_va.to_authorship_log()?;

        // Filter out attestations for files that don't exist in this commit (empty files)
        authorship_log.attestations.retain(|attestation| {
            if let Some(content) = current_va.get_file_content(&attestation.file_path) {
                !content.is_empty()
            } else {
                false
            }
        });

        authorship_log.metadata.base_commit_sha = new_commit.clone();

        // Save computed note when it has payload; otherwise preserve original metadata-only notes.
        let computed_note_has_payload =
            !authorship_log.attestations.is_empty() || !authorship_log.metadata.prompts.is_empty();
        let authorship_json = if computed_note_has_payload {
            authorship_log.serialize_to_string().map_err(|_| {
                GitAiError::Generic("Failed to serialize authorship log".to_string())
            })?
        } else {
            if !source_note_content_loaded {
                source_note_content_by_new_commit =
                    load_note_contents_for_commit_pairs(repo, &commit_pairs)?;
                source_note_content_loaded = true;
            }
            if let Some(raw_note) = source_note_content_by_new_commit.get(new_commit) {
                remap_note_content_for_target_commit(raw_note, new_commit)
            } else {
                authorship_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize authorship log".to_string())
                })?
            }
        };

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

fn is_zero_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

fn is_blob_mode(mode: &str) -> bool {
    mode.starts_with("100") || mode == "120000"
}

fn collect_changed_file_contents_from_diff(
    repo: &Repository,
    diff: &crate::git::diff_tree_to_tree::Diff,
    pathspecs_lookup: &HashSet<&str>,
) -> Result<(HashSet<String>, HashMap<String, String>), GitAiError> {
    let mut changed_files = HashSet::new();
    let mut file_to_blob_oid: Vec<(String, Option<String>)> = Vec::new();
    let mut blob_oids = HashSet::new();

    for delta in diff.deltas() {
        let file_path = delta
            .new_file()
            .path()
            .or(delta.old_file().path())
            .ok_or_else(|| GitAiError::Generic("File path not available".to_string()))?;
        let file_path_str = file_path.to_string_lossy().to_string();

        // Only process files we're tracking.
        if !pathspecs_lookup.contains(file_path_str.as_str()) {
            continue;
        }

        changed_files.insert(file_path_str.clone());

        let new_file = delta.new_file();
        let new_blob_oid = new_file.id();
        // Keep behavior aligned with the old tree+find_blob path:
        // only regular file/symlink blobs are materialized.
        if is_zero_oid(new_blob_oid) || !is_blob_mode(new_file.mode()) {
            file_to_blob_oid.push((file_path_str, None));
            continue;
        }

        let oid = new_blob_oid.to_string();
        blob_oids.insert(oid.clone());
        file_to_blob_oid.push((file_path_str, Some(oid)));
    }

    let mut blob_oid_list: Vec<String> = blob_oids.into_iter().collect();
    blob_oid_list.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oid_list)?;

    let mut file_contents = HashMap::new();
    for (file_path, blob_oid) in file_to_blob_oid {
        let content = blob_oid
            .as_ref()
            .and_then(|oid| blob_contents.get(oid).cloned())
            .unwrap_or_default();
        file_contents.insert(file_path, content);
    }

    Ok((changed_files, file_contents))
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

fn get_empty_tree_oid(repo: &Repository) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push("--empty-tree".to_string());
    let output = exec_git(&args)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn load_commit_metadata_batch(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, CommitObjectMetadata>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_commits = Vec::new();
    let mut seen = HashSet::new();
    for commit_sha in commit_shas {
        if seen.insert(commit_sha.as_str()) {
            unique_commits.push(commit_sha.clone());
        }
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = unique_commits.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let data = output.stdout;

    let mut metadata_by_commit = HashMap::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };
        let header = std::str::from_utf8(&data[pos..header_end])?;
        let mut parts = header.split_whitespace();
        let oid = match parts.next() {
            Some(v) => v.to_string(),
            None => {
                pos = header_end + 1;
                continue;
            }
        };
        let object_type = parts.next().unwrap_or_default();
        if object_type == "missing" {
            pos = header_end + 1;
            continue;
        }
        let size: usize = parts
            .next()
            .ok_or_else(|| {
                GitAiError::Generic("Malformed cat-file --batch header: missing size".to_string())
            })?
            .parse()
            .map_err(|e| {
                GitAiError::Generic(format!("Invalid cat-file --batch object size: {}", e))
            })?;

        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            return Err(GitAiError::Generic(
                "Malformed cat-file --batch output: truncated commit object".to_string(),
            ));
        }

        if object_type == "commit" {
            let content = std::str::from_utf8(&data[content_start..content_end])?;
            let mut tree_oid = String::new();
            let mut first_parent = None;

            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("tree ") {
                    tree_oid = rest.trim().to_string();
                } else if first_parent.is_none()
                    && let Some(rest) = line.strip_prefix("parent ")
                {
                    first_parent = Some(rest.trim().to_string());
                }
                if !tree_oid.is_empty() && first_parent.is_some() {
                    break;
                }
            }

            metadata_by_commit.insert(
                oid,
                CommitObjectMetadata {
                    tree_oid,
                    first_parent,
                },
            );
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    Ok(metadata_by_commit)
}

fn build_first_parent_tree_pairs(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<Vec<(String, String, String)>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(Vec::new());
    }

    let commit_metadata = load_commit_metadata_batch(repo, commit_shas)?;
    let mut parent_commits_to_load = Vec::new();
    let mut seen_parents = HashSet::new();

    for commit_sha in commit_shas {
        let Some(meta) = commit_metadata.get(commit_sha) else {
            continue;
        };
        if let Some(parent_sha) = &meta.first_parent
            && !commit_metadata.contains_key(parent_sha)
            && seen_parents.insert(parent_sha.as_str())
        {
            parent_commits_to_load.push(parent_sha.clone());
        }
    }

    let parent_metadata = load_commit_metadata_batch(repo, &parent_commits_to_load)?;
    let empty_tree_oid = get_empty_tree_oid(repo)?;

    let mut pairs = Vec::with_capacity(commit_shas.len());
    for commit_sha in commit_shas {
        let commit_meta = commit_metadata.get(commit_sha).ok_or_else(|| {
            GitAiError::Generic(format!("Missing commit metadata for {}", commit_sha))
        })?;
        if commit_meta.tree_oid.is_empty() {
            return Err(GitAiError::Generic(format!(
                "Missing tree oid for commit {}",
                commit_sha
            )));
        }

        let parent_tree = match &commit_meta.first_parent {
            Some(parent_sha) => {
                if let Some(parent_meta) = commit_metadata.get(parent_sha) {
                    parent_meta.tree_oid.clone()
                } else if let Some(parent_meta) = parent_metadata.get(parent_sha) {
                    parent_meta.tree_oid.clone()
                } else {
                    return Err(GitAiError::Generic(format!(
                        "Missing parent metadata for {}",
                        parent_sha
                    )));
                }
            }
            None => empty_tree_oid.clone(),
        };

        pairs.push((
            commit_sha.clone(),
            parent_tree,
            commit_meta.tree_oid.clone(),
        ));
    }

    Ok(pairs)
}

fn collect_changed_file_contents_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String, String)],
    pathspecs_lookup: &HashSet<&str>,
    pathspecs: &[String],
) -> Result<ChangedFileContentsByCommit, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--raw".to_string());
    args.push("-z".to_string());
    args.push("--no-abbrev".to_string());
    args.push("-r".to_string());
    if !pathspecs.is_empty() {
        args.push("--".to_string());
        args.extend(pathspecs.iter().cloned());
    }

    let mut stdin_lines = String::new();
    for (_commit_sha, parent_tree, commit_tree) in commit_pairs {
        stdin_lines.push_str(parent_tree);
        stdin_lines.push(' ');
        stdin_lines.push_str(commit_tree);
        stdin_lines.push('\n');
    }

    let output = exec_git_stdin(&args, stdin_lines.as_bytes())?;
    let data = output.stdout;

    let mut commit_deltas: Vec<CommitTrackedDelta> = Vec::with_capacity(commit_pairs.len());
    let mut all_blob_oids = HashSet::new();
    let mut pos = 0usize;

    for _ in commit_pairs {
        // Header format for tree-pair stdin:
        // "<old_tree_oid> <new_tree_oid>\n"
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => {
                return Err(GitAiError::Generic(
                    "Malformed diff-tree --stdin output: missing section header".to_string(),
                ));
            }
        };
        pos = header_end + 1;

        let mut delta = CommitTrackedDelta::default();

        while pos < data.len() && data[pos] == b':' {
            let meta_end = match data[pos..].iter().position(|&b| b == 0) {
                Some(idx) => pos + idx,
                None => {
                    return Err(GitAiError::Generic(
                        "Malformed diff-tree --stdin output: missing NUL after metadata"
                            .to_string(),
                    ));
                }
            };
            let metadata = std::str::from_utf8(&data[pos + 1..meta_end])?;
            let mut fields = metadata.split_whitespace();
            let _old_mode = fields.next().unwrap_or_default();
            let new_mode = fields.next().unwrap_or_default();
            let _old_oid = fields.next().unwrap_or_default();
            let new_oid = fields.next().unwrap_or_default();
            let status = fields.next().unwrap_or_default();
            let status_char = status.chars().next().unwrap_or('M');
            pos = meta_end + 1;

            let path_end = match data[pos..].iter().position(|&b| b == 0) {
                Some(idx) => pos + idx,
                None => {
                    return Err(GitAiError::Generic(
                        "Malformed diff-tree --stdin output: missing NUL after path".to_string(),
                    ));
                }
            };
            let file_path = std::str::from_utf8(&data[pos..path_end])?.to_string();
            pos = path_end + 1;

            if matches!(status_char, 'R' | 'C') {
                // Consume old path for rename/copy records.
                let old_path_end = match data[pos..].iter().position(|&b| b == 0) {
                    Some(idx) => pos + idx,
                    None => {
                        return Err(GitAiError::Generic(
                            "Malformed diff-tree --stdin output: missing NUL after old path"
                                .to_string(),
                        ));
                    }
                };
                pos = old_path_end + 1;
            }

            if !pathspecs_lookup.contains(file_path.as_str()) {
                continue;
            }

            delta.changed_files.insert(file_path.clone());
            let new_blob_oid = if is_zero_oid(new_oid) || !is_blob_mode(new_mode) {
                None
            } else {
                Some(new_oid.to_string())
            };
            if let Some(oid) = &new_blob_oid {
                all_blob_oids.insert(oid.clone());
            }
            delta.file_to_blob_oid.insert(file_path, new_blob_oid);
        }

        commit_deltas.push(delta);
    }

    let mut blob_oid_list: Vec<String> = all_blob_oids.into_iter().collect();
    blob_oid_list.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oid_list)?;

    let mut result = HashMap::new();
    for ((commit_sha, _parent_tree, _commit_tree), delta) in commit_pairs.iter().zip(commit_deltas)
    {
        let mut contents = HashMap::new();
        for (file_path, maybe_blob_oid) in delta.file_to_blob_oid {
            let content = maybe_blob_oid
                .as_ref()
                .and_then(|oid| blob_contents.get(oid).cloned())
                .unwrap_or_default();
            contents.insert(file_path, content);
        }
        result.insert(commit_sha.clone(), (delta.changed_files, contents));
    }

    Ok(result)
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
    if head == base {
        return Ok(Vec::new());
    }

    // Validate commit-ish values early so callers get a clear error.
    repository.find_commit(head.to_string())?;
    repository.find_commit(base.to_string())?;

    // Guard against pathological traversals when `base` is not actually an ancestor.
    // The old BFS fallback could walk huge histories in this case.
    let mut is_ancestor_args = repository.global_args_for_exec();
    is_ancestor_args.push("merge-base".to_string());
    is_ancestor_args.push("--is-ancestor".to_string());
    is_ancestor_args.push(base.to_string());
    is_ancestor_args.push(head.to_string());
    if exec_git(&is_ancestor_args).is_err() {
        return Err(GitAiError::Generic(format!(
            "Base commit {} is not an ancestor of {}",
            base, head
        )));
    }

    // Use git's native graph walker instead of per-parent subprocess traversal.
    // Return newest->oldest so existing callers can keep their current reverse() behavior.
    let mut args = repository.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--topo-order".to_string());
    args.push("--ancestry-path".to_string());
    args.push(format!("{}..{}", base, head));

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;
    let commits = stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

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
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--name-only".to_string());
    args.push("-r".to_string());
    args.push("-z".to_string());

    let stdin_data = commits.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let commit_markers: HashSet<&str> = commits.iter().map(String::as_str).collect();

    let mut pathspecs = HashSet::new();
    for token in output
        .stdout
        .split(|&b| b == 0)
        .filter(|token| !token.is_empty())
    {
        let value = String::from_utf8(token.to_vec())?;
        // diff-tree --stdin prefixes each commit section with the commit SHA.
        // Filter only the exact commit markers we asked diff-tree to emit.
        if commit_markers.contains(value.as_str()) {
            continue;
        }
        pathspecs.insert(value);
    }

    Ok(pathspecs.into_iter().collect())
}

fn load_note_contents_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let note_blob_oids = note_blob_oids_for_commits(repo, commit_shas)?;
    if note_blob_oids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut blob_oids: Vec<String> = note_blob_oids
        .values()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut note_contents = HashMap::new();
    for (commit_sha, blob_oid) in note_blob_oids {
        if let Some(content) = blob_contents.get(&blob_oid) {
            note_contents.insert(commit_sha, content.clone());
        }
    }

    Ok(note_contents)
}

fn load_note_contents_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(HashMap::new());
    }

    let source_commits: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _target_commit)| source_commit.clone())
        .collect();
    let source_note_contents = load_note_contents_for_commits(repo, &source_commits)?;

    let mut source_note_content_by_target_commit = HashMap::new();
    for (source_commit, target_commit) in commit_pairs {
        if let Some(note_content) = source_note_contents.get(source_commit) {
            source_note_content_by_target_commit
                .insert(target_commit.clone(), note_content.clone());
        }
    }

    Ok(source_note_content_by_target_commit)
}

fn remap_note_content_for_target_commit(note_content: &str, target_commit: &str) -> String {
    if let Some(remapped_note) = try_remap_base_commit_sha_field(note_content, target_commit) {
        return remapped_note;
    }

    if let Ok(mut authorship_log) = AuthorshipLog::deserialize_from_string(note_content) {
        authorship_log.metadata.base_commit_sha = target_commit.to_string();
        if let Ok(serialized) = authorship_log.serialize_to_string() {
            return serialized;
        }
    }
    note_content.to_string()
}

fn try_remap_base_commit_sha_field(note_content: &str, target_commit: &str) -> Option<String> {
    let field = "\"base_commit_sha\"";
    let field_pos = note_content.find(field)?;
    let bytes = note_content.as_bytes();

    let mut pos = field_pos + field.len();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return None;
    }
    pos += 1;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return None;
    }
    pos += 1;
    let value_start = pos;

    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 2;
            }
            b'"' => {
                let value_end = pos;
                let mut remapped = String::with_capacity(
                    note_content.len() - (value_end - value_start) + target_commit.len(),
                );
                remapped.push_str(&note_content[..value_start]);
                remapped.push_str(target_commit);
                remapped.push_str(&note_content[value_end..]);
                return Some(remapped);
            }
            _ => {
                pos += 1;
            }
        }
    }

    None
}

fn remap_notes_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    original_note_contents: &HashMap<String, String>,
) -> Result<usize, GitAiError> {
    if commit_pairs.is_empty() || original_note_contents.is_empty() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    for (original_commit, new_commit) in commit_pairs {
        if let Some(raw_note) = original_note_contents.get(original_commit) {
            entries.push((
                new_commit.clone(),
                remap_note_content_for_target_commit(raw_note, new_commit),
            ));
        }
    }

    if entries.is_empty() {
        return Ok(0);
    }

    let count = entries.len();
    crate::git::refs::notes_add_batch(repo, &entries)?;
    Ok(count)
}

fn build_metadata_only_authorship_log_from_source_notes(
    repo: &Repository,
    source_commits: &[String],
    target_commit_sha: &str,
) -> Result<Option<AuthorshipLog>, GitAiError> {
    let mut merged_prompts = BTreeMap::new();
    let mut prompt_totals: HashMap<String, (u32, u32)> = HashMap::new();
    let mut saw_any_note = false;

    for commit_sha in source_commits {
        let Ok(log) = get_reference_as_authorship_log_v3(repo, commit_sha) else {
            continue;
        };
        saw_any_note = true;

        for (prompt_id, prompt_record) in log.metadata.prompts {
            let entry = prompt_totals.entry(prompt_id.clone()).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(prompt_record.total_additions);
            entry.1 = entry.1.saturating_add(prompt_record.total_deletions);
            merged_prompts.insert(prompt_id, prompt_record);
        }
    }

    if !saw_any_note {
        return Ok(None);
    }

    for (prompt_id, (total_additions, total_deletions)) in prompt_totals {
        if let Some(prompt) = merged_prompts.get_mut(&prompt_id) {
            prompt.total_additions = total_additions;
            prompt.total_deletions = total_deletions;
        }
    }

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = target_commit_sha.to_string();
    authorship_log.metadata.prompts = merged_prompts;
    Ok(Some(authorship_log))
}

fn try_fast_path_rebase_note_remap(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
    commits_to_process_lookup: &HashSet<&str>,
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    let fast_path_start = std::time::Instant::now();
    if original_commits.len() != new_commits.len()
        || tracked_paths.is_empty()
        || commits_to_process_lookup.is_empty()
    {
        return Ok(false);
    }

    let commits_to_remap: Vec<(String, String)> = original_commits
        .iter()
        .zip(new_commits.iter())
        .filter(|(_original_commit, new_commit)| {
            commits_to_process_lookup.contains(new_commit.as_str())
        })
        .map(|(original_commit, new_commit)| (original_commit.clone(), new_commit.clone()))
        .collect();

    if commits_to_remap.is_empty() {
        return Ok(false);
    }

    let compare_start = std::time::Instant::now();
    if !tracked_paths_match_for_commit_pairs(repo, &commits_to_remap, tracked_paths)? {
        return Ok(false);
    }
    debug_performance_log(&format!(
        "Fast-path rebase note remap: compared tracked blobs for {} commit pairs in {}ms",
        commits_to_remap.len(),
        compare_start.elapsed().as_millis()
    ));

    let original_commits_for_batch: Vec<String> = commits_to_remap
        .iter()
        .map(|(original_commit, _new_commit)| original_commit.clone())
        .collect();
    let note_oid_lookup_start = std::time::Instant::now();
    let original_note_blob_oids = note_blob_oids_for_commits(repo, &original_commits_for_batch)?;
    debug_performance_log(&format!(
        "Fast-path rebase note remap: resolved {} note blob oids in {}ms",
        original_note_blob_oids.len(),
        note_oid_lookup_start.elapsed().as_millis()
    ));
    if original_note_blob_oids.len() != original_commits_for_batch.len() {
        return Ok(false);
    }

    let mut remapped_blob_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_remap.len());
    for (original_commit, new_commit) in commits_to_remap {
        let blob_oid = match original_note_blob_oids.get(&original_commit) {
            Some(oid) => oid.clone(),
            None => return Ok(false),
        };
        remapped_blob_entries.push((new_commit, blob_oid));
    }

    if remapped_blob_entries.is_empty() {
        return Ok(false);
    }
    let mut blob_oids: Vec<String> = remapped_blob_entries
        .iter()
        .map(|(_new_commit, blob_oid)| blob_oid.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut remapped_note_entries: Vec<(String, String)> =
        Vec::with_capacity(remapped_blob_entries.len());
    for (new_commit, blob_oid) in remapped_blob_entries {
        let Some(raw_note) = blob_contents.get(&blob_oid) else {
            return Ok(false);
        };
        remapped_note_entries.push((
            new_commit.clone(),
            remap_note_content_for_target_commit(raw_note, &new_commit),
        ));
    }

    let remapped_count = remapped_note_entries.len();
    let write_start = std::time::Instant::now();
    crate::git::refs::notes_add_batch(repo, &remapped_note_entries)?;
    debug_performance_log(&format!(
        "Fast-path rebase note remap: wrote {} remapped notes in {}ms",
        remapped_count,
        write_start.elapsed().as_millis()
    ));

    debug_log(&format!(
        "Fast-path remapped authorship logs for {} commits (blob-equivalent tracked files)",
        remapped_count
    ));
    debug_performance_log(&format!(
        "Fast-path rebase note remap complete in {}ms",
        fast_path_start.elapsed().as_millis()
    ));
    Ok(true)
}

fn tracked_paths_match_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(true);
    }

    let mut commits_to_load = Vec::with_capacity(commit_pairs.len() * 2);
    for (left_commit, right_commit) in commit_pairs {
        commits_to_load.push(left_commit.clone());
        commits_to_load.push(right_commit.clone());
    }
    let commit_metadata = load_commit_metadata_batch(repo, &commits_to_load)?;

    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--raw".to_string());
    args.push("-z".to_string());
    args.push("--no-abbrev".to_string());
    args.push("-r".to_string());
    if !tracked_paths.is_empty() {
        args.push("--".to_string());
        args.extend(tracked_paths.iter().cloned());
    }

    let mut stdin_lines = String::new();
    for (left_commit, right_commit) in commit_pairs {
        let left_tree = match commit_metadata.get(left_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.as_str(),
            _ => return Ok(false),
        };
        let right_tree = match commit_metadata.get(right_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.as_str(),
            _ => return Ok(false),
        };
        stdin_lines.push_str(left_tree);
        stdin_lines.push(' ');
        stdin_lines.push_str(right_tree);
        stdin_lines.push('\n');
    }

    let output = exec_git_stdin(&args, stdin_lines.as_bytes())?;
    let data = output.stdout;

    let mut pos = 0usize;
    for _ in commit_pairs {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => return Ok(false),
        };
        pos = header_end + 1;

        // Any delta line means tracked path blobs differ for this pair.
        if pos < data.len() && data[pos] == b':' {
            return Ok(false);
        }

        // Skip any blank separators between sections.
        while pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    // If the output still contains deltas, consider it non-matching to keep correctness.
    while pos < data.len() {
        if data[pos] == b':' {
            return Ok(false);
        }
        if data[pos] == b'\n' {
            pos += 1;
            continue;
        }
        if let Some(next_nl) = data[pos..].iter().position(|&b| b == b'\n') {
            pos += next_nl + 1;
        } else {
            break;
        }
    }

    Ok(true)
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

fn flatten_prompts_for_metadata(
    prompts: &BTreeMap<String, BTreeMap<String, crate::authorship::authorship_log::PromptRecord>>,
) -> BTreeMap<String, crate::authorship::authorship_log::PromptRecord> {
    prompts
        .iter()
        .filter_map(|(prompt_id, commits)| {
            commits
                .values()
                .next()
                .map(|record| (prompt_id.clone(), record.clone()))
        })
        .collect()
}

fn build_file_attestation_from_line_attributions(
    file_path: &str,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Option<crate::authorship::authorship_log_serialization::FileAttestation> {
    let mut by_author: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for line_attr in line_attrs {
        if line_attr.author_id == crate::authorship::working_log::CheckpointKind::Human.to_str() {
            continue;
        }
        by_author
            .entry(line_attr.author_id.clone())
            .or_default()
            .push((line_attr.start_line, line_attr.end_line));
    }

    if by_author.is_empty() {
        return None;
    }

    let mut file_attestation =
        crate::authorship::authorship_log_serialization::FileAttestation::new(
            file_path.to_string(),
        );

    for (author_id, mut ranges) in by_author {
        if ranges.is_empty() {
            continue;
        }
        ranges.sort_by_key(|(start, end)| (*start, *end));

        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some((_, last_end)) => {
                    if start <= last_end.saturating_add(1) {
                        *last_end = (*last_end).max(end);
                    } else {
                        merged.push((start, end));
                    }
                }
                None => merged.push((start, end)),
            }
        }

        let line_ranges = merged
            .into_iter()
            .map(|(start, end)| {
                if start == end {
                    crate::authorship::authorship_log::LineRange::Single(start)
                } else {
                    crate::authorship::authorship_log::LineRange::Range(start, end)
                }
            })
            .collect::<Vec<_>>();

        if !line_ranges.is_empty() {
            file_attestation.add_entry(
                crate::authorship::authorship_log_serialization::AttestationEntry::new(
                    author_id,
                    line_ranges,
                ),
            );
        }
    }

    if file_attestation.entries.is_empty() {
        None
    } else {
        Some(file_attestation)
    }
}

fn upsert_file_attestation(
    authorship_log: &mut AuthorshipLog,
    file_path: &str,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
    file_exists: bool,
) {
    authorship_log
        .attestations
        .retain(|attestation| attestation.file_path != file_path);
    if !file_exists {
        return;
    }
    if let Some(file_attestation) =
        build_file_attestation_from_line_attributions(file_path, line_attrs)
    {
        authorship_log.attestations.push(file_attestation);
    }
}

fn build_authorship_log_from_state(
    base_commit_sha: &str,
    prompts: &BTreeMap<String, BTreeMap<String, crate::authorship::authorship_log::PromptRecord>>,
    attributions: &HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    >,
    existing_files: &HashSet<String>,
) -> AuthorshipLog {
    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = base_commit_sha.to_string();
    authorship_log.metadata.prompts = flatten_prompts_for_metadata(prompts);

    for (file_path, (_, line_attrs)) in attributions {
        if !existing_files.contains(file_path) {
            continue;
        }
        if let Some(file_attestation) =
            build_file_attestation_from_line_attributions(file_path, line_attrs)
        {
            authorship_log.attestations.push(file_attestation);
        }
    }

    authorship_log
}

fn build_prompt_line_metrics_from_attributions(
    attributions: &HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    >,
) -> HashMap<String, PromptLineMetrics> {
    let mut metrics = HashMap::new();
    for (_char_attrs, line_attrs) in attributions.values() {
        add_prompt_line_metrics_for_line_attributions(&mut metrics, line_attrs);
    }
    metrics
}

fn add_prompt_line_metrics_for_line_attributions(
    metrics: &mut HashMap<String, PromptLineMetrics>,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) {
    for line_attr in line_attrs {
        let line_count = line_attr
            .end_line
            .saturating_sub(line_attr.start_line)
            .saturating_add(1);
        if line_attr.author_id != crate::authorship::working_log::CheckpointKind::Human.to_str() {
            let entry = metrics.entry(line_attr.author_id.clone()).or_default();
            entry.accepted_lines = entry.accepted_lines.saturating_add(line_count);
        }
        if let Some(overrode_id) = &line_attr.overrode {
            let entry = metrics.entry(overrode_id.clone()).or_default();
            entry.overridden_lines = entry.overridden_lines.saturating_add(line_count);
        }
    }
}

fn subtract_prompt_line_metrics_for_line_attributions(
    metrics: &mut HashMap<String, PromptLineMetrics>,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) {
    for line_attr in line_attrs {
        let line_count = line_attr
            .end_line
            .saturating_sub(line_attr.start_line)
            .saturating_add(1);
        if line_attr.author_id != crate::authorship::working_log::CheckpointKind::Human.to_str()
            && let Some(entry) = metrics.get_mut(&line_attr.author_id)
        {
            entry.accepted_lines = entry.accepted_lines.saturating_sub(line_count);
        }
        if let Some(overrode_id) = &line_attr.overrode
            && let Some(entry) = metrics.get_mut(overrode_id)
        {
            entry.overridden_lines = entry.overridden_lines.saturating_sub(line_count);
        }
    }
}

fn apply_prompt_line_metrics_to_prompts(
    prompts: &mut BTreeMap<
        String,
        BTreeMap<String, crate::authorship::authorship_log::PromptRecord>,
    >,
    metrics: &HashMap<String, PromptLineMetrics>,
) {
    for (prompt_id, commits) in prompts {
        let prompt_metrics = metrics.get(prompt_id).copied().unwrap_or_default();
        for record in commits.values_mut() {
            record.accepted_lines = prompt_metrics.accepted_lines;
            record.overriden_lines = prompt_metrics.overridden_lines;
        }
    }
}

fn build_original_head_line_author_maps(
    original_head_state: &crate::authorship::virtual_attribution::VirtualAttributions,
) -> HashMap<String, HashMap<String, String>> {
    let mut by_file: HashMap<String, HashMap<String, String>> = HashMap::new();

    for file_path in original_head_state.files() {
        let Some(original_content) = original_head_state.get_file_content(&file_path) else {
            continue;
        };
        let Some(original_line_attrs) = original_head_state.get_line_attributions(&file_path)
        else {
            continue;
        };
        if original_line_attrs.is_empty() {
            continue;
        }

        let original_lines: Vec<&str> = original_content.lines().collect();
        let mut line_to_author: HashMap<String, String> = HashMap::new();
        for line_attr in original_line_attrs {
            if line_attr.author_id == crate::authorship::working_log::CheckpointKind::Human.to_str()
            {
                continue;
            }
            for line_num in line_attr.start_line..=line_attr.end_line {
                let line_idx = (line_num as usize).saturating_sub(1);
                if line_idx < original_lines.len() {
                    line_to_author.insert(
                        original_lines[line_idx].to_string(),
                        line_attr.author_id.clone(),
                    );
                }
            }
        }

        if !line_to_author.is_empty() {
            by_file.insert(file_path, line_to_author);
        }
    }

    by_file
}

fn content_has_intersection_with_author_map(
    content: &str,
    line_to_author: &HashMap<String, String>,
) -> bool {
    content
        .lines()
        .any(|line| line_to_author.contains_key(line))
}

fn transform_changed_files_to_final_state(
    attributions: &mut HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    >,
    file_contents: &mut HashMap<String, String>,
    final_state: HashMap<String, String>,
    original_head_state: Option<&crate::authorship::virtual_attribution::VirtualAttributions>,
    original_line_to_author_maps: Option<&HashMap<String, HashMap<String, String>>>,
    ts: u128,
) -> Result<(), GitAiError> {
    use crate::authorship::attribution_tracker::AttributionTracker;

    let tracker = AttributionTracker::new();

    for (file_path, final_content) in final_state {
        // Keep previous state for missing/deleted files so a later reappearance can still
        // inherit older attributions.
        if final_content.is_empty() {
            continue;
        }

        let source_attrs = attributions
            .get(&file_path)
            .map(|(char_attrs, _)| char_attrs.as_slice());
        let source_content = file_contents.get(&file_path).map(String::as_str);
        let dummy_author = "__DUMMY__";
        let source_has_non_human = source_attrs.as_ref().is_some_and(|attrs| {
            attrs.iter().any(|attr| {
                attr.author_id != crate::authorship::working_log::CheckpointKind::Human.to_str()
            })
        });
        let original_file_has_non_human = original_line_to_author_maps
            .and_then(|maps| maps.get(&file_path))
            .is_some_and(|map| !map.is_empty());

        let mut transformed_attrs = if !source_has_non_human && !original_file_has_non_human {
            Vec::new()
        } else if let (Some(attrs), Some(content)) = (source_attrs, source_content) {
            tracker.update_attributions(content, &final_content, attrs, dummy_author, ts)?
        } else {
            Vec::new()
        };

        // Restore known attributions when the line content clearly maps back to original_head.
        if let Some(original_state) = original_head_state
            && let Some(original_content) = original_state.get_file_content(&file_path)
        {
            if original_content == &final_content {
                if let Some(original_attrs) = original_state.get_char_attributions(&file_path) {
                    transformed_attrs = original_attrs.clone();
                }
            } else if transformed_attrs
                .iter()
                .any(|attr| attr.author_id == dummy_author)
                && let Some(original_line_to_author) =
                    original_line_to_author_maps.and_then(|maps| maps.get(&file_path))
                && content_has_intersection_with_author_map(&final_content, original_line_to_author)
            {
                let final_lines: Vec<&str> = final_content.lines().collect();
                let line_count = final_lines.len();
                let temp_line_attrs =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions(
                        &transformed_attrs,
                        &final_content,
                    );

                let mut dummy_diff = vec![0i32; line_count + 2];
                for la in &temp_line_attrs {
                    if la.author_id != dummy_author {
                        continue;
                    }
                    let start = (la.start_line as usize).max(1).min(line_count);
                    let end = (la.end_line as usize).max(1).min(line_count);
                    if start > end {
                        continue;
                    }
                    dummy_diff[start] += 1;
                    dummy_diff[end + 1] -= 1;
                }

                let mut has_dummy_line = vec![false; line_count + 1]; // 1-indexed
                let mut running = 0i32;
                for line in 1..=line_count {
                    running += dummy_diff[line];
                    has_dummy_line[line] = running > 0;
                }

                let mut line_start_chars = Vec::with_capacity(line_count);
                let mut char_pos = 0usize;
                for line in &final_lines {
                    line_start_chars.push(char_pos);
                    char_pos += line.len() + 1;
                }

                for (line_idx, line_content) in final_lines.iter().enumerate() {
                    let line_num = (line_idx + 1) as u32;
                    if !has_dummy_line[line_num as usize] {
                        continue;
                    }
                    if let Some(original_author) = original_line_to_author.get(*line_content) {
                        let line_start_char = line_start_chars[line_idx];
                        let line_end_char = line_start_char + line_content.len();
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

        transformed_attrs.retain(|attr| attr.author_id != dummy_author);

        let line_attrs = crate::authorship::attribution_tracker::attributions_to_line_attributions(
            &transformed_attrs,
            &final_content,
        );

        attributions.insert(file_path.clone(), (transformed_attrs, line_attrs));
        file_contents.insert(file_path, final_content);
    }

    Ok(())
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

    // Start from the current state so unchanged files stay tracked across commits.
    // This is required for cases where a file changes in commit N, is untouched in N+1,
    // and changes again later in the rewritten sequence.
    let mut attributions = HashMap::new();
    let mut file_contents = HashMap::new();
    for file in source_va.files() {
        if let Some(content) = source_va.get_file_content(&file) {
            file_contents.insert(file.clone(), content.clone());
        }
        if let Some(char_attrs) = source_va.get_char_attributions(&file)
            && let Some(line_attrs) = source_va.get_line_attributions(&file)
        {
            attributions.insert(file, (char_attrs.clone(), line_attrs.clone()));
        }
    }

    // Process each file in the final state
    for (file_path, final_content) in final_state {
        // Skip empty files (they don't exist in this commit yet)
        // Keep the source attributions for when the file appears later
        if final_content.is_empty() {
            continue;
        }

        // Get source attributions and content
        let source_attrs = source_va.get_char_attributions(&file_path);
        let source_content = source_va.get_file_content(&file_path);

        // Transform to final state
        let mut transformed_attrs =
            if let (Some(attrs), Some(content)) = (source_attrs, source_content) {
                // Use a dummy author for new insertions
                let dummy_author = "__DUMMY__";

                // Keep all attributions initially (including dummy ones)
                tracker.update_attributions(content, &final_content, attrs, dummy_author, ts)?
            } else {
                Vec::new()
            };

        // Try to restore attributions from original_head_state using line-content matching
        // This handles commit splitting where content from original_head gets re-applied
        if let Some(original_state) = original_head_state
            && let Some(original_content) = original_state.get_file_content(&file_path)
        {
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

                if let Some(original_line_attrs) = original_state.get_line_attributions(&file_path)
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
                let line_count = final_lines.len();

                // Convert char attributions to line attributions to process line by line
                let temp_line_attrs =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions(
                        &transformed_attrs,
                        &final_content,
                    );

                // Build a line-level bitmap for dummy-attributed lines in O(attrs + lines).
                let mut dummy_diff = vec![0i32; line_count + 2];
                for la in &temp_line_attrs {
                    if la.author_id != dummy_author {
                        continue;
                    }
                    let start = (la.start_line as usize).max(1).min(line_count);
                    let end = (la.end_line as usize).max(1).min(line_count);
                    if start > end {
                        continue;
                    }
                    dummy_diff[start] += 1;
                    dummy_diff[end + 1] -= 1;
                }
                let mut has_dummy_line = vec![false; line_count + 1]; // 1-indexed
                let mut running = 0i32;
                for line in 1..=line_count {
                    running += dummy_diff[line];
                    has_dummy_line[line] = running > 0;
                }

                // Precompute per-line char starts once to avoid O(n^2) prefix sums.
                let mut line_start_chars = Vec::with_capacity(line_count);
                let mut char_pos = 0usize;
                for line in &final_lines {
                    line_start_chars.push(char_pos);
                    char_pos += line.len() + 1; // +1 for newline
                }

                // For each line with dummy attribution, try to restore from original
                for (line_idx, line_content) in final_lines.iter().enumerate() {
                    // Check if this line has a dummy attribution
                    let line_num = (line_idx + 1) as u32; // LineAttribution is 1-indexed
                    let has_dummy = has_dummy_line[line_num as usize];

                    if has_dummy {
                        // Try to find this line content in original state
                        if let Some(original_author) = original_line_to_author.get(*line_content) {
                            // Update all char attributions on this line
                            // Find the char range for this line
                            let line_start_char = line_start_chars[line_idx];
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

        // Now filter out any remaining dummy attributions
        let dummy_author = "__DUMMY__";
        transformed_attrs.retain(|attr| attr.author_id != dummy_author);

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

#[cfg(test)]
mod tests {
    use super::{
        collect_changed_file_contents_from_diff, get_pathspecs_from_commits,
        parse_cat_file_batch_output_with_oids, transform_attributions_to_final_state,
        try_fast_path_rebase_note_remap, walk_commits_to_base,
    };
    use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{
        AttestationEntry, AuthorshipLog, FileAttestation,
    };
    use crate::authorship::virtual_attribution::VirtualAttributions;
    use crate::git::refs::{notes_add, show_authorship_note};
    use crate::git::test_utils::TmpRepo;
    use std::collections::{HashMap, HashSet};

    fn write_minimal_authorship_note(
        repo: &TmpRepo,
        commit_sha: &str,
        file_path: &str,
        author_id: &str,
    ) {
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = commit_sha.to_string();
        let mut file = FileAttestation::new(file_path.to_string());
        file.add_entry(AttestationEntry::new(
            author_id.to_string(),
            vec![LineRange::Range(1, 1)],
        ));
        log.attestations.push(file);

        let note = log
            .serialize_to_string()
            .expect("serialize authorship note");
        notes_add(repo.gitai_repo(), commit_sha, &note).expect("write authorship note");
    }

    #[test]
    fn walk_commits_to_base_linear_history_is_bounded_and_ordered() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("f.txt", "a\n", true).expect("write base");
        repo.commit_with_message("base").expect("commit base");
        let base = repo.get_head_commit_sha().expect("base sha");

        repo.write_file("f.txt", "a\nb\n", true).expect("write mid");
        repo.commit_with_message("mid").expect("commit mid");
        let mid = repo.get_head_commit_sha().expect("mid sha");

        repo.write_file("f.txt", "a\nb\nc\n", true)
            .expect("write head");
        repo.commit_with_message("head").expect("commit head");
        let head = repo.get_head_commit_sha().expect("head sha");

        let commits =
            walk_commits_to_base(repo.gitai_repo(), &head, &base).expect("walk should succeed");

        // Newest -> oldest; callers reverse() for chronological order.
        assert_eq!(commits, vec![head, mid]);
    }

    #[test]
    fn walk_commits_to_base_merge_history_includes_both_sides_without_full_dag_walk() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base").expect("commit base");
        let base = repo.get_head_commit_sha().expect("base sha");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("side").expect("create side branch");
        repo.write_file("side.txt", "side\n", true)
            .expect("write side");
        repo.commit_with_message("side commit")
            .expect("commit side");
        let side_commit = repo.get_head_commit_sha().expect("side sha");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("main.txt", "main\n", true)
            .expect("write main");
        repo.commit_with_message("main commit")
            .expect("commit main");
        let main_commit = repo.get_head_commit_sha().expect("main sha");

        repo.git_command(&["merge", "--no-ff", "side", "-m", "merge side"])
            .expect("merge side");
        let merge_head = repo.get_head_commit_sha().expect("merge sha");

        let commits = walk_commits_to_base(repo.gitai_repo(), &merge_head, &base)
            .expect("walk should succeed");

        assert_eq!(commits.first(), Some(&merge_head));
        assert_eq!(commits.len(), 3);
        assert!(commits.contains(&main_commit));
        assert!(commits.contains(&side_commit));
        assert!(!commits.contains(&base));
    }

    #[test]
    fn walk_commits_to_base_rejects_non_ancestor_base() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("f.txt", "a\n", true).expect("write base");
        repo.commit_with_message("base").expect("commit base");

        repo.write_file("f.txt", "a\nb\n", true)
            .expect("write middle");
        repo.commit_with_message("middle").expect("commit middle");
        let middle = repo.get_head_commit_sha().expect("middle sha");

        repo.write_file("f.txt", "a\nb\nc\n", true)
            .expect("write top");
        repo.commit_with_message("top").expect("commit top");
        let top = repo.get_head_commit_sha().expect("top sha");

        let err = walk_commits_to_base(repo.gitai_repo(), &middle, &top).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("not an ancestor"),
            "unexpected error message: {}",
            msg
        );
    }

    #[test]
    fn get_pathspecs_from_commits_keeps_hex_filenames() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base file");
        repo.commit_with_message("base commit")
            .expect("commit base file");

        let hex_name = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        repo.write_file(hex_name, "x\n", true)
            .expect("write hex file");
        repo.commit_with_message("hex file commit")
            .expect("commit hex file");
        let commit_sha = repo.get_head_commit_sha().expect("head sha");

        let paths = get_pathspecs_from_commits(repo.gitai_repo(), &[commit_sha])
            .expect("collect pathspecs from commit");

        assert!(
            paths.iter().any(|p| p == hex_name),
            "hex filename should be retained in pathspecs: {:?}",
            paths
        );
    }

    #[test]
    fn collect_changed_file_contents_from_diff_handles_add_modify_delete_and_filtering() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("a.txt", "a1\n", true)
            .expect("write a base");
        repo.write_file("c.txt", "c1\n", true)
            .expect("write c base");
        repo.commit_with_message("base").expect("commit base");

        repo.write_file("a.txt", "a2\n", true).expect("modify a");
        repo.write_file("b.txt", "b1\n", true).expect("add b");
        repo.git_command(&["rm", "c.txt"]).expect("delete c");
        repo.commit_with_message("rewrite").expect("commit rewrite");

        let repo_ref = repo.gitai_repo();
        let head_sha = repo.get_head_commit_sha().expect("head sha");
        let head = repo_ref.find_commit(head_sha).expect("head commit");
        let parent = head.parent(0).expect("parent commit");
        let head_tree = head.tree().expect("head tree");
        let parent_tree = parent.tree().expect("parent tree");
        let diff = repo_ref
            .diff_tree_to_tree(Some(&parent_tree), Some(&head_tree), None, None)
            .expect("diff tree-to-tree");

        let tracked_all: HashSet<&str> = ["a.txt", "b.txt", "c.txt"].into_iter().collect();
        let (changed, contents) =
            collect_changed_file_contents_from_diff(repo_ref, &diff, &tracked_all)
                .expect("collect changed contents");

        assert_eq!(changed.len(), 3);
        assert!(changed.contains("a.txt"));
        assert!(changed.contains("b.txt"));
        assert!(changed.contains("c.txt"));
        assert_eq!(contents.get("a.txt").map(String::as_str), Some("a2\n"));
        assert_eq!(contents.get("b.txt").map(String::as_str), Some("b1\n"));
        assert_eq!(contents.get("c.txt").map(String::as_str), Some(""));

        let tracked_subset: HashSet<&str> = ["a.txt"].into_iter().collect();
        let (subset_changed, subset_contents) =
            collect_changed_file_contents_from_diff(repo_ref, &diff, &tracked_subset)
                .expect("collect subset");
        assert_eq!(subset_changed.len(), 1);
        assert!(subset_changed.contains("a.txt"));
        assert_eq!(subset_contents.len(), 1);
        assert_eq!(
            subset_contents.get("a.txt").map(String::as_str),
            Some("a2\n")
        );
    }

    #[test]
    fn parse_cat_file_batch_output_with_oids_parses_empty_and_multiline_blobs() {
        let data = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 6\nx\ny\nz\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb blob 0\n\n";
        let parsed =
            parse_cat_file_batch_output_with_oids(data).expect("parse cat-file batch output");

        assert_eq!(
            parsed
                .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .map(String::as_str),
            Some("x\ny\nz\n")
        );
        assert_eq!(
            parsed
                .get("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn parse_cat_file_batch_output_with_oids_errors_on_truncated_payload() {
        let truncated = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 5\nabc";
        let err = parse_cat_file_batch_output_with_oids(truncated).expect_err("should fail");
        assert!(
            err.to_string().contains("truncated"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn fast_path_rebase_note_remap_copies_logs_when_tracked_blobs_match() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("ai.txt", "base\n", true)
            .expect("write ai base");
        repo.commit_with_message("base").expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature")
            .expect("create feature branch");
        repo.write_file("ai.txt", "base\nfeature\n", true)
            .expect("write feature ai");
        repo.commit_with_message("feature ai commit")
            .expect("commit feature ai");
        let original_commit = repo.get_head_commit_sha().expect("feature sha");
        write_minimal_authorship_note(&repo, &original_commit, "ai.txt", "mock_ai");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("unrelated.txt", "main\n", true)
            .expect("write unrelated");
        repo.commit_with_message("main unrelated")
            .expect("commit unrelated");

        repo.git_command(&["cherry-pick", &original_commit])
            .expect("cherry-pick feature commit");
        let new_commit = repo.get_head_commit_sha().expect("new sha");

        let commits_to_process_lookup: HashSet<&str> = [new_commit.as_str()].into_iter().collect();
        let did_remap = try_fast_path_rebase_note_remap(
            repo.gitai_repo(),
            std::slice::from_ref(&original_commit),
            std::slice::from_ref(&new_commit),
            &commits_to_process_lookup,
            &["ai.txt".to_string()],
        )
        .expect("fast-path remap result");

        assert!(did_remap, "expected fast-path remap to trigger");

        let remapped_note_raw =
            show_authorship_note(repo.gitai_repo(), &new_commit).expect("new note content");
        let remapped =
            AuthorshipLog::deserialize_from_string(&remapped_note_raw).expect("parse new note");
        assert_eq!(remapped.metadata.base_commit_sha, new_commit);
        assert_eq!(remapped.attestations.len(), 1);
        assert_eq!(remapped.attestations[0].file_path, "ai.txt");
    }

    #[test]
    fn fast_path_rebase_note_remap_copies_multiple_commits_in_one_pass() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("ai.txt", "base\n", true)
            .expect("write ai base");
        repo.commit_with_message("base").expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature")
            .expect("create feature branch");

        let mut original_commits = Vec::new();
        for idx in 1..=2 {
            repo.write_file("ai.txt", &format!("base\nfeature {}\n", idx), true)
                .expect("write feature ai");
            repo.commit_with_message(&format!("feature ai commit {}", idx))
                .expect("commit feature ai");
            let original_commit = repo.get_head_commit_sha().expect("feature sha");
            write_minimal_authorship_note(&repo, &original_commit, "ai.txt", "mock_ai");
            original_commits.push(original_commit);
        }

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("unrelated.txt", "main\n", true)
            .expect("write unrelated");
        repo.commit_with_message("main unrelated")
            .expect("commit unrelated");

        let mut new_commits = Vec::new();
        for original_commit in &original_commits {
            repo.git_command(&["cherry-pick", original_commit])
                .expect("cherry-pick feature commit");
            new_commits.push(repo.get_head_commit_sha().expect("new sha"));
        }

        let commits_to_process_lookup: HashSet<&str> =
            new_commits.iter().map(String::as_str).collect();
        let did_remap = try_fast_path_rebase_note_remap(
            repo.gitai_repo(),
            &original_commits,
            &new_commits,
            &commits_to_process_lookup,
            &["ai.txt".to_string()],
        )
        .expect("fast-path remap result");

        assert!(did_remap, "expected fast-path remap to trigger");

        for new_commit in new_commits {
            let remapped_note_raw =
                show_authorship_note(repo.gitai_repo(), &new_commit).expect("new note content");
            let remapped =
                AuthorshipLog::deserialize_from_string(&remapped_note_raw).expect("parse new note");
            assert_eq!(remapped.metadata.base_commit_sha, new_commit);
            assert_eq!(remapped.attestations.len(), 1);
            assert_eq!(remapped.attestations[0].file_path, "ai.txt");
        }
    }

    #[test]
    fn fast_path_rebase_note_remap_declines_when_tracked_blobs_differ() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("ai.txt", "base\n", true)
            .expect("write ai base");
        repo.commit_with_message("base").expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature")
            .expect("create feature branch");
        repo.write_file("ai.txt", "base\nfeature\n", true)
            .expect("write feature ai");
        repo.commit_with_message("feature ai commit")
            .expect("commit feature ai");
        let original_commit = repo.get_head_commit_sha().expect("feature sha");
        write_minimal_authorship_note(&repo, &original_commit, "ai.txt", "mock_ai");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("ai.txt", "base\nmain-only\n", true)
            .expect("write divergent ai");
        repo.commit_with_message("main modifies ai")
            .expect("commit divergent ai");
        let new_commit = repo.get_head_commit_sha().expect("new sha");

        let commits_to_process_lookup: HashSet<&str> = [new_commit.as_str()].into_iter().collect();
        let did_remap = try_fast_path_rebase_note_remap(
            repo.gitai_repo(),
            std::slice::from_ref(&original_commit),
            std::slice::from_ref(&new_commit),
            &commits_to_process_lookup,
            &["ai.txt".to_string()],
        )
        .expect("fast-path remap result");

        assert!(!did_remap, "expected fast-path remap to decline");
    }

    #[test]
    fn transform_attributions_to_final_state_preserves_unchanged_files() {
        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("a.txt", "aaa\n", true).expect("write a");
        repo.write_file("b.txt", "bbb\n", true).expect("write b");
        repo.commit_with_message("base").expect("commit base");
        let base_sha = repo.get_head_commit_sha().expect("base sha");

        let mut attrs = HashMap::new();
        attrs.insert(
            "a.txt".to_string(),
            (
                vec![Attribution::new(0, 4, "ai-a".to_string(), 1)],
                vec![LineAttribution {
                    start_line: 1,
                    end_line: 1,
                    author_id: "ai-a".to_string(),
                    overrode: None,
                }],
            ),
        );
        attrs.insert(
            "b.txt".to_string(),
            (
                vec![Attribution::new(0, 4, "ai-b".to_string(), 1)],
                vec![LineAttribution {
                    start_line: 1,
                    end_line: 1,
                    author_id: "ai-b".to_string(),
                    overrode: None,
                }],
            ),
        );

        let mut file_contents = HashMap::new();
        file_contents.insert("a.txt".to_string(), "aaa\n".to_string());
        file_contents.insert("b.txt".to_string(), "bbb\n".to_string());

        let source_va =
            VirtualAttributions::new(repo.gitai_repo().clone(), base_sha, attrs, file_contents, 1);

        let mut final_state = HashMap::new();
        final_state.insert("a.txt".to_string(), "aaa!\n".to_string());

        let transformed = transform_attributions_to_final_state(&source_va, final_state, None)
            .expect("transform");

        assert_eq!(
            transformed
                .get_file_content("b.txt")
                .map(std::string::String::as_str),
            Some("bbb\n")
        );
        assert!(
            transformed.get_line_attributions("b.txt").is_some(),
            "unchanged file should retain attributions"
        );
    }
}
