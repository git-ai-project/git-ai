use std::collections::{BTreeMap, HashMap, HashSet};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::git::authorship_traversal::{
    commits_have_authorship_notes, load_ai_touched_files_for_commits,
};
use crate::git::notes_api::{
    read_authorship_v3 as get_reference_as_authorship_log_v3, read_note as show_authorship_note,
    write_note as notes_add,
};
use crate::git::range_diff::{CommitMapping, MappingKind, run_range_diff};
use crate::git::repository::{CommitRange, Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;

/// The only input a rewrite operation needs.
#[derive(Debug, Clone)]
pub struct RewriteTriple {
    pub onto: String,
    pub original_head: String,
    pub new_head: String,
}

/// Unified handler for ALL rewrite operations.
///
/// Single entry point for rebase, cherry-pick, commit amend, and merge squash rewrites.
///
/// Algorithm:
/// 1. `git range-diff onto..original onto..new` -> commit mappings
/// 2. For each mapping:
///    - Identical: copy note (with base_commit_sha remapped)
///    - Modified: load old note -> diff file contents -> transfer attributions -> write new note
///    - Deleted: no-op
///    - Added: no-op (new commits get attribution via normal post-commit flow)
/// 3. Migrate working log from original_head to new_head
pub fn handle_rewrite_op_v3(repo: &Repository, triple: &RewriteTriple) -> Result<(), GitAiError> {
    if triple.original_head == triple.new_head {
        return Ok(());
    }

    let mappings = run_range_diff(repo, &triple.onto, &triple.original_head, &triple.new_head)?;

    if mappings.is_empty() {
        return Ok(());
    }

    // Fetch any missing remote notes for original commits
    let original_commits: Vec<String> =
        mappings.iter().filter_map(|m| m.original.clone()).collect();
    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &original_commits);

    // Detect squash groups (N Deleted + 1 Modified/Added)
    let squash_groups = detect_squash_groups(&mappings);

    for mapping in &mappings {
        if mapping.kind == MappingKind::Deleted {
            continue;
        }

        match mapping.kind {
            MappingKind::Identical => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    copy_note(repo, original, new)?;
                }
            }
            MappingKind::Modified => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    if let Some(group) = squash_groups.get(new.as_str()) {
                        transfer_attribution_squash(repo, &group.all_originals(), new)?;
                    } else {
                        let empty_map = HashMap::new();
                        transfer_attribution_via_diff(repo, original, new, &empty_map)?;
                    }
                }
            }
            MappingKind::Added => {
                if let Some(new) = &mapping.new
                    && let Some(group) = squash_groups.get(new.as_str())
                {
                    transfer_attribution_squash(repo, &group.all_originals(), new)?;
                }
            }
            MappingKind::Deleted => unreachable!(),
        }
    }

    migrate_working_log(repo, &triple.original_head, &triple.new_head)?;

    Ok(())
}

