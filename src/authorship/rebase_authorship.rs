use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::error::GitAiError;
use crate::git::authorship_traversal::{
    commits_have_authorship_notes, load_ai_touched_files_for_commits,
};
use crate::git::refs::{get_reference_as_authorship_log_v3, note_blob_oids_for_commits};
use crate::git::repository::{CommitRange, Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::{debug_log, debug_performance_log};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Default)]
struct PromptLineMetrics {
    accepted_lines: u32,
    overridden_lines: u32,
}

/// Pre-loaded note data for all commits involved in a rebase.
/// Eliminates redundant git subprocess calls by reading everything once upfront.
struct RebaseNoteCache {
    /// Which new commits already have authorship notes (to skip reprocessing)
    new_commits_with_notes: HashSet<String>,
    /// Note blob OIDs for original commits (commit_sha → blob_oid)
    original_note_blob_oids: HashMap<String, String>,
    /// Parsed note contents for original commits (commit_sha → raw_content)
    original_note_contents: HashMap<String, String>,
    /// AI-touched file paths extracted from original commit notes
    ai_touched_files: HashSet<String>,
}

fn load_rebase_note_cache(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Result<RebaseNoteCache, GitAiError> {
    // Step 1: Get note blob OIDs for both original and new commits in one batch call.
    // We interleave them to make a single cat-file --batch-check call.
    let mut all_commits = Vec::with_capacity(original_commits.len() + new_commits.len());
    all_commits.extend(original_commits.iter().cloned());
    all_commits.extend(new_commits.iter().cloned());
    let all_note_oids = note_blob_oids_for_commits(repo, &all_commits)?;

    let mut original_note_blob_oids = HashMap::new();
    let mut new_commit_note_blob_oids: HashMap<String, String> = HashMap::new();

    for commit in original_commits {
        if let Some(oid) = all_note_oids.get(commit) {
            original_note_blob_oids.insert(commit.clone(), oid.clone());
        }
    }
    for commit in new_commits {
        if let Some(oid) = all_note_oids.get(commit) {
            new_commit_note_blob_oids.insert(commit.clone(), oid.clone());
        }
    }

    // Step 2: Read all note blob contents (original + new) in one batch call.
    let mut unique_blob_oids: Vec<String> = original_note_blob_oids
        .values()
        .chain(new_commit_note_blob_oids.values())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unique_blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &unique_blob_oids)?;

    // A new commit's note only counts as "already processed" when it has actual
    // attestations.  Empty notes (no attestations) arise when a post-commit hook
    // fires during `rebase --continue` for a human-resolved conflict commit —
    // in that case we must still run the slow-path rewrite to transfer attribution
    // for any AI lines that survived the merge.
    let mut new_commits_with_notes = HashSet::new();
    for (commit, blob_oid) in &new_commit_note_blob_oids {
        if let Some(content) = blob_contents.get(blob_oid) {
            if let Ok(log) = AuthorshipLog::deserialize_from_string(content) {
                if !log.attestations.is_empty() {
                    new_commits_with_notes.insert(commit.clone());
                }
            }
        }
    }

    let mut original_note_contents = HashMap::new();
    let mut ai_touched_files = HashSet::new();

    for (commit_sha, blob_oid) in &original_note_blob_oids {
        if let Some(content) = blob_contents.get(blob_oid) {
            original_note_contents.insert(commit_sha.clone(), content.clone());
            // Extract AI-touched file paths from this note
            crate::git::authorship_traversal::extract_file_paths_from_note_public(
                content,
                &mut ai_touched_files,
            );
        }
    }

    Ok(RebaseNoteCache {
        new_commits_with_notes,
        original_note_blob_oids,
        original_note_contents,
        ai_touched_files,
    })
}

#[derive(Debug, Default, Clone)]
struct CommitTrackedDelta {
    changed_files: HashSet<String>,
    file_to_blob_oid: HashMap<String, Option<String>>,
}

#[derive(Debug, Default, Clone)]
struct CommitObjectMetadata {
    tree_oid: String,
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
            let current_head = repo
                .head()
                .ok()
                .and_then(|head| head.target().ok())
                .map(|oid| oid.to_string());
            if current_head.as_deref() != Some(merge_squash.base_head.as_str()) {
                debug_log(&format!(
                    "Skipping merge --squash pre-commit prep because repo head already advanced past {}",
                    merge_squash.base_head
                ));
                return Ok(());
            }
            // --squash always fails if repo is not clean
            // this clears old working logs in the event you reset, make manual changes, reset, try again
            repo.storage
                .delete_working_log_for_base_commit(&merge_squash.base_head)?;
            if merge_squash.staged_file_blobs.is_empty() {
                debug_log(&format!(
                    "Skipping immediate merge --squash pre-commit prep for {} because no staged snapshot was captured; commit replay will reconstruct from the committed final state",
                    merge_squash.base_head
                ));
                return Ok(());
            }

            // Prepare INITIAL attributions from the squashed changes
            prepare_working_log_after_squash(
                repo,
                &merge_squash.source_head,
                &merge_squash.base_head,
                &merge_squash.staged_file_blobs,
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

            migrate_working_log_after_rebase(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.new_head,
            )?;

            debug_log(&format!(
                "✓ Rewrote authorship for {} rebased commits",
                rebase_complete.new_commits.len()
            ));
        }
        RewriteLogEvent::CherryPickComplete {
            cherry_pick_complete,
        } => {
            // Fix #955: fetch missing notes before attribution rewriting so that
            // daemon mode has the same remote-note resolution as wrapper mode.
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &cherry_pick_complete.source_commits,
            );
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

/// Migrate working log from the pre-rebase HEAD to the post-rebase HEAD.
/// Rebase rewrites commit SHAs, but working logs are keyed by SHA. Without this
/// migration, uncommitted attributions stored in the working log are orphaned on
/// the old SHA and silently lost when the developer eventually commits.
///
/// When only the old working log exists, the entire directory is renamed (preserving
/// INITIAL, checkpoints, and any other data). When both old and new directories
/// exist, only INITIAL attributions are merged into the new directory -- checkpoints
/// from the old directory are intentionally dropped because the new directory's
/// checkpoints already reflect the post-rebase state.
fn migrate_working_log_after_rebase(
    repo: &Repository,
    original_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    if original_head == new_head {
        return Ok(());
    }

    if !repo.storage.has_working_log(original_head) {
        return Ok(());
    }

    if !repo.storage.has_working_log(new_head) {
        repo.storage.rename_working_log(original_head, new_head)?;
    } else {
        let old_wl = repo.storage.working_log_for_base_commit(original_head)?;
        let initial = old_wl.read_initial_attributions();
        if !initial.files.is_empty() {
            let new_wl = repo.storage.working_log_for_base_commit(new_head)?;
            new_wl.write_initial(initial)?;
            debug_log(&format!(
                "Migrated INITIAL attributions from {} to {}",
                original_head, new_head
            ));
        } else {
            debug_log(&format!(
                "No INITIAL attributions to migrate from {} (dropping old working log)",
                original_head
            ));
        }
        repo.storage
            .delete_working_log_for_base_commit(original_head)?;
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
    staged_file_blobs: &HashMap<String, String>,
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

    // Step 3: Materialize the staged snapshot captured with the squash event.
    let mut blob_oids: Vec<String> = changed_files
        .iter()
        .filter_map(|file_path| staged_file_blobs.get(file_path).cloned())
        .collect();
    blob_oids.sort();
    blob_oids.dedup();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut staged_files = HashMap::new();
    for file_path in &changed_files {
        let Some(blob_oid) = staged_file_blobs.get(file_path) else {
            continue;
        };
        if let Some(content) = blob_contents.get(blob_oid) {
            staged_files.insert(file_path.clone(), content.clone());
        }
    }

    // Step 4: Merge VirtualAttributions, favoring target branch (HEAD)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, staged_files)?;

    // Step 5: Convert to INITIAL (everything is uncommitted in a squash).
    // This must stay independent of the live worktree because daemon replay may lag behind
    // later user edits.
    let initial_attributions = merged_va.to_initial_working_log_only();

    // Step 6: Write INITIAL file
    if !initial_attributions.files.is_empty() {
        let working_log = repo
            .storage
            .working_log_for_base_commit(target_branch_head_sha)?;
        let initial_file_contents =
            merged_va.snapshot_contents_for_files(initial_attributions.files.keys());
        working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_file_contents,
        )?;
    }

    Ok(())
}

pub fn prepare_working_log_after_squash_from_final_state(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    final_state: &HashMap<String, String>,
    _human_author: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    let changed_files = repo.diff_changed_files(source_head_sha, target_branch_head_sha)?;
    if changed_files.is_empty() {
        return Ok(());
    }

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

    let squash_files = changed_files
        .iter()
        .filter_map(|file_path| {
            final_state
                .get(file_path)
                .cloned()
                .map(|content| (file_path.clone(), content))
        })
        .collect::<HashMap<_, _>>();

    let merged_va = merge_attributions_favoring_first(target_va, source_va, squash_files)?;
    let initial_attributions = merged_va.to_initial_working_log_only();

    if !initial_attributions.files.is_empty() {
        let working_log = repo
            .storage
            .working_log_for_base_commit(target_branch_head_sha)?;
        let initial_file_contents =
            merged_va.snapshot_contents_for_files(initial_attributions.files.keys());
        working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_file_contents,
        )?;
    }

    Ok(())
}

/// Restore carried-over uncommitted authorship after an async head/base transition.
///
/// This uses only persisted working-log state from `old_head`, persisted state already present on
/// `new_head`, and the exact final file contents captured at command exit.
pub fn restore_working_log_carryover(
    repo: &Repository,
    old_head: &str,
    new_head: &str,
    final_state: HashMap<String, String>,
    human_author: Option<String>,
) -> Result<(), GitAiError> {
    if old_head.is_empty() || new_head.is_empty() || final_state.is_empty() {
        return Ok(());
    }

    let old_va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            old_head.to_string(),
            human_author,
        )?;
    restore_virtual_attribution_carryover(repo, new_head, old_va, final_state)
}

pub fn restore_virtual_attribution_carryover(
    repo: &Repository,
    new_head: &str,
    carried_va: crate::authorship::virtual_attribution::VirtualAttributions,
    final_state: HashMap<String, String>,
) -> Result<(), GitAiError> {
    if new_head.is_empty() || final_state.is_empty() || carried_va.attributions.is_empty() {
        return Ok(());
    }

    let new_va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            new_head.to_string(),
            None,
        )
        .unwrap_or_else(|_| {
            crate::authorship::virtual_attribution::VirtualAttributions::new(
                repo.clone(),
                new_head.to_string(),
                HashMap::new(),
                HashMap::new(),
                0,
            )
        });

    let merged_va = crate::authorship::virtual_attribution::merge_attributions_favoring_first(
        carried_va,
        new_va,
        final_state.clone(),
    )?;
    let initial_attributions = merged_va.to_initial_working_log_only();
    if initial_attributions.files.is_empty() && initial_attributions.prompts.is_empty() {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(new_head)?;
    working_log.write_initial_attributions_with_contents(
        initial_attributions.files,
        initial_attributions.prompts,
        final_state,
    )?;
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
    let target_branch_head = if merge_commit.parent_count()? == 1 {
        // For single-parent commits (squash merges), there's no ambiguity - use the only parent
        // This avoids issues in partial clones where parent_on_refname might fail
        merge_commit.parent(0)?
    } else {
        // For multi-parent commits, find the parent that's on the target branch
        merge_commit.parent_on_refname(merge_ref)?
    };
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

/// Reconstruct attribution state from existing authorship notes instead of running
/// expensive git blame operations. This reads notes from ALL original commits in batch
/// and merges their attributions to get the full state at original_head.
/// Cached version: uses pre-loaded note contents from RebaseNoteCache.
/// Returns: (attributions, file_contents, prompts) or None if reconstruction fails.
#[allow(clippy::type_complexity)]
fn try_reconstruct_attributions_from_notes_cached(
    repo: &Repository,
    original_head: &str,
    original_commits: &[String],
    pathspecs: &[String],
    _is_squash_rebase: bool,
    note_cache: &RebaseNoteCache,
    original_hunks: &HunksByCommitAndFile,
) -> Option<(
    HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    >,
    HashMap<String, String>,
    BTreeMap<String, BTreeMap<String, crate::authorship::authorship_log::PromptRecord>>,
)> {
    use crate::authorship::attribution_tracker::LineAttribution;
    use crate::authorship::authorship_log_serialization::AuthorshipLog;

    let pathspec_set: HashSet<&str> = pathspecs.iter().map(String::as_str).collect();
    let mut prompts: BTreeMap<
        String,
        BTreeMap<String, crate::authorship::authorship_log::PromptRecord>,
    > = BTreeMap::new();

    // Parse all notes and check if any exist.
    let mut parsed_logs: HashMap<String, AuthorshipLog> = HashMap::new();
    for commit in original_commits
        .iter()
        .chain(std::iter::once(&original_head.to_string()))
    {
        if let Some(content) = note_cache.original_note_contents.get(commit.as_str())
            && let Ok(log) = AuthorshipLog::deserialize_from_string(content)
        {
            parsed_logs.insert(commit.clone(), log);
        }
    }

    if parsed_logs.is_empty() {
        return None;
    }

    // Hunk-based replay: process original commits in order, accumulating
    // attributions by applying each commit's hunks (to shift line numbers)
    // then overlaying that commit's note (to stamp new AI-authored lines).
    let mut file_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();

    // Process commits in chronological order (original_commits already ordered
    // oldest-first, with original_head as the tip).
    let all_commits_ordered: Vec<&str> = original_commits
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(original_head))
        .collect();
    // Deduplicate: original_head may already be in original_commits
    let mut seen_commits: HashSet<&str> = HashSet::new();
    let all_commits_ordered: Vec<&str> = all_commits_ordered
        .into_iter()
        .filter(|c| seen_commits.insert(c))
        .collect();

    for commit in &all_commits_ordered {
        // Step 1: Apply this commit's hunks to shift existing attributions.
        if let Some(file_hunks) = original_hunks.get(*commit) {
            for (file_path, hunks) in file_hunks {
                if !pathspec_set.contains(file_path.as_str()) {
                    continue;
                }
                if let Some(attrs) = file_attrs.get(file_path) {
                    let shifted = apply_hunks_to_line_attributions(attrs, hunks);
                    file_attrs.insert(file_path.clone(), shifted);
                }
            }
        }

        // Step 2: Overlay this commit's note attributions.
        if let Some(log) = parsed_logs.get(*commit) {
            for file_attestation in &log.attestations {
                let file_path = &file_attestation.file_path;
                if !pathspec_set.contains(file_path.as_str()) {
                    continue;
                }
                let attrs = file_attrs.entry(file_path.clone()).or_default();
                for entry in &file_attestation.entries {
                    for range in &entry.line_ranges {
                        let (start, end) = match range {
                            crate::authorship::authorship_log::LineRange::Single(l) => (*l, *l),
                            crate::authorship::authorship_log::LineRange::Range(s, e) => (*s, *e),
                        };
                        // Remove any existing attributions that overlap this range,
                        // then insert the new one.
                        overlay_attribution(attrs, start, end, entry.hash.clone());
                    }
                }
            }

            // Collect prompts.
            for (prompt_id, prompt_record) in &log.metadata.prompts {
                prompts
                    .entry(prompt_id.clone())
                    .or_default()
                    .insert(commit.to_string(), prompt_record.clone());
            }
        }
    }

    if file_attrs.values().all(|v| v.is_empty()) {
        return None;
    }

    // Read file contents at HEAD — needed by the caller for the commit replay loop.
    let file_contents = batch_read_file_contents_at_commit(repo, original_head, pathspecs).ok()?;

    // Build return value.
    let mut attributions = HashMap::new();
    for (file_path, mut line_attrs) in file_attrs {
        if !line_attrs.is_empty() {
            line_attrs.sort_by_key(|a| a.start_line);
            attributions.insert(file_path, (Vec::new(), line_attrs));
        }
    }

    Some((attributions, file_contents, prompts))
}