/// Unified entry point for all rewrite authorship operations.
/// Routes each event type to the appropriate handler.
pub fn handle_rewrite_from_event(
    repo: &Repository,
    event: &RewriteLogEvent,
    author: Option<String>,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    match event {
        RewriteLogEvent::RebaseComplete { rebase_complete } => {
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &rebase_complete.original_commits,
            );
            let pairs = pair_commits_for_rewrite(
                repo,
                &rebase_complete.original_commits,
                &rebase_complete.new_commits,
            );
            let is_squash =
                rebase_complete.original_commits.len() > rebase_complete.new_commits.len();
            if is_squash && rebase_complete.new_commits.len() == 1 {
                transfer_attribution_squash(
                    repo,
                    &rebase_complete.original_commits,
                    &rebase_complete.new_commits[0],
                )?;
            } else {
                let content_author_map = build_content_author_map(
                    repo,
                    &rebase_complete.original_commits,
                    &rebase_complete.original_head,
                );
                for (original, new) in &pairs {
                    transfer_attribution_via_diff(repo, original, new, &content_author_map)?;
                }
            }
            migrate_working_log(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.new_head,
            )?;
            Ok(())
        }
        RewriteLogEvent::CherryPickComplete {
            cherry_pick_complete,
        } => {
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &cherry_pick_complete.source_commits,
            );
            let pairs = pair_commits_for_rewrite(
                repo,
                &cherry_pick_complete.source_commits,
                &cherry_pick_complete.new_commits,
            );
            let empty_map = HashMap::new();
            for (source, new) in &pairs {
                transfer_attribution_via_diff(repo, source, new, &empty_map)?;
            }
            migrate_working_log(
                repo,
                &cherry_pick_complete.original_head,
                &cherry_pick_complete.new_head,
            )?;
            Ok(())
        }
        RewriteLogEvent::CommitAmend { commit_amend } => {
            rewrite_authorship_after_commit_amend_with_snapshot(
                repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
                author.unwrap_or_default(),
                final_state_override,
            )?;
            Ok(())
        }
        RewriteLogEvent::MergeSquash { merge_squash } => {
            let current_head = repo
                .head()
                .ok()
                .and_then(|head| head.target().ok())
                .map(|oid| oid.to_string());
            if current_head.as_deref() != Some(merge_squash.base_head.as_str()) {
                tracing::debug!(
                    "Skipping merge --squash pre-commit prep because repo head already advanced past {}",
                    merge_squash.base_head
                );
                return Ok(());
            }
            repo.storage
                .delete_working_log_for_base_commit(&merge_squash.base_head)?;
            if merge_squash.staged_file_blobs.is_empty() {
                tracing::debug!(
                    "Skipping immediate merge --squash pre-commit prep for {} because no staged snapshot was captured; commit replay will reconstruct from the committed final state",
                    merge_squash.base_head
                );
                return Ok(());
            }
            prepare_working_log_after_squash(
                repo,
                &merge_squash.source_head,
                &merge_squash.base_head,
                &merge_squash.staged_file_blobs,
                &author.unwrap_or_default(),
            )?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Pair original commits to new commits using subject-line matching for unequal lengths.
fn pair_commits_for_rewrite(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Vec<(String, String)> {
    if original_commits.len() == new_commits.len() {
        return original_commits
            .iter()
            .zip(new_commits.iter())
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
    }

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
    let mut pairs = Vec::with_capacity(new_commits.len());

    for new_sha in new_commits {
        let new_subject = repo
            .find_commit(new_sha.clone())
            .and_then(|c| c.summary())
            .unwrap_or_default();

        let matched = original_subjects.iter().find(|(orig_sha, orig_subject)| {
            !used.contains(orig_sha) && *orig_subject == new_subject
        });

        let orig_sha = if let Some((orig_sha, _)) = matched {
            orig_sha.clone()
        } else {
            match original_subjects
                .iter()
                .find(|(orig_sha, _)| !used.contains(orig_sha))
            {
                Some((orig_sha, _)) => orig_sha.clone(),
                None => continue,
            }
        };

        used.insert(orig_sha.clone());
        pairs.push((orig_sha, new_sha.clone()));
    }

    pairs
}

/// Copy an authorship note from one commit to another, remapping base_commit_sha.
fn copy_note(repo: &Repository, from_sha: &str, to_sha: &str) -> Result<(), GitAiError> {
    let note_content = match show_authorship_note(repo, from_sha) {
        Some(content) => content,
        None => return Ok(()),
    };
    let remapped = remap_base_commit_sha(&note_content, to_sha);
    notes_add(repo, to_sha, &remapped)?;
    Ok(())
}

/// Transfer attribution from an original commit to a modified commit using diff-based hunk transfer.
/// `content_author_map` maps file_path → (line_content → author_id) for content-matching
/// fallback when lines exist in the new commit that weren't in the original note.
fn transfer_attribution_via_diff(
    repo: &Repository,
    original_sha: &str,
    new_sha: &str,
    content_author_map: &HashMap<String, HashMap<String, String>>,
) -> Result<(), GitAiError> {
    // Skip if the new commit already has an authorship note (e.g. it was already
    // processed via post-commit). This prevents a mis-mapped rebase event from
    // overwriting a valid note with data from an unrelated original commit.
    if show_authorship_note(repo, new_sha).is_some() {
        return Ok(());
    }

    let note_content = match show_authorship_note(repo, original_sha) {
        Some(content) => content,
        None => return Ok(()),
    };

    let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
        Ok(log) => log,
        Err(_) => return Ok(()),
    };

    let attested_files: Vec<String> = authorship_log
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    if attested_files.is_empty() {
        // Metadata-only note — copy with remapped base_commit_sha
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        notes_add(repo, new_sha, &remapped)?;
        return Ok(());
    }

    // Read file contents at both commits in a single batch call each
    let old_contents = batch_cat_file(repo, original_sha, &attested_files)?;
    let new_contents = batch_cat_file(repo, new_sha, &attested_files)?;

    let mut new_log = AuthorshipLog::new();
    new_log.metadata.base_commit_sha = new_sha.to_string();
    new_log.metadata.prompts = authorship_log.metadata.prompts.clone();
    new_log.metadata.sessions = authorship_log.metadata.sessions.clone();
    // humans are scoped per-commit: only include if attestation entries reference h_-prefixed IDs
    // (populated after we process attestations below)

    for file_attestation in &authorship_log.attestations {
        let file_path = &file_attestation.file_path;
        let old_content = match old_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };
        let new_content = match new_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };

        let old_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
        if old_attrs.is_empty() {
            continue;
        }

        let mut transferred =
            diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // Content-matching fallback: fill in lines from the combined content→author map
        // that weren't covered by the diff transfer (lines attributed by earlier commits
        // in the chain whose notes use per-commit-delta scoping).
        if let Some(file_map) = content_author_map.get(file_path) {
            let covered: HashSet<u32> = transferred
                .iter()
                .flat_map(|a| a.start_line..=a.end_line)
                .collect();
            for (line_idx, line_content) in new_content.lines().enumerate() {
                let line_num = (line_idx + 1) as u32;
                if covered.contains(&line_num) {
                    continue;
                }
                if let Some(author_id) = file_map.get(line_content) {
                    transferred.push(LineAttribution {
                        start_line: line_num,
                        end_line: line_num,
                        author_id: author_id.clone(),
                        overrode: None,
                    });
                }
            }
            transferred.sort_by_key(|a| a.start_line);
        }

        if let Some(new_attestation) =
            build_file_attestation_from_line_attributions(file_path, &transferred)
        {
            new_log.attestations.push(new_attestation);
        }
    }

    // Populate humans metadata from two sources:
    // 1. h_-prefixed author IDs in transferred attestation entries
    for attestation in &new_log.attestations {
        for entry in &attestation.entries {
            if entry.hash.starts_with("h_")
                && let Some(record) = authorship_log.metadata.humans.get(&entry.hash)
            {
                new_log
                    .metadata
                    .humans
                    .entry(entry.hash.clone())
                    .or_insert_with(|| record.clone());
            }
        }
    }
    // 2. KnownHuman checkpoints in the working log at the new commit's parent
    //    (written during conflict resolution via `rebase --continue`)
    populate_humans_from_working_log(repo, new_sha, &mut new_log);

    // Conflict resolution fallback: if diff-based transfer produced no attestations,
    // check the working log at the new commit's parent for AI checkpoint data written
    // during `rebase --continue` conflict resolution.
    if new_log.attestations.is_empty() {
        if let Some(conflict_note) = build_note_from_conflict_wl(repo, new_sha) {
            notes_add(repo, new_sha, &conflict_note)?;
            return Ok(());
        }
        // No conflict working log either — remap the original note as a last resort.
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        notes_add(repo, new_sha, &remapped)?;
        return Ok(());
    }

    let serialized = new_log
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    notes_add(repo, new_sha, &serialized)?;

    Ok(())
}