/// Overlay a new attribution range onto an existing sorted attribution list.
/// Removes or splits any existing attributions that overlap the new range,
/// then inserts the new attribution.
fn overlay_attribution(
    attrs: &mut Vec<crate::authorship::attribution_tracker::LineAttribution>,
    start: u32,
    end: u32,
    author_id: String,
) {
    use crate::authorship::attribution_tracker::LineAttribution;

    // Remove overlapping entries, splitting partial overlaps.
    let mut i = 0;
    let mut to_insert_after: Vec<LineAttribution> = Vec::new();
    while i < attrs.len() {
        let a = &attrs[i];
        if a.end_line < start || a.start_line > end {
            // No overlap.
            i += 1;
            continue;
        }
        // Overlap detected — remove and potentially split.
        let removed = attrs.remove(i);
        if removed.start_line < start {
            // Left fragment survives.
            attrs.insert(
                i,
                LineAttribution {
                    start_line: removed.start_line,
                    end_line: start - 1,
                    author_id: removed.author_id.clone(),
                    overrode: removed.overrode.clone(),
                },
            );
            i += 1;
        }
        if removed.end_line > end {
            // Right fragment survives — defer insertion to maintain order.
            to_insert_after.push(LineAttribution {
                start_line: end + 1,
                end_line: removed.end_line,
                author_id: removed.author_id,
                overrode: removed.overrode,
            });
        }
        // Don't increment i — next element shifted into this position.
    }
    for frag in to_insert_after {
        attrs.push(frag);
    }

    // Insert the new attribution.
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id,
        overrode: None,
    });
}

/// Batch read file contents at a specific commit for multiple file paths.
/// Uses a single `git cat-file --batch` call for efficiency.
fn batch_read_file_contents_at_commit(
    repo: &Repository,
    commit_sha: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

    // Build pathspecs like "commit:path" for batch cat-file
    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data: String = file_paths
        .iter()
        .map(|path| format!("{}:{}", commit_sha, path))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let data = &output.stdout;

    let mut results = HashMap::new();
    let mut pos = 0usize;
    let mut path_idx = 0usize;

    while pos < data.len() && path_idx < file_paths.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end]).unwrap_or("");
        let parts: Vec<&str> = header.split_whitespace().collect();

        if parts.len() >= 2 && parts[1] == "missing" {
            // File doesn't exist at this commit
            results.insert(file_paths[path_idx].clone(), String::new());
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        let size: usize = parts[2].parse().unwrap_or(0);
        let content_start = header_end + 1;
        let content_end = content_start + size;

        if content_end <= data.len() {
            let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
            results.insert(file_paths[path_idx].clone(), content);
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
        path_idx += 1;
    }

    Ok(results)
}