/// Transfer attribution from multiple original commits into a single squash result.
/// Uses sequential replay: processes original commits in order, transferring accumulated
/// attributions through each diff step, then overlaying each commit's per-delta note.
/// This matches the old code's hunk-based replay semantics.
fn transfer_attribution_squash(
    repo: &Repository,
    originals: &[String],
    new_sha: &str,
) -> Result<(), GitAiError> {
    if originals.is_empty() {
        return Ok(());
    }

    // Collect all files mentioned in any original note.
    let mut all_files: HashSet<String> = HashSet::new();
    let mut base_log: Option<AuthorshipLog> = None;
    let mut parsed_notes: Vec<(String, AuthorshipLog)> = Vec::new();

    for original_sha in originals {
        let note_content = match show_authorship_note(repo, original_sha) {
            Some(content) => content,
            None => continue,
        };
        let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
            Ok(log) => log,
            Err(_) => continue,
        };

        if base_log.is_none() {
            base_log = Some(authorship_log.clone());
        } else if let Some(ref mut base) = base_log {
            for (k, v) in &authorship_log.metadata.prompts {
                base.metadata.prompts.entry(k.clone()).or_insert(v.clone());
            }
            for (k, v) in &authorship_log.metadata.humans {
                base.metadata.humans.entry(k.clone()).or_insert(v.clone());
            }
            for (k, v) in &authorship_log.metadata.sessions {
                base.metadata.sessions.entry(k.clone()).or_insert(v.clone());
            }
        }

        for att in &authorship_log.attestations {
            all_files.insert(att.file_path.clone());
        }
        parsed_notes.push((original_sha.clone(), authorship_log));
    }

    let Some(mut base) = base_log else {
        return Ok(());
    };

    let all_files_vec: Vec<String> = all_files.iter().cloned().collect();

    // Sequential replay: accumulate attributions by diffing consecutive commits.
    // For each file, maintain current attributions and content, then:
    // 1. Diff previous content → current commit content → transfer attrs
    // 2. Overlay the current commit's note attrs on top
    let mut accumulated_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prev_contents: HashMap<String, String> = HashMap::new();

    for (i, (original_sha, authorship_log)) in parsed_notes.iter().enumerate() {
        let current_contents = batch_cat_file(repo, original_sha, &all_files_vec)?;

        if i > 0 {
            // Diff previous content → current content and transfer accumulated attrs
            for file_path in &all_files_vec {
                let prev_content = prev_contents.get(file_path).map(String::as_str);
                let curr_content = current_contents.get(file_path).map(String::as_str);

                if let (Some(prev_c), Some(curr_c)) = (prev_content, curr_content)
                    && !prev_c.is_empty()
                    && !curr_c.is_empty()
                    && prev_c != curr_c
                    && let Some(attrs) = accumulated_attrs.get(file_path)
                    && !attrs.is_empty()
                {
                    let transferred = diff_based_line_attribution_transfer(prev_c, curr_c, attrs);
                    accumulated_attrs.insert(file_path.clone(), transferred);
                }
            }
        }

        // Overlay this commit's note attributions
        for file_attestation in &authorship_log.attestations {
            let file_path = &file_attestation.file_path;
            let note_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
            if note_attrs.is_empty() {
                continue;
            }
            let entry = accumulated_attrs.entry(file_path.clone()).or_default();
            for attr in note_attrs {
                overlay_attribution(entry, attr.start_line, attr.end_line, attr.author_id);
            }
        }

        prev_contents = current_contents;
    }

    // Finally, diff the last original's content → squash result content
    let final_contents = batch_cat_file(repo, new_sha, &all_files_vec)?;
    for file_path in &all_files_vec {
        let prev_content = prev_contents.get(file_path).map(String::as_str);
        let final_content = final_contents.get(file_path).map(String::as_str);

        if let (Some(prev_c), Some(final_c)) = (prev_content, final_content)
            && !prev_c.is_empty()
            && !final_c.is_empty()
            && prev_c != final_c
            && let Some(attrs) = accumulated_attrs.get(file_path)
            && !attrs.is_empty()
        {
            let transferred = diff_based_line_attribution_transfer(prev_c, final_c, attrs);
            accumulated_attrs.insert(file_path.clone(), transferred);
        }
    }

    base.metadata.base_commit_sha = new_sha.to_string();
    base.attestations.clear();

    for (file_path, mut attrs) in accumulated_attrs {
        attrs.sort_by_key(|a| a.start_line);
        if let Some(attestation) = build_file_attestation_from_line_attributions(&file_path, &attrs)
        {
            base.attestations.push(attestation);
        }
    }

    let serialized = base
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    notes_add(repo, new_sha, &serialized)?;

    Ok(())
}

/// Convert attestation entries (hash + line ranges) into flat LineAttribution list.
fn attestation_entries_to_line_attributions(
    entries: &[crate::authorship::authorship_log_serialization::AttestationEntry],
) -> Vec<LineAttribution> {
    let mut attrs = Vec::new();
    for entry in entries {
        for range in &entry.line_ranges {
            let (start, end) = match range {
                LineRange::Single(l) => (*l, *l),
                LineRange::Range(s, e) => (*s, *e),
            };
            attrs.push(LineAttribution {
                start_line: start,
                end_line: end,
                author_id: entry.hash.clone(),
                overrode: None,
            });
        }
    }
    attrs
}

/// Overlay a new attribution range onto existing attributions, splitting partial overlaps.
fn overlay_attribution(attrs: &mut Vec<LineAttribution>, start: u32, end: u32, author_id: String) {
    let mut i = 0;
    let mut to_insert_after: Vec<LineAttribution> = Vec::new();
    while i < attrs.len() {
        let a = &attrs[i];
        if a.end_line < start || a.start_line > end {
            i += 1;
            continue;
        }
        let removed = attrs.remove(i);
        if removed.start_line < start {
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
            to_insert_after.push(LineAttribution {
                start_line: end + 1,
                end_line: removed.end_line,
                author_id: removed.author_id,
                overrode: removed.overrode,
            });
        }
    }
    for frag in to_insert_after {
        attrs.push(frag);
    }
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id,
        overrode: None,
    });
}

/// Batch read file contents at a specific commit.
fn batch_cat_file(
    repo: &Repository,
    commit_sha: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

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

/// Remap the base_commit_sha field in a serialized note to a new target commit.
fn remap_base_commit_sha(note_content: &str, target_commit: &str) -> String {
    // Fast path: direct string replacement of the JSON field value
    let field = "\"base_commit_sha\"";
    let Some(field_pos) = note_content.find(field) else {
        return note_content.to_string();
    };
    let bytes = note_content.as_bytes();

    let mut pos = field_pos + field.len();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return note_content.to_string();
    }
    pos += 1;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return note_content.to_string();
    }
    pos += 1;
    let value_start = pos;

    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 2;
            }
            b'"' => {
                let mut remapped = String::with_capacity(
                    note_content.len() - (pos - value_start) + target_commit.len(),
                );
                remapped.push_str(&note_content[..value_start]);
                remapped.push_str(target_commit);
                remapped.push_str(&note_content[pos..]);
                return remapped;
            }
            _ => {
                pos += 1;
            }
        }
    }

    note_content.to_string()
}

struct SquashGroup {
    matched_original: Option<String>,
    deleted_originals: Vec<String>,
}

impl SquashGroup {
    fn all_originals(&self) -> Vec<String> {
        let mut all = self.deleted_originals.clone();
        if let Some(matched) = &self.matched_original {
            all.push(matched.clone());
        }
        all
    }
}

/// Detect squash patterns: consecutive Deleted commits followed by a Modified/Added.
fn detect_squash_groups(mappings: &[CommitMapping]) -> HashMap<&str, SquashGroup> {
    let mut groups: HashMap<&str, SquashGroup> = HashMap::new();
    let mut pending_deleted: Vec<String> = Vec::new();

    for mapping in mappings {
        match mapping.kind {
            MappingKind::Deleted => {
                if let Some(original) = &mapping.original {
                    pending_deleted.push(original.clone());
                }
            }
            MappingKind::Modified | MappingKind::Added => {
                if !pending_deleted.is_empty()
                    && let Some(new) = &mapping.new
                {
                    groups.insert(
                        new.as_str(),
                        SquashGroup {
                            matched_original: mapping.original.clone(),
                            deleted_originals: std::mem::take(&mut pending_deleted),
                        },
                    );
                }
                pending_deleted.clear();
            }
            MappingKind::Identical => {
                pending_deleted.clear();
            }
        }
    }
    groups
}