/// Pair original commits with new (rebased) commits for authorship rewriting.
///
/// When the counts are equal we use positional pairing (the common case for a
/// normal rebase where every original commit becomes exactly one new commit).
///
/// When counts differ — which happens when an interactive rebase *drops* one or
/// more commits — positional pairing is wrong: e.g. with originals [A, B, C] and
/// new commits [A′, C′] (B was dropped), a positional zip gives [(A,A′),(B,C′)]
/// so C′ is incorrectly attributed using B's note instead of C's.
///
/// We fix this by matching each new commit to the first unused original commit
/// that has the same subject line (first line of the commit message).  If no
/// subject match is found we fall back to the next positionally-available original
/// so that the pairing is never shorter than `new_commits`.
fn pair_commits_for_rewrite(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Vec<(String, String)> {
    if original_commits.len() == new_commits.len() {
        // Equal length: positional pairing is correct and avoids extra git calls.
        return original_commits
            .iter()
            .zip(new_commits.iter())
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
    }

    // Unequal length (dropped or squashed commits): match by commit subject.
    let original_subjects: Vec<(String, String)> = original_commits
        .iter()
        .map(|sha| {
            let subject = repo
                .find_commit(sha.clone())
                .and_then(|c| c.summary())
                .unwrap_or_default();
            (sha.clone(), subject)
        })
        .collect();

    let mut used: HashSet<String> = HashSet::new();
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(new_commits.len());

    for new_sha in new_commits {
        let new_subject = repo
            .find_commit(new_sha.clone())
            .and_then(|c| c.summary())
            .unwrap_or_default();

        // Prefer an unused original with the same subject.
        let matched = original_subjects.iter().find(|(orig_sha, orig_subject)| {
            !used.contains(orig_sha) && *orig_subject == new_subject
        });

        let orig_sha = if let Some((orig_sha, _)) = matched {
            orig_sha.clone()
        } else {
            // No subject match — fall back to the next positionally-available
            // unused original so every new commit gets a pairing.
            match original_subjects
                .iter()
                .find(|(orig_sha, _)| !used.contains(orig_sha))
            {
                Some((orig_sha, _)) => orig_sha.clone(),
                None => {
                    // All originals consumed (shouldn't happen in practice).
                    continue;
                }
            }
        };

        used.insert(orig_sha.clone());
        pairs.push((orig_sha, new_sha.clone()));
    }

    pairs
}

pub fn rewrite_authorship_after_rebase_v2(
    repo: &Repository,
    original_head: &str,
    original_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    let rewrite_start = std::time::Instant::now();
    let mut timing_phases: Vec<(String, u128)> = Vec::new();
    // Handle edge case: no commits to process
    if new_commits.is_empty() {
        return Ok(());
    }

    // Load all note data upfront in a single pass (eliminates ~6 redundant git subprocess calls).
    let phase_start = std::time::Instant::now();
    let note_cache = load_rebase_note_cache(repo, original_commits, new_commits)?;
    timing_phases.push((
        "load_rebase_note_cache".to_string(),
        phase_start.elapsed().as_millis(),
    ));
    debug_performance_log(&format!(
        "rebase_v2: loaded note cache ({} original notes, {} new with notes) in {}ms",
        note_cache.original_note_contents.len(),
        note_cache.new_commits_with_notes.len(),
        phase_start.elapsed().as_millis()
    ));

    // Filter out commits that already have authorship logs (these are commits from the target branch).
    let force_process_existing_notes = original_commits.len() > new_commits.len();
    let commits_to_process: Vec<String> = new_commits
        .iter()
        .filter(|commit| {
            let has_log = !force_process_existing_notes
                && note_cache.new_commits_with_notes.contains(commit.as_str());
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
    let all_commit_pairs = pair_commits_for_rewrite(repo, original_commits, new_commits);
    let commit_pairs_to_process: Vec<(String, String)> = all_commit_pairs
        .into_iter()
        .filter(|(_original_commit, new_commit)| {
            commits_to_process_lookup.contains(new_commit.as_str())
        })
        .collect();
    let original_commits_for_processing: Vec<String> = commit_pairs_to_process
        .iter()
        .map(|(original_commit, _new_commit)| original_commit.clone())
        .collect();

    // Step 1: Use AI-touched files directly from the note cache as pathspecs.
    // This eliminates a diff-tree --stdin subprocess call entirely.
    // The collect_changed_file_contents step will correctly filter to only files that changed.
    let pathspecs: Vec<String> = note_cache.ai_touched_files.iter().cloned().collect();
    timing_phases.push((
        format!("pathspecs_from_note_cache ({} files)", pathspecs.len()),
        0,
    ));

    if pathspecs.is_empty() {
        // No AI-touched files were rewritten. Preserve metadata-only / prompt-only notes by remapping
        // existing source notes to their corresponding rebased commits.
        // Use cached note contents instead of loading again.
        let original_note_contents: HashMap<String, String> = original_commits_for_processing
            .iter()
            .filter_map(|commit| {
                note_cache
                    .original_note_contents
                    .get(commit)
                    .map(|content| (commit.clone(), content.clone()))
            })
            .collect();
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

    if try_fast_path_rebase_note_remap_cached(
        repo,
        original_commits,
        new_commits,
        &commits_to_process_lookup,
        &pathspecs,
        &note_cache,
    )? {
        return Ok(());
    }

    // Step 2a: Run a SINGLE diff-tree call for both new and original commits.
    // This avoids the ~500ms overhead of spawning a second git subprocess.
    // We concatenate both commit lists, get all results at once, then partition them.
    let diff_tree_start = std::time::Instant::now();
    let new_commit_set: HashSet<&str> = commits_to_process.iter().map(String::as_str).collect();
    let mut combined_commits =
        Vec::with_capacity(commits_to_process.len() + original_commits_for_processing.len());
    combined_commits.extend(commits_to_process.iter().cloned());
    combined_commits.extend(original_commits_for_processing.iter().cloned());
    let (combined_diff_tree_result, combined_hunks) =
        run_diff_tree_with_hunks(repo, &combined_commits, &pathspecs_lookup, &pathspecs)?;

    // Partition diff-tree results: only new commits need DiffTreeResult metadata
    let new_commit_deltas: Vec<_> = combined_diff_tree_result
        .commit_deltas
        .into_iter()
        .filter(|(sha, _)| new_commit_set.contains(sha.as_str()))
        .collect();
    let new_blob_oids: Vec<String> = {
        let mut oids = HashSet::new();
        for (_, delta) in &new_commit_deltas {
            for oid in delta.file_to_blob_oid.values().flatten() {
                oids.insert(oid.clone());
            }
        }
        let mut v: Vec<String> = oids.into_iter().collect();
        v.sort();
        v
    };
    let diff_tree_result = DiffTreeResult {
        commit_deltas: new_commit_deltas,
        all_blob_oids: new_blob_oids,
    };
    let actually_changed_files = diff_tree_result.all_changed_files();

    // Partition hunks: new commits vs original commits
    let mut hunks_by_commit: HunksByCommitAndFile = HashMap::new();
    let mut original_hunks_by_commit: HunksByCommitAndFile = HashMap::new();
    for (commit_sha, file_hunks) in combined_hunks {
        if new_commit_set.contains(commit_sha.as_str()) {
            hunks_by_commit.insert(commit_sha, file_hunks);
        } else {
            original_hunks_by_commit.insert(commit_sha, file_hunks);
        }
    }

    timing_phases.push((
        format!(
            "diff_tree_combined ({} new + {} original commits, {} changed files, {} blobs)",
            commits_to_process.len(),
            original_commits_for_processing.len(),
            actually_changed_files.len(),
            diff_tree_result.all_blob_oids.len(),
        ),
        diff_tree_start.elapsed().as_millis(),
    ));

    // Step 2b: Create attribution state from original_head (before rebase)
    // Only load file contents for files that actually change — skip unchanged files.
    let va_phase_start = std::time::Instant::now();

    let (mut current_attributions, mut current_file_contents, initial_prompts, _rebase_ts) =
        if let Some((attrs, contents, prompts)) = try_reconstruct_attributions_from_notes_cached(
            repo,
            original_head,
            original_commits,
            &pathspecs,
            force_process_existing_notes,
            &note_cache,
            &original_hunks_by_commit,
        ) {
            debug_log("Using fast note-based attribution reconstruction (skipping blame)");
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            (attrs, contents, prompts, ts)
        } else {
            debug_log("Falling back to VirtualAttributions (blame-based reconstruction)");
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

            let mut prompts: BTreeMap<
                String,
                BTreeMap<String, crate::authorship::authorship_log::PromptRecord>,
            > = BTreeMap::new();
            for (prompt_id, commit_map) in current_va.prompts() {
                prompts.insert(prompt_id.clone(), commit_map.clone());
            }

            let ts = current_va.timestamp();
            (attrs, contents, prompts, ts)
        };

    timing_phases.push((
        format!("attribution_reconstruction ({} pathspecs)", pathspecs.len()),
        va_phase_start.elapsed().as_millis(),
    ));

    // Step 2c: Read blob contents — only for the FIRST commit that touches each file.
    // Subsequent commits use hunk-based transfer which doesn't need blob content.
    let blob_phase_start = std::time::Instant::now();
    let first_appearance_blobs = {
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut needed_oids: HashSet<String> = HashSet::new();
        for (_, delta) in &diff_tree_result.commit_deltas {
            for (file_path, maybe_oid) in &delta.file_to_blob_oid {
                if let Some(oid) = maybe_oid {
                    // File has content — only read blob on first appearance.
                    if seen_files.insert(file_path.clone()) {
                        needed_oids.insert(oid.clone());
                    }
                } else {
                    // File deleted — clear from seen set so a later recreation
                    // will have its blob read.
                    seen_files.remove(file_path);
                }
            }
        }
        let mut oid_list: Vec<String> = needed_oids.into_iter().collect();
        oid_list.sort();
        oid_list
    };
    let blob_contents = batch_read_blob_contents_parallel(repo, &first_appearance_blobs)?;
    let mut changed_contents_by_commit =
        assemble_changed_contents(diff_tree_result.commit_deltas, &blob_contents);
    drop(blob_contents);
    timing_phases.push((
        format!(
            "blob_read ({} first-appearance blobs of {} total)",
            first_appearance_blobs.len(),
            diff_tree_result.all_blob_oids.len(),
        ),
        blob_phase_start.elapsed().as_millis(),
    ));

    // Build original_head line-to-author maps for content restoration during transform.
    // Built from current_attributions before the loop mutates them.
    // Used as a fallback for files with no previous content in the diff-based transfer.
    let original_head_line_to_author: HashMap<String, HashMap<String, String>> = {
        let mut maps = HashMap::new();
        for (file_path, (_, line_attrs)) in &current_attributions {
            let mut line_map = HashMap::new();
            if let Some(content) = current_file_contents.get(file_path) {
                let lines: Vec<&str> = content.lines().collect();
                for attr in line_attrs {
                    if attr.author_id
                        != crate::authorship::working_log::CheckpointKind::Human.to_str()
                    {
                        for line_num in attr.start_line..=attr.end_line {
                            if let Some(line_content) =
                                lines.get(line_num.saturating_sub(1) as usize)
                            {
                                line_map.insert(line_content.to_string(), attr.author_id.clone());
                            }
                        }
                    }
                }
            }
            if !line_map.is_empty() {
                maps.insert(file_path.clone(), line_map);
            }
        }
        maps
    };

    // No need to build VirtualAttributions wrapper — diff-based transfer replaces
    // transform_changed_files_to_final_state entirely, eliminating the need for VA in the loop.
    let mut current_prompts = initial_prompts.clone();
    let prompt_line_metrics = build_prompt_line_metrics_from_attributions(&current_attributions);
    apply_prompt_line_metrics_to_prompts(&mut current_prompts, &prompt_line_metrics);

    // Bug fix: start existing_files EMPTY and build it up per-commit as files are
    // introduced by new commits.  Previously this was pre-seeded from the final
    // pre-rebase HEAD state, which caused every intermediate commit's note to include
    // files from future commits (future-file leak).
    let mut existing_files: HashSet<String> = HashSet::new();

    // Build current_authorship_log solely for its metadata (used for the initial
    // metadata_json_template_parts below).  Attestations will be empty because
    // existing_files is empty, but that's fine — cached_file_attestation_text is also
    // empty and gets rebuilt per-commit.
    let current_authorship_log = build_authorship_log_from_state(
        original_head,
        &current_prompts,
        &current_attributions,
        &existing_files,
    );

    // Fast serialization: pre-cache per-file attestation text and metadata template.
    // Instead of calling serialize_to_string() per commit (which rebuilds the entire JSON),
    // we cache each file's attestation text and only update changed files. Assembly is
    // pure string concatenation.
    //
    // Bug fix: start EMPTY rather than pre-seeding from current_authorship_log.attestations.
    // The per-commit loop populates this map as each file is first processed via content-diff.
    let mut cached_file_attestation_text: HashMap<String, String> = HashMap::new();

    // Pre-split metadata JSON template at a placeholder so we only swap the commit SHA per commit.
    // This is rebuilt per-commit when metrics change (attributions updated by hunk/diff transfer).
    let mut metadata_json_template_parts: Option<(String, String)> =
        build_metadata_template_parts(&current_authorship_log.metadata, &current_prompts);

    let mut pending_note_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_process.len());
    let mut pending_note_debug: Vec<(String, usize)> = Vec::with_capacity(commits_to_process.len());

    // Pre-compute parent SHAs for all commits to process.
    // Used to look up working-log checkpoint data for AI-resolved conflicts.
    let commit_parent_shas: HashMap<String, String> = {
        let mut map = HashMap::new();
        for sha in &commits_to_process {
            if let Ok(commit) = repo.find_commit(sha.clone()) {
                if let Ok(parent) = commit.parent(0) {
                    map.insert(sha.clone(), parent.id());
                }
            }
        }
        map
    };

    // Step 3: Process each new commit in order (oldest to newest)
    let loop_start = std::time::Instant::now();
    let mut loop_transform_ms = 0u128;
    let mut loop_serialize_us = 0u128;
    let mut loop_diff_ms = 0u128;
    let mut loop_hunk_ms = 0u128;
    let mut loop_attestation_ms = 0u128;
    let mut loop_content_clone_ms = 0u128;
    let mut loop_metrics_ms = 0u128;
    let mut total_files_diffed = 0usize;
    let mut total_lines_diffed = 0usize;
    let mut total_files_hunk_transferred = 0usize;
    // Track files that have been processed via content-diff at least once.
    // After the first content-diff, our accumulated attribution state matches the
    // commit chain, so we can use hunk-based transfer for subsequent appearances.
    let mut files_with_synced_state: HashSet<String> = HashSet::new();

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

        // Get hunk data for this commit (from the pre-computed diff-tree -p -U0 output)
        let commit_hunks = hunks_by_commit.get(new_commit);

        // Only transform attributions for files that actually changed.
        if !changed_files_in_commit.is_empty() {
            // Update file existence: use blob content when available, hunk data otherwise.
            for file_path in &changed_files_in_commit {
                if let Some(content) = new_content_for_changed_files.get(file_path) {
                    if content.is_empty() {
                        existing_files.remove(file_path);
                    } else {
                        existing_files.insert(file_path.clone());
                    }
                }
                // If no blob content available (hunk-based path), file still exists
                // (deletions would have zero OID which yields empty content in the map)
            }

            let t0 = std::time::Instant::now();
            for file_path in &changed_files_in_commit {
                // Check if blob content is available and non-empty (file not deleted)
                let new_content = new_content_for_changed_files.get(file_path);
                let is_file_deleted = new_content.map(|c| c.is_empty()).unwrap_or(false);

                if is_file_deleted {
                    // File deleted — clear all cached state so recreation uses a clean
                    // content-diff instead of stale attributions/content from before deletion.
                    cached_file_attestation_text.remove(file_path);
                    existing_files.remove(file_path);
                    files_with_synced_state.remove(file_path.as_str());
                    current_file_contents.remove(file_path);
                    current_attributions.remove(file_path);
                    continue;
                }

                // Decide: use hunk-based transfer or content-diff?
                let has_hunks = commit_hunks
                    .and_then(|ch| ch.get(file_path.as_str()))
                    .is_some();
                let use_hunk_based =
                    files_with_synced_state.contains(file_path.as_str()) && has_hunks;

                // Skip early if no data available (avoids wasted subtract+add cycle)
                if !use_hunk_based && new_content.is_none() {
                    continue;
                }

                // Metrics are updated after all files in this commit are processed (below).

                let line_attrs = if use_hunk_based {
                    // FAST PATH: Hunk-based attribution transfer
                    let thunk = std::time::Instant::now();
                    let hunks = commit_hunks.unwrap().get(file_path.as_str()).unwrap();
                    let old_attrs = current_attributions
                        .get(file_path)
                        .map(|(_, la)| la.as_slice())
                        .unwrap_or(&[]);
                    let mut result = apply_hunks_to_line_attributions(old_attrs, hunks);
                    // Bug fix: stamp AI attribution for inserted/replaced lines by
                    // content-matching against the original-HEAD line→author map.
                    // apply_hunks_to_line_attributions only shifts existing attributions;
                    // lines in Replace or Insert hunk regions get no attribution from it.
                    // We recover those by looking up each added line's content.
                    if let Some(file_author_map) = original_head_line_to_author.get(file_path) {
                        for hunk in hunks.iter() {
                            if hunk.new_count > 0 {
                                for (i, added_line) in hunk.added_lines.iter().enumerate() {
                                    if let Some(author_id) =
                                        file_author_map.get(added_line.as_str())
                                    {
                                        let line_num = hunk.new_start + i as u32;
                                        overlay_attribution(
                                            &mut result,
                                            line_num,
                                            line_num,
                                            author_id.clone(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    total_files_hunk_transferred += 1;
                    loop_hunk_ms += thunk.elapsed().as_micros();
                    result
                } else {
                    // SLOW PATH: Content-diff based attribution transfer
                    let new_content = new_content.unwrap();
                    let tdiff = std::time::Instant::now();
                    total_files_diffed += 1;
                    let new_line_count = new_content.lines().count();
                    total_lines_diffed += new_line_count;
                    let result = compute_line_attrs_for_changed_file(
                        new_content,
                        current_file_contents.get(file_path),
                        current_attributions
                            .get(file_path)
                            .map(|(_, la)| la.as_slice()),
                        original_head_line_to_author.get(file_path),
                    );
                    loop_diff_ms += tdiff.elapsed().as_micros();
                    files_with_synced_state.insert(file_path.clone());
                    result
                };

                let tatt = std::time::Instant::now();
                if let Some(text) = serialize_attestation_from_line_attrs(file_path, &line_attrs) {
                    cached_file_attestation_text.insert(file_path.clone(), text);
                } else {
                    cached_file_attestation_text.remove(file_path);
                }
                loop_attestation_ms += tatt.elapsed().as_micros();
                let tclone = std::time::Instant::now();
                current_attributions.insert(file_path.clone(), (Vec::new(), line_attrs));
                if !use_hunk_based && let Some(content) = new_content {
                    current_file_contents.insert(file_path.clone(), content.clone());
                }
                loop_content_clone_ms += tclone.elapsed().as_micros();
            }
            loop_transform_ms += t0.elapsed().as_millis();

            // Recompute prompt_line_metrics scoped to only the DELTA of this commit:
            // count only AI lines at positions that were inserted/replaced by this commit
            // (from hunk data), not all accumulated AI lines in the file.  This gives each
            // commit's note an accepted_lines that reflects its own contribution.
            let tmetrics = std::time::Instant::now();
            let delta_prompt_metrics = build_delta_prompt_metrics_from_hunks_and_attrs(
                &current_attributions,
                &changed_files_in_commit,
                commit_hunks,
            );
            apply_prompt_line_metrics_to_prompts(&mut current_prompts, &delta_prompt_metrics);
            // Only include prompts that contributed new AI lines to this commit's diff.
            let active_prompts_for_commit: BTreeMap<String, BTreeMap<String, crate::authorship::authorship_log::PromptRecord>> = current_prompts
                .iter()
                .filter(|(pid, _)| {
                    delta_prompt_metrics
                        .get(pid.as_str())
                        .map_or(false, |m| m.accepted_lines > 0)
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            metadata_json_template_parts =
                build_metadata_template_parts(&current_authorship_log.metadata, &active_prompts_for_commit);
            loop_metrics_ms += tmetrics.elapsed().as_micros();
        }

        // Serialize note for this commit using fast cached assembly.
        // Per-commit-delta: include only files changed by this specific commit.
        let t0 = std::time::Instant::now();
        let commit_has_attestations = !changed_files_in_commit.is_empty()
            && changed_files_in_commit.iter().any(|f| {
                cached_file_attestation_text
                    .get(f.as_str())
                    .map_or(false, |t| !t.is_empty())
            });
        // If the slow-path computation produced AI attestations for this commit's changed
        // files, assemble a fresh note from the per-file cache. Otherwise fall back to
        // the original pre-rebase note (remapped to the new SHA) — this preserves fast-path
        // semantics for commits whose content was unaffected by the rebase, and produces
        // no note when the original commit had none (human-only commits).
        let authorship_json = if commit_has_attestations {
            // Assemble note from cached per-file text for THIS commit's changed files only.
            let mut output = String::with_capacity(512);
            for file_path in &changed_files_in_commit {
                if let Some(text) = cached_file_attestation_text.get(file_path.as_str()) {
                    if !text.is_empty() {
                        output.push_str(text);
                    }
                }
            }
            output.push_str("---\n");
            if let Some((ref prefix, ref suffix)) = metadata_json_template_parts {
                output.push_str(prefix);
                output.push_str(new_commit);
                output.push_str(suffix);
            }
            Some(output)
        } else {
            // No AI attribution from the diff-based transfer.  This is the normal case
            // for human-only commits.  However, it also fires when the conflict was
            // resolved by AI with *different* content than the original commit (e.g.
            // MAX_CONNECTIONS = 100 → 75), because the content-diff can't carry
            // attribution for changed lines.
            //
            // Check the working log for this commit's parent: if it contains an AI
            // checkpoint for any of the changed files (written by `git-ai checkpoint`
            // during `rebase --continue` conflict resolution), use those line_attributions
            // directly to build the note.
            if let Some(parent_sha) = commit_parent_shas.get(new_commit) {
                build_note_from_conflict_wl(
                    repo,
                    new_commit,
                    parent_sha,
                    &changed_files_in_commit,
                )
            } else {
                None
            }
        };
        loop_serialize_us += t0.elapsed().as_micros();
        if let Some(authorship_json) = authorship_json {
            let file_count = changed_files_in_commit
                .iter()
                .filter(|f| {
                    cached_file_attestation_text
                        .get(f.as_str())
                        .map_or(false, |t| !t.is_empty())
                })
                .count();
            pending_note_entries.push((new_commit.clone(), authorship_json));
            pending_note_debug.push((new_commit.clone(), file_count));
        }
    }

    timing_phases.push((
        format!(
            "commit_processing_loop ({} commits)",
            commits_to_process.len()
        ),
        loop_start.elapsed().as_millis(),
    ));
    timing_phases.push(("  loop:transform".to_string(), loop_transform_ms));
    timing_phases.push((
        format!(
            "    transform:diff ({} files, {} lines)",
            total_files_diffed, total_lines_diffed
        ),
        loop_diff_ms / 1000,
    ));
    timing_phases.push((
        format!(
            "    transform:hunk_transfer ({} files)",
            total_files_hunk_transferred
        ),
        loop_hunk_ms / 1000,
    ));
    timing_phases.push((
        "    transform:attestation_serialize".to_string(),
        loop_attestation_ms / 1000,
    ));
    timing_phases.push((
        "    transform:content_clone".to_string(),
        loop_content_clone_ms / 1000,
    ));
    timing_phases.push((
        "    transform:metrics_rebuild".to_string(),
        loop_metrics_ms / 1000,
    ));
    timing_phases.push(("  loop:serialize".to_string(), loop_serialize_us / 1000));
    timing_phases.push(("  loop:metrics".to_string(), loop_metrics_ms / 1000));

    let phase_start = std::time::Instant::now();
    if !pending_note_entries.is_empty() {
        crate::git::refs::notes_add_batch(repo, &pending_note_entries)?;
    }
    timing_phases.push((
        format!("notes_add_batch ({} entries)", pending_note_entries.len()),
        phase_start.elapsed().as_millis(),
    ));

    for (commit_sha, file_count) in pending_note_debug {
        debug_log(&format!(
            "Saved authorship log for commit {} ({} files)",
            commit_sha, file_count
        ));
    }

    let total_ms = rewrite_start.elapsed().as_millis();
    debug_performance_log(&format!(
        "rebase_v2: TOTAL rewrite_authorship_after_rebase_v2 in {}ms",
        total_ms
    ));

    // Write detailed timing breakdown for benchmarking
    if let Ok(timing_path) = std::env::var("GIT_AI_REBASE_TIMING_FILE") {
        let mut summary = format!("TOTAL={}ms\n", total_ms);
        for (name, ms) in &timing_phases {
            summary.push_str(&format!("  {}={}ms\n", name, ms));
        }
        let _ = std::fs::write(&timing_path, summary);
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
    if new_commits.is_empty() {
        return Err(GitAiError::Generic(
            "cherry-pick rewrite missing new commits".to_string(),
        ));
    }

    if source_commits.is_empty() {
        return Err(GitAiError::Generic(
            "cherry-pick rewrite missing source commits".to_string(),
        ));
    }

    if source_commits.len() != new_commits.len() {
        return Err(GitAiError::Generic(format!(
            "cherry-pick rewrite commit count mismatch source_commits={} new_commits={}",
            source_commits.len(),
            new_commits.len()
        )));
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

    if try_fast_path_cherry_pick_note_remap(repo, &commit_pairs, &pathspecs)? {
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

pub(crate) fn committed_file_snapshot_between_commits(
    repo: &Repository,
    from_commit: Option<&str>,
    to_commit: &str,
) -> Result<HashMap<String, String>, GitAiError> {
    let to_commit = repo.find_commit(to_commit.to_string())?;
    let to_tree = to_commit.tree()?;
    if matches!(from_commit, None | Some("initial")) {
        let mut args = repo.global_args_for_exec();
        args.push("ls-tree".to_string());
        args.push("-r".to_string());
        args.push("-z".to_string());
        args.push("--name-only".to_string());
        args.push(to_tree.id());

        let output = exec_git(&args)?;
        let tracked_paths = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .collect::<Vec<_>>();
        return get_committed_files_content(repo, &to_commit.id(), &tracked_paths);
    }

    let from_tree = repo.find_commit(from_commit.unwrap().to_string())?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None, None)?;
    let tracked_paths = diff
        .deltas()
        .filter_map(|delta| delta.new_file().path().or(delta.old_file().path()))
        .map(|path| path.to_string_lossy().to_string())
        .collect::<HashSet<_>>();

    if tracked_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let tracked_lookup = tracked_paths
        .iter()
        .map(|path| path.as_str())
        .collect::<HashSet<_>>();
    let (_changed_files, contents) =
        collect_changed_file_contents_from_diff(repo, &diff, &tracked_lookup)?;
    Ok(contents)
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

            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("tree ") {
                    tree_oid = rest.trim().to_string();
                    break;
                }
            }

            metadata_by_commit.insert(oid, CommitObjectMetadata { tree_oid });
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    Ok(metadata_by_commit)
}

/// Collect changed file contents for a list of commit SHAs using a single diff-tree --stdin call.
/// Result of parsing diff-tree output: per-commit deltas and the set of all blob OIDs needed.
struct DiffTreeResult {
    commit_deltas: Vec<(String, CommitTrackedDelta)>,
    all_blob_oids: Vec<String>, // sorted, deduplicated
}

impl DiffTreeResult {
    fn all_changed_files(&self) -> HashSet<String> {
        let mut files = HashSet::new();
        for (_commit, delta) in &self.commit_deltas {
            files.extend(delta.changed_files.iter().cloned());
        }
        files
    }
}

/// A unified diff hunk header parsed from `git diff-tree -p -U0` output.
/// Represents a contiguous change region in a file.
#[derive(Debug, Clone)]
struct DiffHunk {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
    /// Content of `+` lines from the unified diff output for this hunk.
    /// Used by the hunk-based attribution path to stamp AI attribution on
    /// newly-inserted/replaced lines via content-matching.
    added_lines: Vec<String>,
}

/// Per-commit, per-file hunk information extracted from `git diff-tree -p -U0`.
/// Maps commit_sha → file_path → Vec<DiffHunk>.
type HunksByCommitAndFile = HashMap<String, HashMap<String, Vec<DiffHunk>>>;

/// Parse a unified diff hunk header line like `@@ -10,5 +12,6 @@ context`
/// Returns None if parsing fails.
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 || parts[0] != "@@" {
        return None;
    }

    let old_part = parts[1].trim_start_matches('-');
    let new_part = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range_spec(old_part)?;
    let (new_start, new_count) = parse_range_spec(new_part)?;

    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        added_lines: Vec::new(),
    })
}

/// Parse a range spec like "10,5" or "10" (count defaults to 1, but "10,0" means 0).
fn parse_range_spec(spec: &str) -> Option<(u32, u32)> {
    if let Some((start_str, count_str)) = spec.split_once(',') {
        let start = start_str.parse().ok()?;
        let count = count_str.parse().ok()?;
        Some((start, count))
    } else {
        let start = spec.parse().ok()?;
        Some((start, 1))
    }
}

/// Apply hunk-based line offset adjustments to existing line attributions.
///
/// Instead of re-diffing file contents, this uses pre-computed hunk information from
/// `git diff-tree -p -U0` to adjust attribution line numbers. For each hunk:
/// - Lines before the hunk: keep at same position (with accumulated offset)
/// - Lines in a deletion region: dropped (those lines were removed)
/// - Lines after the hunk: shifted by the net offset (new_count - old_count)
///
/// This is O(attrs + hunks) instead of O(file_length) for the full diff approach.
fn apply_hunks_to_line_attributions(
    old_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
    hunks: &[DiffHunk],
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    if hunks.is_empty() {
        return old_attrs.to_vec();
    }

    // Build preserved segments: ranges of old line numbers that survive and their offset.
    // Between hunks, lines are preserved with an accumulated offset.
    let mut segments: Vec<(u32, u32, i64)> = Vec::with_capacity(hunks.len() + 1);
    let mut offset: i64 = 0;
    let mut prev_old_end: u32 = 1; // 1-indexed

    for hunk in hunks {
        // Preserved segment before this hunk
        if prev_old_end < hunk.old_start + 1 {
            // Lines from prev_old_end to hunk.old_start are preserved
            // For pure insertions (old_count=0), old_start points to the line AFTER which
            // insertion happens, so lines up to and including old_start are preserved
            let seg_end = if hunk.old_count == 0 {
                hunk.old_start // inclusive
            } else {
                hunk.old_start.saturating_sub(1) // up to but not including the hunk
            };
            if prev_old_end <= seg_end {
                segments.push((prev_old_end, seg_end, offset));
            }
        }

        // The hunk itself: old lines old_start..old_start+old_count-1 are deleted/replaced.
        // No segment for these lines (they're removed).
        // For pure insertion (old_count=0): no lines are removed, but offset changes.

        offset += hunk.new_count as i64 - hunk.old_count as i64;

        if hunk.old_count == 0 {
            prev_old_end = hunk.old_start + 1; // after the insertion point
        } else {
            prev_old_end = hunk.old_start + hunk.old_count; // after the deleted range
        }
    }

    // Final segment after last hunk (up to a very large line number)
    segments.push((prev_old_end, u32::MAX, offset));

    // Apply the mapping to each attribution
    let mut new_attrs: Vec<crate::authorship::attribution_tracker::LineAttribution> =
        Vec::with_capacity(old_attrs.len());

    for attr in old_attrs {
        // For each attribution range, find the preserved segments that overlap
        for &(seg_start, seg_end, seg_offset) in &segments {
            let range_start = attr.start_line.max(seg_start);
            let range_end = attr.end_line.min(seg_end);

            if range_start <= range_end {
                let new_start = (range_start as i64 + seg_offset).max(1) as u32;
                let new_end = (range_end as i64 + seg_offset).max(1) as u32;
                new_attrs.push(crate::authorship::attribution_tracker::LineAttribution {
                    start_line: new_start,
                    end_line: new_end,
                    author_id: attr.author_id.clone(),
                    overrode: attr.overrode.clone(),
                });
            }
        }
    }

    new_attrs
}

/// Combined diff-tree call that extracts BOTH raw file metadata (changed files, blob OIDs)
/// AND hunk information from unified diff patches, using a single `git diff-tree --stdin --raw -p -U0` call.
/// This replaces two separate subprocess calls with one.
fn run_diff_tree_with_hunks(
    repo: &Repository,
    commit_shas: &[String],
    pathspecs_lookup: &HashSet<&str>,
    pathspecs: &[String],
) -> Result<(DiffTreeResult, HunksByCommitAndFile), GitAiError> {
    if commit_shas.is_empty() {
        return Ok((
            DiffTreeResult {
                commit_deltas: Vec::new(),
                all_blob_oids: Vec::new(),
            },
            HashMap::new(),
        ));
    }

    // Use --raw for file metadata and -p -U0 for minimal patch hunks, in one call.
    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--raw".to_string());
    args.push("-p".to_string());
    args.push("-U0".to_string());
    args.push("--no-color".to_string());
    args.push("--no-abbrev".to_string());
    args.push("-r".to_string());
    if !pathspecs.is_empty() {
        args.push("--".to_string());
        args.extend(pathspecs.iter().cloned());
    }

    let stdin_data = commit_shas.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let text = String::from_utf8_lossy(&output.stdout);

    // Parse the combined output: raw metadata lines (starting with ':') + unified diff patches
    let commit_set: HashSet<&str> = commit_shas.iter().map(String::as_str).collect();
    let mut commit_deltas: Vec<(String, CommitTrackedDelta)> =
        Vec::with_capacity(commit_shas.len());
    let mut all_blob_oids = HashSet::new();
    let mut hunks_by_commit: HunksByCommitAndFile = HashMap::new();

    let mut current_commit: Option<String> = None;
    let mut current_delta = CommitTrackedDelta::default();
    let mut current_diff_file: Option<String> = None;

    for line in text.lines() {
        // Commit header line (hex SHA)
        // Use .get(..40) instead of &line[..40] to safely handle lines containing
        // multi-byte UTF-8 characters where byte index 40 may not be a char boundary.
        if let Some(prefix) = line.get(..40)
            && commit_set.contains(prefix)
            && prefix.chars().all(|c| c.is_ascii_hexdigit())
        {
            // Save previous commit's delta
            if let Some(ref prev_commit) = current_commit {
                commit_deltas.push((prev_commit.clone(), std::mem::take(&mut current_delta)));
            }
            current_commit = Some(prefix.to_string());
            current_diff_file = None;
            continue;
        }

        // Raw metadata line: :old_mode new_mode old_oid new_oid status\tpath
        if line.starts_with(':') {
            if let Some(ref _commit) = current_commit {
                // Parse raw metadata
                let tab_pos = line.find('\t');
                if let Some(tp) = tab_pos {
                    let metadata = &line[1..tp];
                    let raw_path = &line[tp + 1..];
                    let mut fields = metadata.split_whitespace();
                    let _old_mode = fields.next().unwrap_or_default();
                    let new_mode = fields.next().unwrap_or_default();
                    let _old_oid = fields.next().unwrap_or_default();
                    let new_oid = fields.next().unwrap_or_default();
                    let status = fields.next().unwrap_or_default();
                    let status_char = status.chars().next().unwrap_or('M');

                    // For renames/copies, raw format has "old_path\tnew_path";
                    // use the new (destination) path.
                    let file_path = if matches!(status_char, 'R' | 'C') {
                        raw_path
                            .rsplit_once('\t')
                            .map(|(_, new)| new)
                            .unwrap_or(raw_path)
                            .to_string()
                    } else {
                        raw_path.to_string()
                    };

                    if pathspecs_lookup.contains(file_path.as_str()) {
                        current_delta.changed_files.insert(file_path.clone());
                        let new_blob_oid = if is_zero_oid(new_oid) || !is_blob_mode(new_mode) {
                            None
                        } else {
                            Some(new_oid.to_string())
                        };
                        if let Some(oid) = &new_blob_oid {
                            all_blob_oids.insert(oid.clone());
                        }
                        current_delta
                            .file_to_blob_oid
                            .insert(file_path, new_blob_oid);
                    }
                }
            }
            continue;
        }

        // diff --git a/path b/path
        if line.starts_with("diff --git ") {
            if let Some(b_path) = line.split(" b/").last() {
                current_diff_file = Some(b_path.to_string());
            }
            continue;
        }

        // Hunk header: @@ -old_start[,old_count] +new_start[,new_count] @@
        if line.starts_with("@@ ") {
            if let (Some(commit), Some(file)) = (&current_commit, &current_diff_file)
                && let Some(hunk) = parse_hunk_header(line)
            {
                hunks_by_commit
                    .entry(commit.clone())
                    .or_default()
                    .entry(file.clone())
                    .or_default()
                    .push(hunk);
            }
            continue;
        }

        // Capture `+` lines (added content) into the most-recent hunk for this file.
        // The `+++` file-header line is excluded. With -U0 there are no context lines,
        // so every `+` line is a genuine addition — exactly what we need for the
        // content-match attribution pass in the hunk-based transfer path.
        if line.starts_with('+') && !line.starts_with("+++ ") {
            if let (Some(commit), Some(file)) = (&current_commit, &current_diff_file)
                && let Some(file_hunks) = hunks_by_commit.get_mut(commit)
                && let Some(hunks) = file_hunks.get_mut(file.as_str())
                && let Some(last_hunk) = hunks.last_mut()
            {
                last_hunk.added_lines.push(line[1..].to_string());
            }
            continue;
        }

        // Skip other lines (index, ---, context lines)
    }

    // Save last commit's delta
    if let Some(ref commit) = current_commit {
        commit_deltas.push((commit.clone(), std::mem::take(&mut current_delta)));
    }

    // Ensure all commits have deltas (some may have no changes)
    let delta_commits: HashSet<String> = commit_deltas.iter().map(|(c, _)| c.clone()).collect();
    for commit_sha in commit_shas {
        if !delta_commits.contains(commit_sha) {
            commit_deltas.push((commit_sha.clone(), CommitTrackedDelta::default()));
        }
    }

    let mut blob_oid_list: Vec<String> = all_blob_oids.into_iter().collect();
    blob_oid_list.sort();

    Ok((
        DiffTreeResult {
            commit_deltas,
            all_blob_oids: blob_oid_list,
        },
        hunks_by_commit,
    ))
}

/// Assemble per-commit changed file contents from diff-tree deltas and blob contents.
fn assemble_changed_contents(
    commit_deltas: Vec<(String, CommitTrackedDelta)>,
    blob_contents: &HashMap<String, String>,
) -> ChangedFileContentsByCommit {
    let mut result = HashMap::new();
    for (commit_sha, delta) in commit_deltas {
        let mut contents = HashMap::new();
        for (file_path, maybe_blob_oid) in delta.file_to_blob_oid {
            match maybe_blob_oid {
                None => {
                    // No blob OID = file was deleted (zero OID in diff-tree)
                    contents.insert(file_path, String::new());
                }
                Some(ref oid) => {
                    // Only include if we actually read this blob's content.
                    // Non-first-appearance blobs are skipped during reading
                    // and will use hunk-based transfer instead.
                    if let Some(content) = blob_contents.get(oid) {
                        contents.insert(file_path, content.clone());
                    }
                    // else: blob not read — file will use hunk-based path
                }
            }
        }
        result.insert(commit_sha, (delta.changed_files, contents));
    }
    result
}

/// Read blob contents in parallel using multiple `git cat-file --batch` processes.
/// Falls back to a single call for small batches.
const MAX_PARALLEL_BLOB_READS: usize = 4;
const BLOB_BATCH_CHUNK_SIZE: usize = 200;

fn batch_read_blob_contents_parallel(
    repo: &Repository,
    blob_oids: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if blob_oids.is_empty() {
        return Ok(HashMap::new());
    }
    if blob_oids.len() <= BLOB_BATCH_CHUNK_SIZE {
        return batch_read_blob_contents(repo, blob_oids);
    }

    let global_args = repo.global_args_for_exec();
    let chunks: Vec<Vec<String>> = blob_oids
        .chunks(BLOB_BATCH_CHUNK_SIZE)
        .map(|c| c.to_vec())
        .collect();

    let results = smol::block_on(async {
        let semaphore = std::sync::Arc::new(smol::lock::Semaphore::new(MAX_PARALLEL_BLOB_READS));
        let mut tasks = Vec::new();

        for chunk in chunks {
            let args = global_args.clone();
            let sem = std::sync::Arc::clone(&semaphore);

            let task = smol::spawn(async move {
                let _permit = sem.acquire().await;
                smol::unblock(move || {
                    let mut cat_args = args;
                    cat_args.push("cat-file".to_string());
                    cat_args.push("--batch".to_string());
                    let stdin_data = chunk.join("\n") + "\n";
                    let output = exec_git_stdin(&cat_args, stdin_data.as_bytes())?;
                    parse_cat_file_batch_output_with_oids(&output.stdout)
                })
                .await
            });

            tasks.push(task);
        }

        futures::future::join_all(tasks).await
    });

    let mut merged = HashMap::new();
    for result in results {
        merged.extend(result?);
    }
    Ok(merged)
}

pub fn rewrite_authorship_after_commit_amend(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    _human_author: String,
) -> Result<AuthorshipLog, GitAiError> {
    rewrite_authorship_after_commit_amend_with_snapshot(
        repo,
        original_commit,
        amended_commit,
        _human_author,
        None,
    )
}

pub fn rewrite_authorship_after_commit_amend_with_snapshot(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    human_author: String,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<AuthorshipLog, GitAiError> {
    use crate::authorship::virtual_attribution::VirtualAttributions;

    // Get the files that changed between original and amended commit
    let changed_files = repo.list_commit_files(amended_commit, None)?;
    let mut pathspecs: HashSet<String> = changed_files.into_iter().collect();

    let working_log = repo.storage.working_log_for_base_commit(original_commit)?;
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
    let working_va = if let Some(snapshot) = final_state_override {
        smol::block_on(async {
            VirtualAttributions::from_working_log_for_commit_snapshot(
                repo_clone,
                original_commit.to_string(),
                &pathspecs_vec,
                if has_existing_prompts {
                    None
                } else {
                    Some(human_author.clone())
                },
                None,
                snapshot,
            )
            .await
        })?
    } else {
        smol::block_on(async {
            VirtualAttributions::from_working_log_for_commit(
                repo_clone,
                original_commit.to_string(),
                &pathspecs_vec,
                if has_existing_prompts {
                    None
                } else {
                    Some(human_author.clone())
                },
                None,
            )
            .await
        })?
    };

    // Phase 2: Get parent of amended commit for diff calculation
    let amended_commit_obj = repo.find_commit(amended_commit.to_string())?;
    let parent_sha = if amended_commit_obj.parent_count()? > 0 {
        amended_commit_obj.parent(0)?.id().to_string()
    } else {
        "initial".to_string()
    };

    let pathspecs_set = pathspecs;

    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            amended_commit,
            Some(&pathspecs_set),
            final_state_override,
        )?;

    // Update base commit SHA
    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Inject custom attributes into all PromptRecords (same behavior as post_commit).
    // Always use Config::fresh() to support runtime config updates
    // (especially important for daemon mode, but also good for consistency)
    let custom_attrs = crate::config::Config::fresh().custom_attributes().clone();
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    // Save authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    crate::git::refs::notes_add(repo, amended_commit, &authorship_json)?;

    // Save INITIAL file for uncommitted attributions
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo.storage.working_log_for_base_commit(amended_commit)?;
        let initial_file_contents =
            working_va.snapshot_contents_for_files(initial_attributions.files.keys());
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_file_contents,
        )?;
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
    final_state_override: Option<HashMap<String, String>>,
) -> Result<(), GitAiError> {
    if target_commit_sha.trim().is_empty()
        || old_head_sha.trim().is_empty()
        || is_zero_oid(target_commit_sha)
        || is_zero_oid(old_head_sha)
    {
        debug_log("Skipping reset working-log reconstruction for invalid zero/empty oid");
        return Ok(());
    }

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
            .filter(|f| {
                user_paths.iter().any(|p| {
                    f == p
                        || (p.ends_with('/') && f.starts_with(p))
                        || f.starts_with(&format!("{}/", p))
                })
            })
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

    // Step 2: Build final state from the captured command-exit snapshot when available.
    let has_captured_snapshot = final_state_override.is_some();
    let final_state = if let Some(final_state_override) = final_state_override {
        final_state_override
    } else {
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
        final_state
    };

    // Step 3: Build VirtualAttributions from old_head with working log applied.
    // When we have a captured snapshot, use it instead of the live worktree so line
    // coordinates stay stable under async replay.
    let repo_clone = repo.clone();
    let old_head_clone = old_head_sha.to_string();
    let pathspecs_clone = pathspecs.clone();

    let old_head_va = if has_captured_snapshot {
        smol::block_on(async {
            crate::authorship::virtual_attribution::VirtualAttributions::from_working_log_for_commit_snapshot(
                repo_clone,
                old_head_clone,
                &pathspecs_clone,
                None,
                Some(target_commit_sha.to_string()),
                &final_state,
            )
            .await
        })?
    } else {
        smol::block_on(async {
            crate::authorship::virtual_attribution::VirtualAttributions::from_working_log_for_commit(
                repo_clone,
                old_head_clone,
                &pathspecs_clone,
                None,
                Some(target_commit_sha.to_string()),
            )
            .await
        })?
    };

    debug_log(&format!(
        "Built old_head VA with {} files, {} prompts",
        old_head_va.files().len(),
        old_head_va.prompts().len()
    ));

    // Step 4: Build VirtualAttributions from target_commit
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

    // Step 6: Convert to INITIAL (everything is uncommitted after reset) without consulting the
    // live worktree again.
    let initial_attributions = merged_va.to_initial_working_log_only();

    debug_log(&format!(
        "Generated INITIAL attributions for {} files, {} prompts",
        initial_attributions.files.len(),
        initial_attributions.prompts.len()
    ));

    // Step 7: Write INITIAL file
    let new_working_log = repo
        .storage
        .working_log_for_base_commit(target_commit_sha)?;
    new_working_log.reset_working_log()?;

    if !initial_attributions.files.is_empty() {
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            final_state,
        )?;
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

/// Test-only wrapper that builds a RebaseNoteCache and calls the cached version.
#[cfg(test)]
fn try_fast_path_rebase_note_remap(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
    commits_to_process_lookup: &HashSet<&str>,
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    let note_cache = load_rebase_note_cache(repo, original_commits, new_commits)?;
    try_fast_path_rebase_note_remap_cached(
        repo,
        original_commits,
        new_commits,
        commits_to_process_lookup,
        tracked_paths,
        &note_cache,
    )
}

/// Cached version of try_fast_path_rebase_note_remap that uses pre-loaded note data.
fn try_fast_path_rebase_note_remap_cached(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
    commits_to_process_lookup: &HashSet<&str>,
    tracked_paths: &[String],
    note_cache: &RebaseNoteCache,
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

    // Use cached note blob OIDs and contents instead of additional git calls.
    for (original_commit, _) in &commits_to_remap {
        if !note_cache
            .original_note_blob_oids
            .contains_key(original_commit)
        {
            return Ok(false);
        }
    }

    let mut remapped_note_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_remap.len());
    for (original_commit, new_commit) in &commits_to_remap {
        let Some(raw_note) = note_cache.original_note_contents.get(original_commit) else {
            return Ok(false);
        };
        remapped_note_entries.push((
            new_commit.clone(),
            remap_note_content_for_target_commit(raw_note, new_commit),
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

fn try_fast_path_cherry_pick_note_remap(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    let fast_path_start = std::time::Instant::now();
    if commit_pairs.is_empty() || tracked_paths.is_empty() {
        return Ok(false);
    }

    let compare_start = std::time::Instant::now();
    if !tracked_paths_match_for_commit_pairs(repo, commit_pairs, tracked_paths)? {
        return Ok(false);
    }
    debug_performance_log(&format!(
        "Fast-path cherry-pick note remap: compared tracked blobs for {} commit pairs in {}ms",
        commit_pairs.len(),
        compare_start.elapsed().as_millis()
    ));

    let source_commits: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _new_commit)| source_commit.clone())
        .collect();
    let note_oid_lookup_start = std::time::Instant::now();
    let source_note_blob_oids = note_blob_oids_for_commits(repo, &source_commits)?;
    debug_performance_log(&format!(
        "Fast-path cherry-pick note remap: resolved {} note blob oids in {}ms",
        source_note_blob_oids.len(),
        note_oid_lookup_start.elapsed().as_millis()
    ));
    if source_note_blob_oids.len() != source_commits.len() {
        return Ok(false);
    }

    let mut remapped_blob_entries: Vec<(String, String)> = Vec::with_capacity(commit_pairs.len());
    for (source_commit, new_commit) in commit_pairs {
        let blob_oid = match source_note_blob_oids.get(source_commit) {
            Some(oid) => oid.clone(),
            None => return Ok(false),
        };
        remapped_blob_entries.push((new_commit.clone(), blob_oid));
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
        "Fast-path cherry-pick note remap: wrote {} remapped notes in {}ms",
        remapped_count,
        write_start.elapsed().as_millis()
    ));

    debug_log(&format!(
        "Fast-path remapped authorship logs for {} cherry-picked commits (blob-equivalent tracked files)",
        remapped_count
    ));
    debug_performance_log(&format!(
        "Fast-path cherry-pick note remap complete in {}ms",
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

fn build_metadata_template_parts(
    metadata: &crate::authorship::authorship_log_serialization::AuthorshipMetadata,
    prompts: &BTreeMap<String, BTreeMap<String, crate::authorship::authorship_log::PromptRecord>>,
) -> Option<(String, String)> {
    let mut template_meta = metadata.clone();
    template_meta.base_commit_sha = "BASE_COMMIT_SHA_PLACEHOLDER".to_string();
    template_meta.prompts = flatten_prompts_for_metadata(prompts);
    serde_json::to_string_pretty(&template_meta)
        .ok()
        .map(|template| {
            let parts: Vec<&str> = template.splitn(2, "BASE_COMMIT_SHA_PLACEHOLDER").collect();
            (
                parts[0].to_string(),
                parts.get(1).unwrap_or(&"").to_string(),
            )
        })
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

/// Serialize attestation text directly from line_attrs without building intermediate FileAttestation.
/// This avoids HashMap allocation, sorting, and range merging overhead.
fn serialize_attestation_from_line_attrs(
    file_path: &str,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Option<String> {
    use std::fmt::Write;

    if line_attrs.is_empty() {
        return None;
    }

    let human_id = crate::authorship::working_log::CheckpointKind::Human.to_str();

    // Collect runs of (author_id, start, end) merging adjacent lines
    let mut runs: Vec<(&str, u32, u32)> = Vec::new();
    for attr in line_attrs {
        if attr.author_id == human_id {
            continue;
        }
        match runs.last_mut() {
            Some((last_author, _, last_end))
                if *last_author == attr.author_id.as_str() && attr.start_line <= *last_end + 1 =>
            {
                *last_end = (*last_end).max(attr.end_line);
            }
            _ => {
                runs.push((attr.author_id.as_str(), attr.start_line, attr.end_line));
            }
        }
    }

    if runs.is_empty() {
        return None;
    }

    let mut output = String::with_capacity(128);
    if file_path.contains(' ') || file_path.contains('\t') || file_path.contains('\n') {
        let _ = write!(output, "\"{}\"", file_path);
    } else {
        output.push_str(file_path);
    }
    output.push('\n');

    // Group runs by author_id, preserving order of first appearance
    let mut author_order: Vec<&str> = Vec::new();
    let mut author_ranges: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
    for &(author, start, end) in &runs {
        let entry = author_ranges.entry(author).or_default();
        if entry.is_empty() {
            author_order.push(author);
        }
        entry.push((start, end));
    }

    for author in &author_order {
        output.push_str("  ");
        output.push_str(author);
        output.push(' ');
        let ranges = &author_ranges[author];
        let mut first = true;
        for &(start, end) in ranges {
            if !first {
                output.push(',');
            }
            first = false;
            if start == end {
                let _ = write!(output, "{}", start);
            } else {
                let _ = write!(output, "{}-{}", start, end);
            }
        }
        output.push('\n');
    }

    Some(output)
}

/// Compute new line attributions for a file after content changes.
/// Uses diff-based positional transfer when previous content/attrs are available,
/// otherwise falls back to content-matching from the original_head line→author map.
fn compute_line_attrs_for_changed_file(
    new_content: &str,
    old_content: Option<&String>,
    old_attrs: Option<&[crate::authorship::attribution_tracker::LineAttribution]>,
    original_head_line_map: Option<&HashMap<String, String>>,
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    if let (Some(old_c), Some(old_a)) = (old_content, old_attrs) {
        diff_based_line_attribution_transfer(old_c, new_content, old_a)
    } else {
        // No previous content — fall back to content-matching from original_head
        let mut attrs = Vec::new();
        for (line_idx, line_content) in new_content.lines().enumerate() {
            if let Some(author_id) = original_head_line_map.and_then(|m| m.get(line_content)) {
                let line_num = (line_idx + 1) as u32;
                attrs.push(crate::authorship::attribution_tracker::LineAttribution {
                    start_line: line_num,
                    end_line: line_num,
                    author_id: author_id.clone(),
                    overrode: None,
                });
            }
        }
        attrs
    }
}

/// Transfer line attributions from old file content to new file content using line-level diffing.
/// This replaces the blame-based slow path by using imara-diff to compute how lines moved
/// between the old and new versions, then carrying attributions forward positionally.
///
/// - Equal lines: carry the original attribution forward
/// - Inserted lines: no attribution (new content)
/// - Deleted lines: dropped
/// - Replaced lines: no attribution (content changed)
fn diff_based_line_attribution_transfer(
    old_content: &str,
    new_content: &str,
    old_line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Build a lookup from 0-indexed line index → author_id for old content
    let mut old_line_author: Vec<Option<&str>> = vec![None; old_lines.len()];
    for attr in old_line_attrs {
        for line_num in attr.start_line..=attr.end_line {
            let idx = (line_num as usize).saturating_sub(1);
            if idx < old_line_author.len() {
                old_line_author[idx] = Some(&attr.author_id);
            }
        }
    }

    let diff_ops = capture_diff_slices(&old_lines, &new_lines);

    let mut new_line_attrs: Vec<crate::authorship::attribution_tracker::LineAttribution> =
        Vec::with_capacity(new_lines.len());

    for op in &diff_ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                // Carry attributions forward for equal lines
                for i in 0..*len {
                    let old_idx = old_index + i;
                    let new_line_num = (new_index + i + 1) as u32;
                    if let Some(Some(author_id)) = old_line_author.get(old_idx) {
                        new_line_attrs.push(
                            crate::authorship::attribution_tracker::LineAttribution {
                                start_line: new_line_num,
                                end_line: new_line_num,
                                author_id: author_id.to_string(),
                                overrode: None,
                            },
                        );
                    }
                }
            }
            DiffOp::Insert { .. } | DiffOp::Delete { .. } | DiffOp::Replace { .. } => {
                // Insert: new lines, no attribution
                // Delete: old lines removed, nothing to output
                // Replace: content changed, no attribution carried
            }
        }
    }

    new_line_attrs
}

/// Build an authorship note for `new_commit` from working-log checkpoint data stored
/// under `parent_sha`.  This is the fallback path for AI-resolved rebase conflicts:
/// when content-diff transfer produces no AI attribution (because the AI wrote *different*
/// content from the original commit), we fall back to the `line_attributions` that
/// `git-ai checkpoint` recorded in the working log during `rebase --continue`.
///
/// Returns `None` when no AI checkpoint data exists for any of `changed_files`
/// (human-only resolution or no checkpoint at all).
fn build_note_from_conflict_wl(
    repo: &crate::git::repository::Repository,
    new_commit: &str,
    parent_sha: &str,
    changed_files: &HashSet<String>,
) -> Option<String> {
    use crate::authorship::authorship_log_serialization::generate_short_hash;
    use crate::authorship::working_log::CheckpointKind;

    let working_log = repo.storage.working_log_for_base_commit(parent_sha).ok()?;
    let checkpoints = working_log.read_all_checkpoints().ok()?;

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = new_commit.to_string();

    let mut has_ai_content = false;

    for checkpoint in &checkpoints {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }

        // Record the prompt from this AI checkpoint in the metadata.
        if let Some(agent_id) = &checkpoint.agent_id {
            let author_id = generate_short_hash(&agent_id.id, &agent_id.tool);
            authorship_log
                .metadata
                .prompts
                .entry(author_id)
                .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    messages: Vec::new(),
                    total_additions: checkpoint.line_stats.additions,
                    total_deletions: checkpoint.line_stats.deletions,
                    accepted_lines: 0,
                    overriden_lines: 0,
                    messages_url: None,
                    custom_attributes: None,
                });
        }

        for entry in &checkpoint.entries {
            if !changed_files.contains(&entry.file) {
                continue;
            }
            if entry.line_attributions.is_empty() {
                continue;
            }
            if let Some(file_att) =
                build_file_attestation_from_line_attributions(&entry.file, &entry.line_attributions)
            {
                authorship_log.attestations.push(file_att);
                has_ai_content = true;
            }
        }
    }

    if !has_ai_content {
        return None;
    }

    authorship_log
        .serialize_to_string()
        .ok()
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

/// Compute per-commit-delta prompt line metrics by intersecting the
/// post-processing line attributions with the hunk data for this commit.
/// Only counts AI lines at line positions that were INSERTED or REPLACED
/// by this commit (i.e., lines in the hunk's new-side range).
///
/// This gives the correct per-commit contribution: a commit that carries
/// forward 8 AI lines from its parent plus adds 8 new AI lines will report
/// accepted_lines = 8, not 16.
fn build_delta_prompt_metrics_from_hunks_and_attrs(
    attributions: &HashMap<
        String,
        (
            Vec<crate::authorship::attribution_tracker::Attribution>,
            Vec<crate::authorship::attribution_tracker::LineAttribution>,
        ),
    >,
    changed_files: &HashSet<String>,
    commit_hunks: Option<&HashMap<String, Vec<DiffHunk>>>,
) -> HashMap<String, PromptLineMetrics> {
    let human_id = crate::authorship::working_log::CheckpointKind::Human.to_str();
    let mut metrics: HashMap<String, PromptLineMetrics> = HashMap::new();

    for file_path in changed_files {
        let Some((_, line_attrs)) = attributions.get(file_path) else {
            continue;
        };

        let file_hunks = commit_hunks.and_then(|h| h.get(file_path.as_str()));
        let Some(file_hunks) = file_hunks else {
            // No hunk data for this file — count all AI lines as delta.
            // Happens for files not tracked by the diff (e.g. new binary files).
            add_prompt_line_metrics_for_line_attributions(&mut metrics, line_attrs);
            continue;
        };

        // Build set of new-side line numbers (lines inserted/replaced by this commit).
        let mut added_line_nums: HashSet<u32> = HashSet::with_capacity(
            file_hunks.iter().map(|h| h.new_count as usize).sum(),
        );
        for hunk in file_hunks {
            for i in 0..hunk.new_count {
                added_line_nums.insert(hunk.new_start + i);
            }
        }

        // Count AI attributions only at inserted positions.
        for attr in line_attrs {
            if attr.author_id == human_id {
                continue;
            }
            for line_num in attr.start_line..=attr.end_line {
                if added_line_nums.contains(&line_num) {
                    if let Some(m) = metrics.get_mut(&attr.author_id) {
                        m.accepted_lines = m.accepted_lines.saturating_add(1);
                    } else {
                        metrics.insert(
                            attr.author_id.clone(),
                            PromptLineMetrics {
                                accepted_lines: 1,
                                overridden_lines: 0,
                            },
                        );
                    }
                }
            }
        }
    }

    metrics
}

fn add_prompt_line_metrics_for_line_attributions(
    metrics: &mut HashMap<String, PromptLineMetrics>,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) {
    let human_id = crate::authorship::working_log::CheckpointKind::Human.to_str();
    for line_attr in line_attrs {
        let line_count = line_attr
            .end_line
            .saturating_sub(line_attr.start_line)
            .saturating_add(1);
        if line_attr.author_id != human_id {
            // Use get_mut to avoid cloning author_id when the key already exists
            if let Some(entry) = metrics.get_mut(&line_attr.author_id) {
                entry.accepted_lines = entry.accepted_lines.saturating_add(line_count);
            } else {
                metrics.insert(
                    line_attr.author_id.clone(),
                    PromptLineMetrics {
                        accepted_lines: line_count,
                        overridden_lines: 0,
                    },
                );
            }
        }
        if let Some(overrode_id) = &line_attr.overrode {
            if let Some(entry) = metrics.get_mut(overrode_id) {
                entry.overridden_lines = entry.overridden_lines.saturating_add(line_count);
            } else {
                metrics.insert(
                    overrode_id.clone(),
                    PromptLineMetrics {
                        accepted_lines: 0,
                        overridden_lines: line_count,
                    },
                );
            }
        }
    }
}

#[allow(dead_code)]
fn subtract_prompt_line_metrics_for_line_attributions(
    metrics: &mut HashMap<String, PromptLineMetrics>,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) {
    let human_id = crate::authorship::working_log::CheckpointKind::Human.to_str();
    for line_attr in line_attrs {
        let line_count = line_attr
            .end_line
            .saturating_sub(line_attr.start_line)
            .saturating_add(1);
        if line_attr.author_id != human_id
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
        parse_cat_file_batch_output_with_oids, rewrite_authorship_after_cherry_pick,
        transform_attributions_to_final_state, try_fast_path_rebase_note_remap,
        walk_commits_to_base,
    };
    use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
    use crate::authorship::authorship_log::{LineRange, PromptRecord};
    use crate::authorship::authorship_log_serialization::{
        AttestationEntry, AuthorshipLog, FileAttestation,
    };
    use crate::authorship::virtual_attribution::VirtualAttributions;
    use crate::authorship::working_log::{AgentId, Checkpoint, CheckpointKind};
    use crate::git::refs::{notes_add, show_authorship_note};
    use crate::git::rewrite_log::{RebaseCompleteEvent, RewriteLogEvent};
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
    fn rewrite_authorship_after_cherry_pick_errors_on_mismatched_commit_counts() {
        let repo = TmpRepo::new().expect("tmp repo");
        let err = rewrite_authorship_after_cherry_pick(
            repo.gitai_repo(),
            &["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()],
            &[
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                "cccccccccccccccccccccccccccccccccccccccc".to_string(),
            ],
            "human",
        )
        .expect_err("mismatched cherry-pick mapping should fail");

        assert!(
            err.to_string()
                .contains("cherry-pick rewrite commit count mismatch")
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

    #[test]
    fn rebase_complete_migrates_initial_to_new_head() {
        let repo = TmpRepo::new().expect("create tmp repo");

        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature")
            .expect("create feature branch");
        repo.write_file("feature.txt", "feature code\n", true)
            .expect("write feature");
        repo.commit_with_message("feature commit")
            .expect("commit feature");
        let original_head = repo.get_head_commit_sha().expect("feature head sha");

        let mut initial_files = HashMap::new();
        initial_files.insert(
            "uncommitted.txt".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 5,
                author_id: "ai-author-1".to_string(),
                overrode: None,
            }],
        );
        let mut prompts = HashMap::new();
        prompts.insert(
            "ai-author-1".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "test-tool".to_string(),
                    id: "session-1".to_string(),
                    model: "test-model".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 5,
                total_deletions: 0,
                accepted_lines: 5,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E100".to_string()),
                    ("team".to_string(), "test".to_string()),
                ])),
            },
        );

        let old_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&original_head)
            .unwrap();
        old_wl
            .write_initial_attributions(initial_files.clone(), prompts.clone())
            .expect("write INITIAL");

        let old_initial = old_wl.read_initial_attributions();
        assert_eq!(
            old_initial.files.len(),
            1,
            "INITIAL should exist on old HEAD before rebase"
        );

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("upstream.txt", "upstream\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream commit")
            .expect("commit upstream");
        let new_head = repo
            .get_head_commit_sha()
            .expect("upstream sha as simulated new_head");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite_authorship_if_needed should succeed");

        let new_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap();
        let migrated = new_wl.read_initial_attributions();

        assert_eq!(
            migrated.files.len(),
            1,
            "INITIAL should have been migrated to new HEAD"
        );
        assert!(
            migrated.files.contains_key("uncommitted.txt"),
            "migrated INITIAL should contain the uncommitted file"
        );
        let attrs = &migrated.files["uncommitted.txt"];
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 5);
        assert_eq!(attrs[0].author_id, "ai-author-1");

        assert!(
            migrated.prompts.contains_key("ai-author-1"),
            "migrated INITIAL should preserve prompt records"
        );
    }

    #[test]
    fn rebase_complete_no_initial_is_noop() {
        let repo = TmpRepo::new().expect("create tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file("feature.txt", "code\n", true)
            .expect("write feature");
        repo.commit_with_message("feature commit")
            .expect("commit feature");
        let original_head = repo.get_head_commit_sha().expect("feature sha");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("upstream.txt", "upstream\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream commit")
            .expect("commit upstream");
        let new_head = repo.get_head_commit_sha().expect("upstream sha");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite_authorship_if_needed should succeed with no INITIAL");

        let new_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap();
        let migrated = new_wl.read_initial_attributions();
        assert!(
            migrated.files.is_empty(),
            "no INITIAL should exist on new HEAD when none existed on old HEAD"
        );
    }

    #[test]
    fn rebase_complete_migrates_multi_file_initial() {
        let repo = TmpRepo::new().expect("create tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file("feature.txt", "feature\n", true)
            .expect("write feature");
        repo.commit_with_message("feature commit")
            .expect("commit feature");
        let original_head = repo.get_head_commit_sha().expect("feature sha");

        let mut initial_files = HashMap::new();
        initial_files.insert(
            "file_a.py".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 10,
                author_id: "ai-cursor".to_string(),
                overrode: None,
            }],
        );
        initial_files.insert(
            "file_b.py".to_string(),
            vec![
                LineAttribution {
                    start_line: 1,
                    end_line: 3,
                    author_id: "ai-cursor".to_string(),
                    overrode: None,
                },
                LineAttribution {
                    start_line: 7,
                    end_line: 12,
                    author_id: "ai-copilot".to_string(),
                    overrode: None,
                },
            ],
        );

        let mut prompts = HashMap::new();
        prompts.insert(
            "ai-cursor".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess-1".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 13,
                total_deletions: 0,
                accepted_lines: 13,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E200".to_string()),
                    ("team".to_string(), "platform".to_string()),
                ])),
            },
        );
        prompts.insert(
            "ai-copilot".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "copilot".to_string(),
                    id: "sess-2".to_string(),
                    model: "gpt-4o".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 6,
                total_deletions: 0,
                accepted_lines: 6,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E200".to_string()),
                    ("team".to_string(), "platform".to_string()),
                ])),
            },
        );

        let old_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&original_head)
            .unwrap();
        old_wl
            .write_initial_attributions(initial_files, prompts)
            .expect("write multi-file INITIAL");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("upstream.txt", "upstream\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream")
            .expect("commit upstream");
        let new_head = repo.get_head_commit_sha().expect("new sha");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite should succeed");

        let migrated = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap()
            .read_initial_attributions();

        assert_eq!(migrated.files.len(), 2, "both files should be migrated");
        assert!(migrated.files.contains_key("file_a.py"));
        assert!(migrated.files.contains_key("file_b.py"));

        let b_attrs = &migrated.files["file_b.py"];
        assert_eq!(
            b_attrs.len(),
            2,
            "file_b.py should have both attribution ranges"
        );

        assert_eq!(
            migrated.prompts.len(),
            2,
            "both prompt records should be migrated"
        );
        assert!(migrated.prompts.contains_key("ai-cursor"));
        assert!(migrated.prompts.contains_key("ai-copilot"));
    }

    #[test]
    fn rebase_complete_merges_initial_when_both_working_logs_exist() {
        let repo = TmpRepo::new().expect("create tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file("feature.txt", "feature\n", true)
            .expect("write feature");
        repo.commit_with_message("feature commit")
            .expect("commit feature");
        let original_head = repo.get_head_commit_sha().expect("feature sha");

        let mut old_initial_files = HashMap::new();
        old_initial_files.insert(
            "old_file.txt".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 3,
                author_id: "ai-old".to_string(),
                overrode: None,
            }],
        );
        let mut old_prompts = HashMap::new();
        old_prompts.insert(
            "ai-old".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "test-tool".to_string(),
                    id: "old-session".to_string(),
                    model: "test-model".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 3,
                total_deletions: 0,
                accepted_lines: 3,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E300".to_string()),
                    ("team".to_string(), "infra".to_string()),
                ])),
            },
        );

        let old_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&original_head)
            .unwrap();
        old_wl
            .write_initial_attributions(old_initial_files, old_prompts)
            .expect("write old INITIAL");

        repo.switch_branch(&default_branch)
            .expect("switch default branch");
        repo.write_file("upstream.txt", "upstream\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream commit")
            .expect("commit upstream");
        let new_head = repo.get_head_commit_sha().expect("upstream sha");

        let new_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap();
        let checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            "diff".to_string(),
            "new-author".to_string(),
            vec![],
        );
        new_wl
            .append_checkpoint(&checkpoint)
            .expect("write checkpoint on new HEAD");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite should succeed when both working logs exist");

        let merged_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap();
        let migrated = merged_wl.read_initial_attributions();

        assert_eq!(
            migrated.files.len(),
            1,
            "INITIAL from old HEAD should be merged into new HEAD"
        );
        assert!(migrated.files.contains_key("old_file.txt"));
        assert!(migrated.prompts.contains_key("ai-old"));

        let checkpoints = merged_wl
            .read_all_checkpoints()
            .expect("read checkpoints on new HEAD");
        assert_eq!(
            checkpoints.len(),
            1,
            "checkpoint on new HEAD should be preserved"
        );
        assert_eq!(checkpoints[0].author, "new-author");

        assert!(
            !repo.gitai_repo().storage.has_working_log(&original_head),
            "old working log should be cleaned up"
        );
    }

    #[test]
    fn regression_initial_preserved_through_checkpoint_commit_rebase() {
        let repo = TmpRepo::new().expect("create tmp repo");

        repo.write_file("app.py", "def main():\n    print('hello')\n", true)
            .expect("write base app.py");
        repo.commit_with_message("initial commit")
            .expect("initial commit");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file(
            "app.py",
            "import logging\ndef main():\n    logging.info('Starting')\n    return 42\n",
            true,
        )
        .expect("write AI app.py");
        repo.write_file(
            "utils.py",
            "def helper():\n    return 'one'\ndef helper_two():\n    return 'two'\n",
            true,
        )
        .expect("write AI utils.py");

        repo.trigger_checkpoint_with_ai("cursor", None, None)
            .expect("AI checkpoint for both files");

        repo.commit_with_message("AI feature work")
            .expect("feature commit");
        let original_head = repo.get_head_commit_sha().expect("feature sha");

        let mut initial_files = HashMap::new();
        initial_files.insert(
            "utils.py".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 4,
                author_id: "cursor".to_string(),
                overrode: None,
            }],
        );
        let mut prompts = HashMap::new();
        prompts.insert(
            "cursor".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "session-1".to_string(),
                    model: "test-model".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 4,
                total_deletions: 0,
                accepted_lines: 4,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E400".to_string()),
                    ("team".to_string(), "backend".to_string()),
                ])),
            },
        );
        let old_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&original_head)
            .unwrap();
        old_wl
            .write_initial_attributions(initial_files, prompts)
            .expect("write INITIAL for uncommitted utils.py");

        let pre_rebase_initial = old_wl.read_initial_attributions();
        assert_eq!(
            pre_rebase_initial.files.len(),
            1,
            "INITIAL should exist before rebase"
        );

        repo.switch_branch(&default_branch)
            .expect("switch to default");
        repo.write_file("README.md", "# Test Project\n", true)
            .expect("write upstream README");
        repo.commit_with_message("upstream: add README")
            .expect("upstream commit");
        let new_head = repo.get_head_commit_sha().expect("upstream sha");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite should succeed");

        let new_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap();
        let migrated = new_wl.read_initial_attributions();

        assert_eq!(
            migrated.files.len(),
            1,
            "INITIAL should be migrated to new HEAD after rebase"
        );
        assert!(
            migrated.files.contains_key("utils.py"),
            "utils.py should be in migrated INITIAL"
        );
        let utils_attrs = &migrated.files["utils.py"];
        assert_eq!(utils_attrs.len(), 1);
        assert_eq!(utils_attrs[0].start_line, 1);
        assert_eq!(utils_attrs[0].end_line, 4);
        assert_eq!(utils_attrs[0].author_id, "cursor");

        assert!(
            migrated.prompts.contains_key("cursor"),
            "cursor prompt record should be migrated"
        );
        assert!(
            !repo.gitai_repo().storage.has_working_log(&original_head),
            "old working log should not exist after rename"
        );
    }

    #[test]
    fn regression_initial_survives_amend_then_rebase() {
        let repo = TmpRepo::new().expect("create tmp repo");

        repo.write_file("app.py", "def main():\n    pass\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file(
            "app.py",
            "import logging\ndef main():\n    logging.info('v1')\n    return 1\n",
            true,
        )
        .expect("write feature v1");
        repo.commit_with_message("feature v1")
            .expect("commit feature v1");
        let v1_head = repo.get_head_commit_sha().expect("v1 sha");

        let mut initial_files = HashMap::new();
        initial_files.insert(
            "utils.py".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 8,
                author_id: "ai-cursor".to_string(),
                overrode: None,
            }],
        );
        let mut prompts = HashMap::new();
        prompts.insert(
            "ai-cursor".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess-amend".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 8,
                total_deletions: 0,
                accepted_lines: 8,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E400".to_string()),
                    ("team".to_string(), "backend".to_string()),
                ])),
            },
        );
        let v1_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&v1_head)
            .unwrap();
        v1_wl
            .write_initial_attributions(initial_files.clone(), prompts.clone())
            .expect("write INITIAL on v1");

        repo.write_file(
            "app.py",
            "import logging\ndef main():\n    logging.info('v2')\n    return 2\n",
            true,
        )
        .expect("write feature v2");
        let amend_sha = repo.amend_commit("feature v2").expect("amend commit");
        assert_ne!(v1_head, amend_sha, "amend should produce new SHA");

        let amend_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                v1_head.clone(),
                amend_sha.clone(),
                false,
                vec![v1_head.clone()],
                vec![amend_sha.clone()],
            ),
        };
        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &amend_event,
            "Test User".to_string(),
            &vec![amend_event.clone()],
            true,
        )
        .expect("amend rewrite should succeed");

        let amend_initial = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&amend_sha)
            .unwrap()
            .read_initial_attributions();
        assert_eq!(amend_initial.files.len(), 1, "INITIAL should survive amend");
        assert!(amend_initial.files.contains_key("utils.py"));

        repo.switch_branch(&default_branch)
            .expect("switch to default");
        repo.write_file("upstream.txt", "upstream change\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream commit")
            .expect("commit upstream");
        let rebase_new_head = repo.get_head_commit_sha().expect("rebase new head");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                amend_sha.clone(),
                rebase_new_head.clone(),
                false,
                vec![amend_sha.clone()],
                vec![rebase_new_head.clone()],
            ),
        };
        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rebase rewrite should succeed");

        let final_initial = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&rebase_new_head)
            .unwrap()
            .read_initial_attributions();
        assert_eq!(
            final_initial.files.len(),
            1,
            "INITIAL should survive amend + rebase"
        );
        assert!(final_initial.files.contains_key("utils.py"));
        let attrs = &final_initial.files["utils.py"];
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 8);
        assert_eq!(attrs[0].author_id, "ai-cursor");
        assert!(final_initial.prompts.contains_key("ai-cursor"));
    }

    #[test]
    fn regression_multi_tool_initial_with_disjoint_files_survives_rebase() {
        let repo = TmpRepo::new().expect("create tmp repo");

        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit")
            .expect("commit base");
        let default_branch = repo.current_branch().expect("default branch");

        repo.create_branch("feature").expect("create feature");
        repo.write_file("committed.py", "print('committed')\n", true)
            .expect("write committed");
        repo.commit_with_message("feature commit")
            .expect("commit feature");
        let original_head = repo.get_head_commit_sha().expect("feature sha");

        let mut initial_files = HashMap::new();
        initial_files.insert(
            "cursor_file.py".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 10,
                author_id: "ai-cursor".to_string(),
                overrode: None,
            }],
        );
        initial_files.insert(
            "copilot_file.py".to_string(),
            vec![
                LineAttribution {
                    start_line: 1,
                    end_line: 5,
                    author_id: "ai-copilot".to_string(),
                    overrode: None,
                },
                LineAttribution {
                    start_line: 10,
                    end_line: 15,
                    author_id: "ai-copilot".to_string(),
                    overrode: None,
                },
            ],
        );
        initial_files.insert(
            "shared_file.py".to_string(),
            vec![
                LineAttribution {
                    start_line: 1,
                    end_line: 3,
                    author_id: "ai-cursor".to_string(),
                    overrode: None,
                },
                LineAttribution {
                    start_line: 4,
                    end_line: 8,
                    author_id: "ai-copilot".to_string(),
                    overrode: None,
                },
            ],
        );

        let mut prompts = HashMap::new();
        prompts.insert(
            "ai-cursor".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess-cursor".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 13,
                total_deletions: 0,
                accepted_lines: 13,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E500".to_string()),
                    ("team".to_string(), "security".to_string()),
                ])),
            },
        );
        prompts.insert(
            "ai-copilot".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "copilot".to_string(),
                    id: "sess-copilot".to_string(),
                    model: "gpt-4o".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 16,
                total_deletions: 0,
                accepted_lines: 16,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: Some(HashMap::from([
                    ("employee_id".to_string(), "E500".to_string()),
                    ("team".to_string(), "security".to_string()),
                ])),
            },
        );

        let old_wl = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&original_head)
            .unwrap();
        old_wl
            .write_initial_attributions(initial_files, prompts)
            .expect("write multi-tool INITIAL");

        repo.switch_branch(&default_branch)
            .expect("switch to default");
        repo.write_file("upstream.txt", "upstream\n", true)
            .expect("write upstream");
        repo.commit_with_message("upstream commit")
            .expect("commit upstream");
        let new_head = repo.get_head_commit_sha().expect("new sha");

        let rebase_event = RewriteLogEvent::RebaseComplete {
            rebase_complete: RebaseCompleteEvent::new(
                original_head.clone(),
                new_head.clone(),
                false,
                vec![original_head.clone()],
                vec![new_head.clone()],
            ),
        };

        super::rewrite_authorship_if_needed(
            repo.gitai_repo(),
            &rebase_event,
            "Test User".to_string(),
            &vec![rebase_event.clone()],
            true,
        )
        .expect("rewrite should succeed");

        let migrated = repo
            .gitai_repo()
            .storage
            .working_log_for_base_commit(&new_head)
            .unwrap()
            .read_initial_attributions();

        assert_eq!(
            migrated.files.len(),
            3,
            "all three files should be migrated"
        );
        assert!(migrated.files.contains_key("cursor_file.py"));
        assert!(migrated.files.contains_key("copilot_file.py"));
        assert!(migrated.files.contains_key("shared_file.py"));

        let copilot_attrs = &migrated.files["copilot_file.py"];
        assert_eq!(
            copilot_attrs.len(),
            2,
            "copilot_file.py should have both attribution ranges"
        );
        assert_eq!(copilot_attrs[0].start_line, 1);
        assert_eq!(copilot_attrs[0].end_line, 5);
        assert_eq!(copilot_attrs[1].start_line, 10);
        assert_eq!(copilot_attrs[1].end_line, 15);

        let shared_attrs = &migrated.files["shared_file.py"];
        assert_eq!(
            shared_attrs.len(),
            2,
            "shared_file.py should have attributions from both tools"
        );

        assert_eq!(
            migrated.prompts.len(),
            2,
            "both prompt records should be migrated"
        );
        assert!(migrated.prompts.contains_key("ai-cursor"));
        assert!(migrated.prompts.contains_key("ai-copilot"));

        let cursor_prompt = &migrated.prompts["ai-cursor"];
        assert_eq!(cursor_prompt.agent_id.tool, "cursor");
        assert_eq!(cursor_prompt.total_additions, 13);

        let copilot_prompt = &migrated.prompts["ai-copilot"];
        assert_eq!(copilot_prompt.agent_id.tool, "copilot");
        assert_eq!(copilot_prompt.total_additions, 16);
    }

    /// Micro-benchmark comparing diff-based transfer vs char-level transform (old blame-based slow path).
    /// The char-level approach uses AttributionTracker::update_attributions + attributions_to_line_attributions.
    /// The diff-based approach uses diff_based_line_attribution_transfer (line-level diff only).
    ///
    /// Run with: cargo test --lib diff_based_transfer_benchmark -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diff_based_transfer_benchmark() {
        use crate::authorship::attribution_tracker::AttributionTracker;
        use std::time::Instant;

        let num_files = 20;
        let lines_per_file = 200;
        let num_commits = 100;

        println!("\n=== Diff-Based vs Char-Level Transform Benchmark ===");
        println!(
            "Files: {}, Lines/file: {}, Commits: {}",
            num_files, lines_per_file, num_commits
        );

        // Build initial file contents and both types of attributions
        let mut file_contents: Vec<String> = Vec::new();
        let mut line_attrs_per_file: Vec<Vec<LineAttribution>> = Vec::new();
        let mut char_attrs_per_file: Vec<Vec<Attribution>> = Vec::new();

        for file_idx in 0..num_files {
            let mut lines = Vec::new();
            let mut line_attrs = Vec::new();
            for line_idx in 0..lines_per_file {
                let content = format!("// AI code module {} line {}", file_idx, line_idx);
                let author = format!("ai-{}", line_idx % 3);
                lines.push(content);
                line_attrs.push(LineAttribution {
                    start_line: (line_idx + 1) as u32,
                    end_line: (line_idx + 1) as u32,
                    author_id: author,
                    overrode: None,
                });
            }
            let content = lines.join("\n") + "\n";

            // Build char-level attributions matching the line attributions
            let mut char_attrs = Vec::new();
            let mut char_pos = 0usize;
            for (line_idx, line) in content.lines().enumerate() {
                let line_end = char_pos + line.len() + 1; // +1 for newline
                char_attrs.push(Attribution::new(
                    char_pos,
                    line_end,
                    format!("ai-{}", line_idx % 3),
                    1,
                ));
                char_pos = line_end;
            }

            file_contents.push(content);
            line_attrs_per_file.push(line_attrs);
            char_attrs_per_file.push(char_attrs);
        }

        // Generate modified content per commit: insert 2 lines at top + modify 10% of lines
        let mut all_new_contents: Vec<Vec<String>> = Vec::new();
        let mut prev_contents = file_contents.clone();

        for commit_idx in 0..num_commits {
            let mut new_contents = Vec::new();
            for (file_idx, old_content) in prev_contents.iter().enumerate() {
                let old_lines: Vec<&str> = old_content.lines().collect();
                let mut new_lines: Vec<String> = Vec::new();
                if commit_idx == 0 {
                    // First commit: insert header lines (simulating main branch changes)
                    new_lines.push(format!("// Main header for module {}", file_idx));
                    new_lines.push("// Marker".to_string());
                }
                for (line_idx, line) in old_lines.iter().enumerate() {
                    if commit_idx == 0 && line_idx % 10 == 5 {
                        new_lines.push(format!("{} MODIFIED", line));
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
                new_contents.push(new_lines.join("\n") + "\n");
            }
            all_new_contents.push(new_contents.clone());
            prev_contents = new_contents;
        }

        // ===== Benchmark 1: Diff-based transfer (new approach) =====
        let start = Instant::now();
        let mut current_line_attrs = line_attrs_per_file.clone();
        let mut current_contents = file_contents.clone();
        for commit_contents in &all_new_contents {
            for file_idx in 0..num_files {
                let new_content = &commit_contents[file_idx];
                let old_content = &current_contents[file_idx];
                let old_attrs = &current_line_attrs[file_idx];
                let new_attrs = super::diff_based_line_attribution_transfer(
                    old_content,
                    new_content,
                    old_attrs,
                );
                current_line_attrs[file_idx] = new_attrs;
                current_contents[file_idx] = new_content.clone();
            }
        }
        let diff_based_duration = start.elapsed();
        let diff_total_attrs: usize = current_line_attrs.iter().map(|a| a.len()).sum();

        // ===== Benchmark 2: Char-level transform (old slow path) =====
        let tracker = AttributionTracker::new();
        let start = Instant::now();
        let mut current_char_attrs = char_attrs_per_file.clone();
        let mut current_contents2 = file_contents.clone();
        for commit_contents in &all_new_contents {
            for file_idx in 0..num_files {
                let new_content = &commit_contents[file_idx];
                let old_content = &current_contents2[file_idx];
                let old_attrs = &current_char_attrs[file_idx];
                let new_attrs = tracker
                    .update_attributions(old_content, new_content, old_attrs, "__DUMMY__", 1)
                    .unwrap();
                let line_attrs =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions(
                        &new_attrs,
                        new_content,
                    );
                current_char_attrs[file_idx] = new_attrs;
                current_contents2[file_idx] = new_content.clone();
                let _ = line_attrs; // used in real code for serialization
            }
        }
        let char_level_duration = start.elapsed();
        let char_total_attrs: usize = current_char_attrs.iter().map(|a| a.len()).sum();

        // ===== Benchmark 3: Full old slow path (char-level + VA wrapper + metrics + serialization) =====
        // This measures what the old slow path actually did per commit:
        // 1. Clone attributions into VA wrapper
        // 2. transform_changed_files_to_final_state (char-level diff)
        // 3. subtract/add prompt line metrics
        // 4. upsert_file_attestation per file
        // 5. Full serialization per commit
        let start = Instant::now();
        let mut full_slow_char_attrs = char_attrs_per_file.clone();
        let mut full_slow_contents = file_contents.clone();
        let mut full_slow_line_attrs = line_attrs_per_file.clone();
        for commit_contents in &all_new_contents {
            // Clone attributions (VA wrapper construction overhead)
            let _cloned_attrs: Vec<Vec<Attribution>> = full_slow_char_attrs.clone();
            let _cloned_contents: Vec<String> = full_slow_contents.clone();

            for file_idx in 0..num_files {
                let new_content = &commit_contents[file_idx];
                let old_content = &full_slow_contents[file_idx];
                let old_attrs = &full_slow_char_attrs[file_idx];

                // Step 1: char-level transform
                let new_attrs = tracker
                    .update_attributions(old_content, new_content, old_attrs, "__DUMMY__", 1)
                    .unwrap();
                // Step 2: convert to line attrs
                let line_attrs =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions(
                        &new_attrs,
                        new_content,
                    );
                // Step 3: serialize file attestation (old path did this per file per commit)
                let _serialized = super::build_file_attestation_from_line_attributions(
                    &format!("file_{}.rs", file_idx),
                    &line_attrs,
                );

                full_slow_char_attrs[file_idx] = new_attrs;
                full_slow_contents[file_idx] = new_content.clone();
                full_slow_line_attrs[file_idx] = line_attrs;
            }
        }
        let full_slow_duration = start.elapsed();

        // ===== Benchmark 4: Full new path (diff-based + fast serialization) =====
        let start = Instant::now();
        let mut full_fast_line_attrs = line_attrs_per_file.clone();
        let mut full_fast_contents = file_contents.clone();
        for commit_contents in &all_new_contents {
            for file_idx in 0..num_files {
                let new_content = &commit_contents[file_idx];
                let old_content = &full_fast_contents[file_idx];
                let old_attrs = &full_fast_line_attrs[file_idx];

                // Step 1: diff-based transfer
                let new_attrs = super::diff_based_line_attribution_transfer(
                    old_content,
                    new_content,
                    old_attrs,
                );
                // Step 2: serialize attestation from line attributions
                let _serialized = super::build_file_attestation_from_line_attributions(
                    &format!("file_{}.rs", file_idx),
                    &new_attrs,
                );

                full_fast_line_attrs[file_idx] = new_attrs;
                full_fast_contents[file_idx] = new_content.clone();
            }
        }
        let full_fast_duration = start.elapsed();

        let transform_speedup =
            char_level_duration.as_secs_f64() / diff_based_duration.as_secs_f64();
        let pipeline_speedup = full_slow_duration.as_secs_f64() / full_fast_duration.as_secs_f64();

        println!("\n--- Transform-Only Results ---");
        println!(
            "Diff-based transfer (new):    {:>8.1}ms  ({} line attrs)",
            diff_based_duration.as_secs_f64() * 1000.0,
            diff_total_attrs
        );
        println!(
            "Char-level transform (old):   {:>8.1}ms  ({} char attrs)",
            char_level_duration.as_secs_f64() * 1000.0,
            char_total_attrs
        );
        println!("Transform speedup:            {:>8.1}x", transform_speedup);

        println!("\n--- Full Pipeline Results (transform + serialization + overhead) ---");
        println!(
            "New pipeline (diff + serial):  {:>8.1}ms",
            full_fast_duration.as_secs_f64() * 1000.0
        );
        println!(
            "Old pipeline (char + VA + serial): {:>5.1}ms",
            full_slow_duration.as_secs_f64() * 1000.0
        );
        println!("Full pipeline speedup:         {:>8.1}x", pipeline_speedup);
        println!("===================================================\n");

        // The diff-based approach should be significantly faster than char-level transform.
        // In release mode with 200-line files we consistently see 3-4x improvement.
        assert!(
            pipeline_speedup >= 2.0,
            "Expected at least 2x pipeline speedup, got {:.1}x",
            pipeline_speedup
        );
    }

    /// Scaling benchmark: measures how diff-based vs char-level transform performance
    /// changes as file size increases from 50 to 5000 lines.
    ///
    /// Run with: cargo test --lib --release diff_based_transfer_scaling -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diff_based_transfer_scaling() {
        use crate::authorship::attribution_tracker::AttributionTracker;
        use std::time::Instant;

        let num_files = 5;
        let num_commits = 10;
        let file_sizes = [50, 100, 200, 500, 1000, 2000, 5000];

        println!("\n=== Scaling Benchmark: Diff-Based vs Char-Level ===");
        println!(
            "{:>8} {:>12} {:>12} {:>8}",
            "Lines", "Diff(ms)", "CharLvl(ms)", "Speedup"
        );
        println!("{}", "-".repeat(48));

        for &lines_per_file in &file_sizes {
            // Build initial content and attributions
            let mut file_contents = Vec::new();
            let mut line_attrs_per_file = Vec::new();
            let mut char_attrs_per_file = Vec::new();

            for file_idx in 0..num_files {
                let mut lines = Vec::new();
                let mut line_attrs = Vec::new();
                for line_idx in 0..lines_per_file {
                    lines.push(format!("// AI code module {} line {}", file_idx, line_idx));
                    line_attrs.push(LineAttribution {
                        start_line: (line_idx + 1) as u32,
                        end_line: (line_idx + 1) as u32,
                        author_id: format!("ai-{}", line_idx % 3),
                        overrode: None,
                    });
                }
                let content = lines.join("\n") + "\n";
                let mut char_attrs = Vec::new();
                let mut pos = 0usize;
                for (li, line) in content.lines().enumerate() {
                    let end = pos + line.len() + 1;
                    char_attrs.push(Attribution::new(pos, end, format!("ai-{}", li % 3), 1));
                    pos = end;
                }
                file_contents.push(content);
                line_attrs_per_file.push(line_attrs);
                char_attrs_per_file.push(char_attrs);
            }

            // Generate modified content: insert 5 lines + modify 10%
            let mut all_new = Vec::new();
            let mut prev = file_contents.clone();
            for ci in 0..num_commits {
                let mut new_batch = Vec::new();
                for (fi, _) in prev.iter().enumerate() {
                    let old_lines: Vec<&str> = prev[fi].lines().collect();
                    let mut new_lines = Vec::new();
                    if ci == 0 {
                        for h in 0..5 {
                            new_lines.push(format!("// Header {} mod {}", h, fi));
                        }
                    }
                    for (li, line) in old_lines.iter().enumerate() {
                        if ci == 0 && li % 10 == 5 {
                            new_lines.push(format!("{} MOD", line));
                        } else {
                            new_lines.push(line.to_string());
                        }
                    }
                    new_batch.push(new_lines.join("\n") + "\n");
                }
                all_new.push(new_batch.clone());
                prev = new_batch;
            }

            // Benchmark diff-based
            let start = Instant::now();
            let mut cur_la = line_attrs_per_file.clone();
            let mut cur_c = file_contents.clone();
            for commit_contents in &all_new {
                for fi in 0..num_files {
                    let na = super::diff_based_line_attribution_transfer(
                        &cur_c[fi],
                        &commit_contents[fi],
                        &cur_la[fi],
                    );
                    cur_la[fi] = na;
                    cur_c[fi] = commit_contents[fi].clone();
                }
            }
            let diff_ms = start.elapsed().as_secs_f64() * 1000.0;

            // Benchmark char-level
            let tracker = AttributionTracker::new();
            let start = Instant::now();
            let mut cur_ca = char_attrs_per_file.clone();
            let mut cur_c2 = file_contents.clone();
            for commit_contents in &all_new {
                for fi in 0..num_files {
                    let na = tracker
                        .update_attributions(
                            &cur_c2[fi],
                            &commit_contents[fi],
                            &cur_ca[fi],
                            "__DUMMY__",
                            1,
                        )
                        .unwrap();
                    let _la =
                        crate::authorship::attribution_tracker::attributions_to_line_attributions(
                            &na,
                            &commit_contents[fi],
                        );
                    cur_ca[fi] = na;
                    cur_c2[fi] = commit_contents[fi].clone();
                }
            }
            let char_ms = start.elapsed().as_secs_f64() * 1000.0;

            let speedup = char_ms / diff_ms;
            println!(
                "{:>8} {:>12.1} {:>12.1} {:>8.1}x",
                lines_per_file, diff_ms, char_ms, speedup
            );
        }
        println!("===================================================\n");
    }

    #[test]
    fn diff_based_transfer_equal_content() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline2\nline3\n";
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai-b".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
        ];
        let result = super::diff_based_line_attribution_transfer(old, new, &attrs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].author_id, "ai-a");
        assert_eq!(result[1].author_id, "ai-b");
        assert_eq!(result[2].author_id, "ai-a");
    }

    #[test]
    fn diff_based_transfer_insertion_shifts_lines() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nnew_line\nline2\nline3\n";
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai-b".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
        ];
        let result = super::diff_based_line_attribution_transfer(old, new, &attrs);
        // line1 kept (line 1), new_line inserted (line 2, no attr), line2 kept (line 3), line3 kept (line 4)
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[0].author_id, "ai-a");
        assert_eq!(result[1].start_line, 3); // shifted from line 2 to line 3
        assert_eq!(result[1].author_id, "ai-b");
        assert_eq!(result[2].start_line, 4); // shifted from line 3 to line 4
        assert_eq!(result[2].author_id, "ai-a");
    }

    #[test]
    fn diff_based_transfer_deletion_removes_line() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline3\n";
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai-b".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
        ];
        let result = super::diff_based_line_attribution_transfer(old, new, &attrs);
        // line1 kept (line 1), line2 deleted, line3 kept (line 2)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[0].author_id, "ai-a");
        assert_eq!(result[1].start_line, 2);
        assert_eq!(result[1].author_id, "ai-a");
    }

    #[test]
    fn diff_based_transfer_replacement_drops_attribution() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\n";
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai-b".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
        ];
        let result = super::diff_based_line_attribution_transfer(old, new, &attrs);
        // line1 kept (line 1), line2 replaced by "modified" (line 2, no attr), line3 kept (line 3)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[0].author_id, "ai-a");
        assert_eq!(result[1].start_line, 3);
        assert_eq!(result[1].author_id, "ai-a");
    }

    #[test]
    fn diff_based_transfer_handles_duplicate_lines_correctly() {
        // This tests the case that the old content-matching approach got wrong:
        // identical lines from different authors should be tracked by position, not content
        let old = "let x = 42;\nlet y = 0;\nlet x = 42;\n";
        let new = "let x = 42;\nlet z = 1;\nlet y = 0;\nlet x = 42;\n";
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai-b".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai-c".to_string(),
                overrode: None,
            },
        ];
        let result = super::diff_based_line_attribution_transfer(old, new, &attrs);
        // line "let x = 42;" (1) kept as line 1 (ai-a)
        // "let z = 1;" inserted (line 2, no attr)
        // "let y = 0;" kept (line 3, ai-b)
        // "let x = 42;" (3) kept as line 4 (ai-c) — NOT ai-a!
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[0].author_id, "ai-a");
        assert_eq!(result[1].start_line, 3);
        assert_eq!(result[1].author_id, "ai-b");
        assert_eq!(result[2].start_line, 4);
        assert_eq!(result[2].author_id, "ai-c");
    }
}