/// Build a combined content→author map from all original commits' notes.
/// Maps file_path → (line_content → author_id) based on the file content at `original_head`.
/// This enables content-matching for lines attributed by earlier commits in the chain.
fn build_content_author_map(
    repo: &Repository,
    original_commits: &[String],
    original_head: &str,
) -> HashMap<String, HashMap<String, String>> {
    let mut all_file_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();

    for sha in original_commits {
        let note = match show_authorship_note(repo, sha) {
            Some(n) => n,
            None => continue,
        };
        let log = match AuthorshipLog::deserialize_from_string(&note) {
            Ok(l) => l,
            Err(_) => continue,
        };
        for attestation in &log.attestations {
            let attrs = attestation_entries_to_line_attributions(&attestation.entries);
            all_file_attrs
                .entry(attestation.file_path.clone())
                .or_default()
                .extend(attrs);
        }
    }

    if all_file_attrs.is_empty() {
        return HashMap::new();
    }

    let file_paths: Vec<String> = all_file_attrs.keys().cloned().collect();
    let head_contents = batch_cat_file(repo, original_head, &file_paths).unwrap_or_default();

    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (file_path, attrs) in &all_file_attrs {
        let content = match head_contents.get(file_path) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut line_map: HashMap<String, String> = HashMap::new();
        for attr in attrs {
            for line_num in attr.start_line..=attr.end_line {
                let idx = line_num.saturating_sub(1) as usize;
                if let Some(line_content) = lines.get(idx)
                    && !line_content.is_empty()
                {
                    line_map
                        .entry(line_content.to_string())
                        .or_insert_with(|| attr.author_id.clone());
                }
            }
        }
        if !line_map.is_empty() {
            result.insert(file_path.clone(), line_map);
        }
    }

    result
}

/// Populate humans metadata from the working log at the new commit's parent SHA.
/// This handles the case where KnownHuman checkpoints were written during conflict
/// resolution (rebase --continue) and need to appear in the note's metadata.
fn populate_humans_from_working_log(repo: &Repository, new_sha: &str, log: &mut AuthorshipLog) {
    let parent_sha = match repo
        .find_commit(new_sha.to_string())
        .ok()
        .and_then(|c| c.parent(0).ok())
    {
        Some(p) => p.id(),
        None => return,
    };

    let working_log = match repo.storage.working_log_for_base_commit(&parent_sha) {
        Ok(wl) => wl,
        Err(_) => return,
    };
    let checkpoints = match working_log.read_all_checkpoints() {
        Ok(cps) => cps,
        Err(_) => return,
    };

    let changed_files = get_changed_files_in_commit(repo, new_sha);

    for checkpoint in &checkpoints {
        if checkpoint.kind != CheckpointKind::KnownHuman {
            continue;
        }
        // Only include if checkpoint covers files that changed in this commit
        if !checkpoint
            .entries
            .iter()
            .any(|e| changed_files.contains(&e.file))
        {
            continue;
        }
        let hash = crate::authorship::authorship_log_serialization::generate_human_short_hash(
            &checkpoint.author,
        );
        log.metadata.humans.entry(hash).or_insert_with(|| {
            crate::authorship::authorship_log::HumanRecord {
                author: checkpoint.author.clone(),
            }
        });
    }
}

/// Get the list of files changed in a commit (vs its first parent).
fn get_changed_files_in_commit(repo: &Repository, commit_sha: &str) -> HashSet<String> {
    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--no-commit-id".to_string());
    args.push("-r".to_string());
    args.push("--name-only".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        Err(_) => HashSet::new(),
    }
}

/// Build an authorship note from working-log checkpoint data written during conflict resolution.
///
/// When an AI resolves a rebase conflict (via `git-ai checkpoint` during `rebase --continue`),
/// the diff-based transfer can't carry attribution because the content differs from the original.
/// This fallback reads the working log at the new commit's parent SHA and builds a note from
/// the AI checkpoint entries for the changed files.
fn build_note_from_conflict_wl(repo: &Repository, new_commit: &str) -> Option<String> {
    use crate::authorship::authorship_log_serialization::generate_short_hash;

    let parent_sha = repo
        .find_commit(new_commit.to_string())
        .ok()?
        .parent(0)
        .ok()?
        .id();

    let working_log = repo.storage.working_log_for_base_commit(&parent_sha).ok()?;
    let checkpoints = working_log.read_all_checkpoints().ok()?;

    let changed_files = get_changed_files_in_commit(repo, new_commit);
    if changed_files.is_empty() {
        return None;
    }

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = new_commit.to_string();

    let mut file_line_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut has_ai_content = false;

    for checkpoint in &checkpoints {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }

        if checkpoint.kind == CheckpointKind::KnownHuman {
            let hash = crate::authorship::authorship_log_serialization::generate_human_short_hash(
                &checkpoint.author,
            );
            authorship_log
                .metadata
                .humans
                .entry(hash)
                .or_insert_with(|| crate::authorship::authorship_log::HumanRecord {
                    author: checkpoint.author.clone(),
                });
            continue;
        }

        let agent_id = match &checkpoint.agent_id {
            Some(id) => id,
            None => continue,
        };

        if checkpoint.trace_id.is_some() {
            let session_id = crate::authorship::authorship_log_serialization::generate_session_id(
                &agent_id.id,
                &agent_id.tool,
            );
            authorship_log
                .metadata
                .sessions
                .entry(session_id)
                .or_insert_with(|| crate::authorship::authorship_log::SessionRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    custom_attributes: None,
                });
        } else {
            let author_id = generate_short_hash(&agent_id.id, &agent_id.tool);
            authorship_log
                .metadata
                .prompts
                .entry(author_id)
                .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    total_additions: checkpoint.line_stats.additions,
                    total_deletions: checkpoint.line_stats.deletions,
                    accepted_lines: 0,
                    overriden_lines: 0,
                    custom_attributes: None,
                    messages_url: None,
                });
        }

        for entry in &checkpoint.entries {
            if !changed_files.contains(&entry.file) {
                continue;
            }
            if entry.line_attributions.is_empty() {
                continue;
            }
            file_line_attrs
                .entry(entry.file.clone())
                .or_default()
                .extend(entry.line_attributions.iter().cloned());
        }
    }

    let mut accepted_per_author: HashMap<String, u32> = HashMap::new();
    for (file_path, line_attrs) in &file_line_attrs {
        for la in line_attrs {
            *accepted_per_author.entry(la.author_id.clone()).or_insert(0) +=
                la.end_line - la.start_line + 1;
        }
        if let Some(file_att) = build_file_attestation_from_line_attributions(file_path, line_attrs)
        {
            authorship_log.attestations.push(file_att);
            has_ai_content = true;
        }
    }

    for (author_id, count) in accepted_per_author {
        if let Some(record) = authorship_log.metadata.prompts.get_mut(&author_id) {
            record.accepted_lines = count;
        }
    }

    if !has_ai_content {
        return None;
    }

    authorship_log.serialize_to_string().ok()
}

/// Migrate working log from original_head SHA to new_head SHA.
fn migrate_working_log(
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
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
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
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
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
    if initial_attributions.files.is_empty()
        && initial_attributions.prompts.is_empty()
        && initial_attributions.sessions.is_empty()
    {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(new_head)?;
    working_log.write_initial_attributions_with_contents(
        initial_attributions.files,
        initial_attributions.prompts,
        initial_attributions.humans,
        final_state,
        initial_attributions.sessions,
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

    tracing::debug!(
        "Rewriting authorship for squash/rebase merge: {} -> {}",
        source_head_sha,
        merge_commit_sha
    );

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
            tracing::debug!(
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
                notes_add(repo, merge_commit_sha, &authorship_json)?;
            }
        } else {
            // No files changed, nothing to do
            tracing::debug!("No files changed in merge, skipping authorship rewrite");
        }
        return Ok(());
    }

    tracing::debug!(
        "Processing {} changed files for merge authorship",
        changed_files.len()
    );

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

    tracing::debug!(
        "Read {} committed files from merge commit",
        committed_files.len()
    );

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
            for (hash, record) in log.metadata.humans {
                authorship_log.metadata.humans.entry(hash).or_insert(record);
            }
            for (id, record) in log.metadata.sessions {
                authorship_log.metadata.sessions.entry(id).or_insert(record);
            }
        }
    }

    for (prompt_id, record) in authorship_log.metadata.prompts.iter_mut() {
        if let Some((additions, deletions)) = summed_totals.get(prompt_id) {
            record.total_additions = *additions;
            record.total_deletions = *deletions;
        }
    }

    tracing::debug!(
        "Created authorship log with {} attestations, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    );

    // Step 7: Save authorship log to git notes
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    notes_add(repo, merge_commit_sha, &authorship_json)?;

    tracing::debug!(
        "✓ Saved authorship log for merge commit {}",
        merge_commit_sha
    );

    Ok(())
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

fn is_zero_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

fn is_blob_mode(mode: &str) -> bool {
    mode.starts_with("100") || mode == "120000"
}

#[doc(hidden)]
pub fn collect_changed_file_contents_from_diff(
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

#[doc(hidden)]
pub fn parse_cat_file_batch_output_with_oids(
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

    // Check if original commit has an authorship log with prompts or humans
    let has_existing_log = get_reference_as_authorship_log_v3(repo, original_commit).is_ok();
    let has_existing_data = if has_existing_log {
        let original_log = get_reference_as_authorship_log_v3(repo, original_commit).unwrap();
        !original_log.metadata.prompts.is_empty()
            || !original_log.metadata.humans.is_empty()
            || !original_log.metadata.sessions.is_empty()
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
                if has_existing_data {
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
                if has_existing_data {
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

    // Fill unattributed lines with bg agent attribution (same as post_commit path)
    if !matches!(
        crate::authorship::background_agent::detect(),
        crate::authorship::background_agent::BackgroundAgent::None
            | crate::authorship::background_agent::BackgroundAgent::WithHooks { .. }
    ) {
        let diff_base = if parent_sha == "initial" {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        } else {
            &parent_sha
        };
        if let Ok(added_lines) = repo.diff_added_lines(diff_base, amended_commit, None) {
            let committed_hunks: std::collections::HashMap<
                String,
                Vec<crate::authorship::authorship_log::LineRange>,
            > = added_lines
                .into_iter()
                .filter(|(_, lines)| !lines.is_empty())
                .map(|(path, lines)| {
                    (
                        path,
                        crate::authorship::authorship_log::LineRange::compress_lines(&lines),
                    )
                })
                .collect();
            crate::authorship::background_agent::fill_unattributed_lines(
                &mut authorship_log,
                &committed_hunks,
                &human_author,
            );
        }
    }

    // Preserve human contributors from the original commit's note — deleting a
    // KnownHuman-attributed line removes the attribution coordinate but must not
    // erase the contributor's association with the commit.
    if let Ok(original_log) = get_reference_as_authorship_log_v3(repo, original_commit) {
        for (id, record) in original_log.metadata.humans {
            authorship_log.metadata.humans.entry(id).or_insert(record);
        }
        // Only preserve sessions from the original commit if they are still
        // referenced by attestations in the amended commit.
        let referenced_session_ids: std::collections::HashSet<String> = authorship_log
            .attestations
            .iter()
            .flat_map(|fa| fa.entries.iter())
            .filter_map(|entry| {
                if entry.hash.starts_with("s_") {
                    Some(
                        entry
                            .hash
                            .split("::")
                            .next()
                            .unwrap_or(&entry.hash)
                            .to_string(),
                    )
                } else {
                    None
                }
            })
            .collect();
        for (id, record) in original_log.metadata.sessions {
            if referenced_session_ids.contains(&id) {
                authorship_log.metadata.sessions.entry(id).or_insert(record);
            }
        }
    }

    // Inject custom attributes into all PromptRecords and SessionRecords (same behavior as post_commit).
    // Always use Config::fresh() to support runtime config updates
    // (especially important for daemon mode, but also good for consistency)
    let custom_attrs = crate::config::Config::fresh().custom_attributes().clone();
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    // Save authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    notes_add(repo, amended_commit, &authorship_json)?;

    // Save INITIAL file for uncommitted attributions
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo.storage.working_log_for_base_commit(amended_commit)?;
        let initial_file_contents =
            working_va.snapshot_contents_for_files(initial_attributions.files.keys());
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
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
        tracing::debug!("Skipping reset working-log reconstruction for invalid zero/empty oid");
        return Ok(());
    }

    tracing::debug!(
        "Reconstructing working log after reset from {} to {}",
        old_head_sha,
        target_commit_sha
    );

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
        tracing::debug!("No files changed between commits, nothing to reconstruct");
        // Still delete old working log
        repo.storage
            .delete_working_log_for_base_commit(old_head_sha)?;
        return Ok(());
    }

    tracing::debug!(
        "Processing {} files for reset authorship reconstruction",
        pathspecs.len()
    );

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
        tracing::debug!("Read {} files from working directory", final_state.len());
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

    tracing::debug!(
        "Built old_head VA with {} files, {} prompts",
        old_head_va.files().len(),
        old_head_va.prompts().len()
    );

    // Step 4: Build VirtualAttributions from target_commit.
    //
    // The original intent was to capture AI lines that predate the reset range — lines that were
    // AI-authored before `target_commit` and are still present in the working directory — so that
    // `merge_attributions_favoring_first` (Step 5) could fill gaps in `old_head_va` with them.
    //
    // The implementation was broken from the start: it called `new_for_base_commit` with both
    // `base_commit` and `blame_start_commit` set to `target_commit_sha`, producing a blame range
    // of `target..target` (oldest == newest). That range is always empty — every line is
    // attributed to a boundary commit and mapped to human — so `target_va` always had zero AI
    // attributions and never filled any gaps.
    //
    // Additionally, `old_head_va` is built via `from_working_log_for_commit`, which replays the
    // existing working log entries at `old_head` on top of blame. Any AI lines that predate the
    // reset range and are tracked by git-ai are already carried into `old_head_va` through the
    // working log replay, so a correct `target_va` would have been redundant anyway.
    //
    // We create an empty VA directly (no subprocess calls). The merge result is identical to
    // before the fix because `target_va` was always empty.
    let target_va = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        crate::authorship::virtual_attribution::VirtualAttributions::new(
            repo.clone(),
            target_commit_sha.to_string(),
            HashMap::new(),
            HashMap::new(),
            ts,
        )
    };

    // Step 5: Merge VAs favoring old_head to preserve uncommitted AI changes
    // old_head (with working log) wins overlaps, target fills gaps
    let merged_va = crate::authorship::virtual_attribution::merge_attributions_favoring_first(
        old_head_va,
        target_va,
        final_state.clone(),
    )?;

    tracing::debug!("Merged VAs, result has {} files", merged_va.files().len());

    // Step 6: Convert to INITIAL (everything is uncommitted after reset) without consulting the
    // live worktree again.
    let initial_attributions = merged_va.to_initial_working_log_only();

    tracing::debug!(
        "Generated INITIAL attributions for {} files, {} prompts",
        initial_attributions.files.len(),
        initial_attributions.prompts.len()
    );

    // Step 7: Write INITIAL file
    let new_working_log = repo
        .storage
        .working_log_for_base_commit(target_commit_sha)?;
    new_working_log.reset_working_log()?;

    if !initial_attributions.files.is_empty() {
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            final_state,
            initial_attributions.sessions,
        )?;
    }

    // Delete old working log
    repo.storage
        .delete_working_log_for_base_commit(old_head_sha)?;

    tracing::debug!(
        "✓ Wrote INITIAL attributions to working log for {}",
        target_commit_sha
    );

    Ok(())
}

/// Get all file paths modified across a list of commits
#[doc(hidden)]
pub fn get_pathspecs_from_commits(
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

fn build_metadata_only_authorship_log_from_source_notes(
    repo: &Repository,
    source_commits: &[String],
    target_commit_sha: &str,
) -> Result<Option<AuthorshipLog>, GitAiError> {
    use crate::authorship::authorship_log::{HumanRecord, SessionRecord};

    let mut merged_prompts = BTreeMap::new();
    let mut prompt_totals: HashMap<String, (u32, u32)> = HashMap::new();
    let mut merged_humans: BTreeMap<String, HumanRecord> = BTreeMap::new();
    let mut merged_sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
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
        for (hash, record) in log.metadata.humans {
            merged_humans.entry(hash).or_insert(record);
        }
        for (id, record) in log.metadata.sessions {
            merged_sessions.entry(id).or_insert(record);
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
    authorship_log.metadata.humans = merged_humans;
    authorship_log.metadata.sessions = merged_sessions;
    Ok(Some(authorship_log))
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

#[doc(hidden)]
pub fn build_file_attestation_from_line_attributions(
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
#[doc(hidden)]
pub fn diff_based_line_attribution_transfer(
    old_content: &str,
    new_content: &str,
    old_line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Build a sparse lookup from 0-indexed line position → author_id for old content.
    // Using a HashMap instead of a full-size Vec avoids allocating O(file_size) memory
    // when only a small fraction of lines carry AI attribution.
    let mut old_line_author: HashMap<usize, &str> = HashMap::new();
    for attr in old_line_attrs {
        for line_num in attr.start_line..=attr.end_line {
            let idx = (line_num as usize).saturating_sub(1);
            if idx < old_lines.len() {
                old_line_author.insert(idx, &attr.author_id);
            }
        }
    }

    let diff_ops = capture_diff_slices(&old_lines, &new_lines);

    let mut new_line_attrs: Vec<crate::authorship::attribution_tracker::LineAttribution> =
        Vec::with_capacity(old_line_author.len());

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
                    if let Some(author_id) = old_line_author.get(&old_idx) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_transfer_squash_closing_brace() {
        let old_content = "func serve() {\n    listen()\n    handle()\n}\n";
        let new_content =
            "func serve() {\n    listen()\n    handle()\n    logMetrics()\n    shutdown()\n}\n";

        // All 4 lines in old are AI-attributed (1-indexed)
        let old_attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 4,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // Print what we get
        for attr in &result {
            eprintln!(
                "Line {}-{}: {}",
                attr.start_line, attr.end_line, attr.author_id
            );
        }

        // The test expects:
        // Lines 1-3: AI (equal, transferred)
        // Lines 4-5: nothing (inserted)
        // Line 6 (}): should be AI (equal, transferred from old line 4)
        assert_eq!(result.len(), 4, "Should have 4 transferred attributions");
    }

    #[test]
    fn test_diff_transfer_prepend_header() {
        // Simulates rebase where upstream prepends a header line
        let old_content =
            "fn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";
        let new_content = "// header\nfn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";

        // Lines 2-7 in old are AI (1-indexed)
        let old_attrs = vec![
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 4,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 5,
                end_line: 5,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 6,
                end_line: 6,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 7,
                end_line: 7,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // All 6 AI lines should transfer, shifted by +1
        assert_eq!(
            result.len(),
            6,
            "Should have 6 transferred attributions (all shifted by +1)"
        );
        assert_eq!(result[0].start_line, 3); // fn a1 at line 3
        assert_eq!(result[5].start_line, 8); // fn b3 at line 8
    }

    #[test]
    fn test_diff_transfer_with_trailing_newline_mismatch() {
        // Original has no trailing newline, new has trailing newline (common after git merge)
        let old_content =
            "fn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";
        let new_content = "// header\nfn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}\n";

        let old_attrs = vec![LineAttribution {
            start_line: 2,
            end_line: 7,
            author_id: "ai1".to_string(),
            overrode: None,
        }];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);
        for attr in &result {
            eprintln!(
                "Line {}-{}: {}",
                attr.start_line, attr.end_line, attr.author_id
            );
        }
        // Should still transfer all 6 lines
        let total_lines: u32 = result.iter().map(|a| a.end_line - a.start_line + 1).sum();
        assert_eq!(
            total_lines, 6,
            "Should transfer all 6 AI lines even with trailing newline mismatch"
        );
    }
}
