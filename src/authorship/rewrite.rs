use std::collections::HashMap;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::hunk_shift::{DiffHunk, parse_hunk_header};
use crate::error::GitAiError;
use crate::git::notes_api;
use crate::git::repo_state::is_valid_git_oid;
use crate::git::repository::{Repository, exec_git, exec_git_allow_nonzero, exec_git_stdin};

#[derive(Debug)]
pub enum RewriteEvent {
    NonFastForward {
        old_tip: String,
        new_tip: String,
        onto: Option<String>,
    },
    CherryPickComplete {
        sources: Vec<String>,
        new_commits: Vec<String>,
    },
    SquashMerge {
        source_head: String,
        squash_commit: String,
        onto: String,
    },
}

pub(crate) struct DiffTreeResult {
    pub hunks_by_file: HashMap<String, Vec<DiffHunk>>,
    pub renames: Vec<(String, String)>,
}

pub fn handle_rewrite_event(repo: &Repository, event: RewriteEvent) -> Result<(), GitAiError> {
    match event {
        RewriteEvent::SquashMerge {
            ref source_head,
            ref squash_commit,
            ref onto,
        } => handle_squash_merge(repo, source_head, squash_commit, onto),
        RewriteEvent::NonFastForward {
            ref old_tip,
            ref new_tip,
            ref onto,
        } => handle_non_fast_forward_rewrite(repo, old_tip, new_tip, onto.as_deref()).map(|_| ()),
        RewriteEvent::CherryPickComplete {
            sources,
            new_commits,
        } => {
            let mappings: Vec<(String, String)> = sources.into_iter().zip(new_commits).collect();
            if mappings.is_empty() {
                return Ok(());
            }
            let source_shas: Vec<String> = mappings.iter().map(|(src, _)| src.clone()).collect();
            crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &source_shas)?;
            shift_authorship_notes_merging_existing(repo, &mappings)
        }
    }
}

pub fn handle_non_fast_forward_rewrite(
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
    onto: Option<&str>,
) -> Result<Vec<(String, String)>, GitAiError> {
    let mappings = derive_mappings_from_range_diff(repo, old_tip, new_tip, onto)?;
    if mappings.is_empty() {
        return Ok(Vec::new());
    }
    let source_shas: Vec<String> = mappings.iter().map(|(src, _)| src.clone()).collect();
    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &source_shas)?;
    shift_authorship_notes_merging_existing(repo, &mappings)?;
    Ok(mappings)
}

fn handle_squash_merge(
    repo: &Repository,
    source_head: &str,
    squash_commit: &str,
    onto: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::hunk_shift::apply_hunk_shifts_to_file_attestation;

    let target_notes = notes_api::read_notes_batch(repo, &[squash_commit.to_string()])?;
    let existing_target_log = target_notes
        .get(squash_commit)
        .and_then(|raw| AuthorshipLog::deserialize_from_string(raw).ok())
        .filter(|log| !log.attestations.is_empty());

    let base = find_merge_base(repo, source_head, onto).unwrap_or_else(|| onto.to_string());
    let source_commits = list_commits_in_range(repo, &base, source_head);
    let sources = if source_commits.is_empty() {
        vec![source_head.to_string()]
    } else {
        source_commits
    };

    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &sources)?;

    // Batch-read all source notes in O(1) git calls
    let source_notes_map = notes_api::read_notes_batch(repo, &sources)?;

    // Collect which source commits have parseable notes and need intermediate diffs
    struct SourceNote {
        log: AuthorshipLog,
        diff_idx: Option<usize>,
    }

    let mut source_notes: Vec<SourceNote> = Vec::new();
    let mut diff_pairs: Vec<(String, String)> = Vec::new();

    for src_sha in &sources {
        let Some(raw) = source_notes_map.get(src_sha) else {
            continue;
        };
        let Ok(log) = AuthorshipLog::deserialize_from_string(raw) else {
            continue;
        };

        let diff_idx = if src_sha.as_str() != source_head {
            let idx = diff_pairs.len();
            diff_pairs.push((src_sha.clone(), source_head.to_string()));
            Some(idx)
        } else {
            None
        };

        source_notes.push(SourceNote { log, diff_idx });
    }

    if source_notes.is_empty() {
        if let Some(existing_log) = existing_target_log.as_ref()
            && !repo.storage.has_working_log(onto)
        {
            return write_authorship_log(repo, squash_commit, existing_log);
        }
        return post_squash_resolution_working_log(
            repo,
            onto,
            squash_commit,
            existing_target_log,
            &sources,
        );
    }

    // Add the final source_head→squash_commit pair
    let final_diff_idx = diff_pairs.len();
    diff_pairs.push((source_head.to_string(), squash_commit.to_string()));

    // Single batched diff-tree call for ALL intermediate shifts + final shift
    let diff_results = compute_diff_trees_batch(repo, &diff_pairs)?;

    // Phase 1: Shift intermediate notes to source_head's coordinate space and merge
    let mut merged_log: Option<AuthorshipLog> = None;

    for note in source_notes {
        let mut log = note.log;

        if let Some(idx) = note.diff_idx {
            let diff_to_tip = &diff_results[idx];
            for (old_path, new_path) in &diff_to_tip.renames {
                for attestation in &mut log.attestations {
                    if attestation.file_path == *old_path {
                        attestation.file_path = new_path.clone();
                    }
                }
            }
            if !diff_to_tip.hunks_by_file.is_empty() {
                log.attestations = log
                    .attestations
                    .iter()
                    .filter_map(|fa| match diff_to_tip.hunks_by_file.get(&fa.file_path) {
                        Some(hunks) => apply_hunk_shifts_to_file_attestation(fa, hunks),
                        None => Some(fa.clone()),
                    })
                    .collect();
            }
        }

        match merged_log.as_mut() {
            Some(existing) => merge_authorship_logs(existing, &log),
            None => merged_log = Some(log),
        }
    }

    let Some(mut final_log) = merged_log else {
        return Ok(());
    };

    // Phase 2: Shift merged log from source_head to squash_commit
    let diff_result = &diff_results[final_diff_idx];

    for (old_path, new_path) in &diff_result.renames {
        for attestation in &mut final_log.attestations {
            if attestation.file_path == *old_path {
                attestation.file_path = new_path.clone();
            }
        }
    }

    if !diff_result.hunks_by_file.is_empty() {
        final_log.attestations = final_log
            .attestations
            .iter()
            .filter_map(|fa| match diff_result.hunks_by_file.get(&fa.file_path) {
                Some(hunks) => apply_hunk_shifts_to_file_attestation(fa, hunks),
                None => Some(fa.clone()),
            })
            .collect();
    }

    final_log.metadata.base_commit_sha = squash_commit.to_string();

    let shifted_log = match existing_target_log {
        Some(existing) => {
            crate::authorship::conflict_resolution::merge_conflict_resolution_authorship(
                repo,
                Some(final_log),
                existing,
                &sources,
                squash_commit,
            )
        }
        None => final_log,
    };

    if repo.storage.has_working_log(onto) {
        post_squash_resolution_working_log(repo, onto, squash_commit, Some(shifted_log), &sources)
    } else {
        write_authorship_log(repo, squash_commit, &shifted_log)
    }
}

fn post_squash_resolution_working_log(
    repo: &Repository,
    onto: &str,
    squash_commit: &str,
    existing_shifted_log: Option<AuthorshipLog>,
    sources: &[String],
) -> Result<(), GitAiError> {
    if !repo.storage.has_working_log(onto) {
        if let Some(log) = existing_shifted_log {
            return write_authorship_log(repo, squash_commit, &log);
        }
        return Ok(());
    }

    let source_shas = sources.to_vec();
    let commit_for_transform = squash_commit.to_string();
    let author = repo.git_author_identity().formatted_or_unknown();
    crate::authorship::post_commit::post_commit_from_working_log_with_transform_and_options(
        repo,
        Some(onto.to_string()),
        squash_commit.to_string(),
        author,
        crate::authorship::post_commit::PostCommitOptions {
            supress_output: true,
            compute_stats: false,
        },
        move |resolution_log| {
            Ok(
                crate::authorship::conflict_resolution::merge_conflict_resolution_authorship(
                    repo,
                    existing_shifted_log,
                    resolution_log,
                    &source_shas,
                    &commit_for_transform,
                ),
            )
        },
    )
    .map(|_| ())
}

fn write_authorship_log(
    repo: &Repository,
    commit_sha: &str,
    log: &AuthorshipLog,
) -> Result<(), GitAiError> {
    let serialized = log.serialize_to_string().map_err(|e| {
        GitAiError::Generic(format!("failed to serialize rewrite authorship log: {}", e))
    })?;
    notes_api::write_notes_batch(repo, &[(commit_sha.to_string(), serialized)])
}

pub fn shift_authorship_notes(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    shift_authorship_notes_with_existing_mode(repo, mappings, false)
}

pub fn shift_authorship_notes_merging_existing(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    shift_authorship_notes_with_existing_mode(repo, mappings, true)
}

fn shift_authorship_notes_with_existing_mode(
    repo: &Repository,
    mappings: &[(String, String)],
    merge_existing_targets: bool,
) -> Result<(), GitAiError> {
    use crate::authorship::hunk_shift::apply_hunk_shifts_to_file_attestation;

    tracing::debug!("shift_authorship_notes: {} mappings", mappings.len());

    if mappings.is_empty() {
        return Ok(());
    }

    // Batch-read all notes for source and target commits in O(1) git calls
    let all_shas: Vec<String> = mappings
        .iter()
        .flat_map(|(src, dst)| [src.clone(), dst.clone()])
        .collect();
    let notes_map = notes_api::read_notes_batch(repo, &all_shas)?;

    // Determine which mappings need processing
    struct PendingShift {
        new_sha: String,
        log: AuthorshipLog,
        diff_pair_idx: usize,
    }

    let mut pending: Vec<PendingShift> = Vec::new();
    let mut verbatim_writes: Vec<(String, String)> = Vec::new();
    let mut diff_pairs: Vec<(String, String)> = Vec::new();
    let mut existing_by_target: HashMap<String, AuthorshipLog> = HashMap::new();

    for (source_sha, new_sha) in mappings {
        if let Some(existing_raw) = notes_map.get(new_sha) {
            if let Ok(existing_log) = AuthorshipLog::deserialize_from_string(existing_raw) {
                if !existing_log.attestations.is_empty() {
                    if merge_existing_targets {
                        existing_by_target
                            .entry(new_sha.clone())
                            .or_insert(existing_log);
                    } else {
                        continue;
                    }
                }
            } else {
                continue;
            }
        }

        let Some(raw_note) = notes_map.get(source_sha) else {
            continue;
        };

        let Ok(log) = AuthorshipLog::deserialize_from_string(raw_note) else {
            if !merge_existing_targets {
                verbatim_writes.push((new_sha.clone(), raw_note.clone()));
            }
            continue;
        };

        let diff_pair_idx = diff_pairs.len();
        diff_pairs.push((source_sha.clone(), new_sha.clone()));
        pending.push(PendingShift {
            new_sha: new_sha.clone(),
            log,
            diff_pair_idx,
        });
    }

    if pending.is_empty() && verbatim_writes.is_empty() {
        return Ok(());
    }

    // Single batched diff-tree call for all pairs
    let diff_results = if !diff_pairs.is_empty() {
        compute_diff_trees_batch(repo, &diff_pairs)?
    } else {
        Vec::new()
    };

    // Apply shifts and merge logs that share a target commit
    let mut merged_by_target = existing_by_target;

    for shift in pending {
        let diff_result = &diff_results[shift.diff_pair_idx];
        let mut log = shift.log;

        for (old_path, new_path) in &diff_result.renames {
            for attestation in &mut log.attestations {
                if attestation.file_path == *old_path {
                    attestation.file_path = new_path.clone();
                }
            }
        }

        if !diff_result.hunks_by_file.is_empty() {
            log.attestations = log
                .attestations
                .iter()
                .filter_map(|fa| match diff_result.hunks_by_file.get(&fa.file_path) {
                    Some(hunks) => apply_hunk_shifts_to_file_attestation(fa, hunks),
                    None => Some(fa.clone()),
                })
                .collect();
        }

        log.metadata.base_commit_sha = shift.new_sha.clone();

        match merged_by_target.get_mut(&shift.new_sha) {
            Some(existing) => merge_authorship_logs(existing, &log),
            None => {
                merged_by_target.insert(shift.new_sha, log);
            }
        }
    }

    let mut all_writes = verbatim_writes;
    for (sha, log) in merged_by_target {
        let serialized = log.serialize_to_string().map_err(|e| {
            GitAiError::Generic(format!("failed to serialize shifted authorship log: {}", e))
        })?;
        all_writes.push((sha, serialized));
    }

    // Single batched write for all notes
    notes_api::write_notes_batch(repo, &all_writes)?;

    Ok(())
}

fn merge_authorship_logs(target: &mut AuthorshipLog, source: &AuthorshipLog) {
    for src_fa in &source.attestations {
        if let Some(existing_fa) = target
            .attestations
            .iter_mut()
            .find(|a| a.file_path == src_fa.file_path)
        {
            // Merge entries into existing file attestation
            for src_entry in &src_fa.entries {
                if let Some(existing_entry) = existing_fa
                    .entries
                    .iter_mut()
                    .find(|e| e.hash == src_entry.hash)
                {
                    for range in &src_entry.line_ranges {
                        if !existing_entry.line_ranges.contains(range) {
                            existing_entry.line_ranges.push(range.clone());
                        }
                    }
                } else {
                    existing_fa.entries.push(src_entry.clone());
                }
            }
        } else {
            target.attestations.push(src_fa.clone());
        }
    }
    // Merge all metadata maps
    for (key, record) in &source.metadata.prompts {
        target
            .metadata
            .prompts
            .entry(key.clone())
            .or_insert_with(|| record.clone());
    }
    for (key, record) in &source.metadata.sessions {
        target
            .metadata
            .sessions
            .entry(key.clone())
            .or_insert_with(|| record.clone());
    }
    for (key, record) in &source.metadata.humans {
        target
            .metadata
            .humans
            .entry(key.clone())
            .or_insert_with(|| record.clone());
    }
}

fn derive_mappings_from_range_diff(
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
    onto_hint: Option<&str>,
) -> Result<Vec<(String, String)>, GitAiError> {
    let Some(base) = find_merge_base(repo, old_tip, new_tip) else {
        return Ok(Vec::new());
    };

    // Rewind: branch moved backward
    if base == new_tip {
        crate::authorship::rewrite_reset::reconstruct_working_log_after_backward_reset(
            repo, old_tip, new_tip,
        )?;
        return Ok(Vec::new());
    }

    // Fast-forward: no rewrite happened
    if base == old_tip {
        return Ok(Vec::new());
    }

    // Validate onto_hint: it must be an ancestor of new_tip and different from new_tip.
    // If the hint is invalid (e.g., from a checkout-then-rebase where first HEAD change
    // is the checkout, not the rebase), fall back to base.
    let onto = match onto_hint {
        Some(hint) if hint != new_tip && hint != old_tip && is_ancestor(repo, hint, new_tip) => {
            hint
        }
        _ => &base,
    };
    let range_diff_output = run_range_diff(repo, &base, old_tip, onto, new_tip)?;
    let mut mappings = parse_range_diff_output(&range_diff_output);

    let merge_mappings = derive_merge_commit_mappings(repo, &base, old_tip, new_tip, &mappings)?;
    mappings.extend(merge_mappings);

    Ok(mappings)
}

fn is_ancestor(repo: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "merge-base".to_string(),
        "--is-ancestor".to_string(),
        ancestor.to_string(),
        descendant.to_string(),
    ]);
    exec_git_allow_nonzero(&args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_merge_base(repo: &Repository, a: &str, b: &str) -> Option<String> {
    let mut args = repo.global_args_for_exec();
    args.extend(["merge-base".to_string(), a.to_string(), b.to_string()]);

    let output = exec_git_allow_nonzero(&args).ok()?;
    if !output.status.success() {
        return None;
    }
    let base = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if base.is_empty() { None } else { Some(base) }
}

pub(crate) fn list_commits_in_range(repo: &Repository, base: &str, tip: &str) -> Vec<String> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--reverse".to_string(),
        format!("{}..{}", base, tip),
    ]);
    exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn run_range_diff(
    repo: &Repository,
    old_base: &str,
    old_tip: &str,
    new_base: &str,
    new_tip: &str,
) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "range-diff".to_string(),
        "--no-color".to_string(),
        "--no-abbrev".to_string(),
        "-s".to_string(),
        "--creation-factor=100".to_string(),
        format!("{}..{}", old_base, old_tip),
        format!("{}..{}", new_base, new_tip),
    ]);
    let output = exec_git(&args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_range_diff_output(output: &str) -> Vec<(String, String)> {
    let mut mappings = Vec::new();
    let mut pending_dropped: Vec<String> = Vec::new();
    let mut previous_new_sha: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Find first 40-char hex SHA
        let Some((old_sha, rest)) = find_next_sha(trimmed) else {
            continue;
        };

        // Skip whitespace, read status character
        let rest = rest.trim_start();
        let Some(status_char) = rest.chars().next() else {
            continue;
        };

        match status_char {
            '<' => {
                // Dropped commit (squashed into a later commit)
                if !old_sha.chars().all(|c| c == '0') {
                    if let Some(new_sha) = previous_new_sha.as_ref() {
                        mappings.push((old_sha, new_sha.clone()));
                    } else {
                        pending_dropped.push(old_sha);
                    }
                }
            }
            '=' | '!' => {
                // Matched pair
                let after_status = &rest[status_char.len_utf8()..];
                let Some((new_sha, _)) = find_next_sha(after_status) else {
                    continue;
                };
                if old_sha.chars().all(|c| c == '0') || new_sha.chars().all(|c| c == '0') {
                    continue;
                }
                // Map any preceding dropped commits to this new commit (squash)
                for dropped in pending_dropped.drain(..) {
                    mappings.push((dropped, new_sha.clone()));
                }
                previous_new_sha = Some(new_sha.clone());
                mappings.push((old_sha, new_sha));
            }
            _ => {
                // '>' (new commit) or other — skip
                continue;
            }
        }
    }

    mappings
}

/// Find the first maximal ASCII-hex run in `s` whose length is a valid git OID
/// length (40 for SHA-1, 64 for SHA-256) and return it with the remainder of
/// the string after the run.
///
/// Scans over bytes rather than chars so a multibyte commit subject (e.g. a
/// range-diff `-s` line like `Café …`) never makes a window boundary land
/// inside a char and panic. Only a matched, all-ASCII window is converted to a
/// `String`. Taking the maximal run (delimited by non-hex on both sides) means
/// a 64-char SHA-256 OID is returned in full instead of truncated to 40.
fn find_next_sha(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        let run_len = end - start;
        if run_len == 40 || run_len == 64 {
            // The run is all ASCII hex, so slicing here is always char-safe.
            return Some((s[start..end].to_string(), &s[end..]));
        }
        // Not an OID-length run; skip past it entirely and keep scanning.
        i = end;
    }
    None
}

// DEFERRED (code-review #15): old->new merge commits are paired greedily by
// first parent-set match (the inner loop `break`s on the first new_merge whose
// parents all map). When two sibling merges in the same range share an
// identical parent mapping, the first-match pairing can attach old_merge A's
// note to new_merge B and vice versa. Harmless in the common single-merge case;
// a precise fix would disambiguate ties (e.g. by tree identity or commit order)
// instead of taking the first structural match.
fn derive_merge_commit_mappings(
    repo: &Repository,
    base: &str,
    old_tip: &str,
    new_tip: &str,
    existing_mappings: &[(String, String)],
) -> Result<Vec<(String, String)>, GitAiError> {
    let old_merges = list_merge_commits(repo, base, old_tip)?;
    let new_merges = list_merge_commits(repo, base, new_tip)?;

    if old_merges.is_empty() || new_merges.is_empty() {
        return Ok(Vec::new());
    }

    // Batch-check which old merges have notes
    let commits_with_notes = notes_api::commits_with_notes(repo, &old_merges)?;
    let merge_parent_map = get_commit_parents_batch(
        repo,
        &old_merges
            .iter()
            .chain(new_merges.iter())
            .cloned()
            .collect::<Vec<_>>(),
    );

    let mut merge_mappings: Vec<(String, String)> = Vec::new();

    for old_merge in &old_merges {
        if !commits_with_notes.contains(old_merge) {
            continue;
        }

        let old_parents = merge_parent_map.get(old_merge).cloned().unwrap_or_default();
        if old_parents.is_empty() {
            continue;
        }

        for new_merge in &new_merges {
            if merge_mappings.iter().any(|(_, n)| n == new_merge) {
                continue;
            }

            let new_parents = merge_parent_map.get(new_merge).cloned().unwrap_or_default();
            if new_parents.len() != old_parents.len() {
                continue;
            }

            let all_match = old_parents.iter().zip(new_parents.iter()).all(|(op, np)| {
                if existing_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                if merge_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                op == np
            });

            if all_match {
                merge_mappings.push((old_merge.clone(), new_merge.clone()));
                break;
            }
        }
    }

    Ok(merge_mappings)
}

fn list_merge_commits(repo: &Repository, base: &str, tip: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--merges".to_string(),
        "--topo-order".to_string(),
        "--reverse".to_string(),
        format!("{}..{}", base, tip),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn get_commit_parents_batch(repo: &Repository, shas: &[String]) -> HashMap<String, Vec<String>> {
    if shas.is_empty() {
        return HashMap::new();
    }
    let mut args = repo.global_args_for_exec();
    args.extend([
        "show".to_string(),
        "-s".to_string(),
        "--format=%H %P".to_string(),
        "--no-walk".to_string(),
    ]);
    args.extend(shas.iter().cloned());

    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let sha = parts.next()?.to_string();
            let parents = parts.map(ToOwned::to_owned).collect::<Vec<_>>();
            Some((sha, parents))
        })
        .collect()
}

/// Batch-compute diff-trees for multiple commit pairs in a single git process.
/// Resolves commits to tree SHAs, then pipes all pairs into `git diff-tree --stdin`.
pub(crate) fn compute_diff_trees_batch(
    repo: &Repository,
    pairs: &[(String, String)],
) -> Result<Vec<DiffTreeResult>, GitAiError> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }

    // Collect unique commit SHAs and resolve them all to tree SHAs in one rev-parse call
    let mut unique_shas: Vec<String> = Vec::new();
    for (src, dst) in pairs {
        if !unique_shas.contains(src) {
            unique_shas.push(src.clone());
        }
        if !unique_shas.contains(dst) {
            unique_shas.push(dst.clone());
        }
    }

    let mut rev_parse_args = repo.global_args_for_exec();
    rev_parse_args.push("rev-parse".to_string());
    for sha in &unique_shas {
        rev_parse_args.push(format!("{}^{{tree}}", sha));
    }
    let rev_output = exec_git(&rev_parse_args)?;
    let rev_stdout = String::from_utf8_lossy(&rev_output.stdout);
    let tree_shas: Vec<&str> = rev_stdout.lines().collect();

    if tree_shas.len() != unique_shas.len() {
        return Err(GitAiError::Generic(format!(
            "rev-parse returned {} trees for {} commits",
            tree_shas.len(),
            unique_shas.len()
        )));
    }

    // Build commit→tree lookup
    let sha_to_tree: HashMap<&str, &str> = unique_shas
        .iter()
        .zip(tree_shas.iter())
        .map(|(commit, tree)| (commit.as_str(), *tree))
        .collect();

    // Build stdin: one "tree1 tree2\n" line per pair
    let mut stdin_data = String::new();
    let mut tree_pair_keys: Vec<(&str, &str)> = Vec::with_capacity(pairs.len());
    for (src, dst) in pairs {
        let src_tree = sha_to_tree[src.as_str()];
        let dst_tree = sha_to_tree[dst.as_str()];
        stdin_data.push_str(src_tree);
        stdin_data.push(' ');
        stdin_data.push_str(dst_tree);
        stdin_data.push('\n');
        tree_pair_keys.push((src_tree, dst_tree));
    }

    // Single git diff-tree --stdin call.
    //
    // We intentionally use the General profile (no PatchParse prefix forcing)
    // here: `diff-tree` is plumbing and -- unlike the `git diff` porcelain --
    // ignores the user's diff.{noprefix,mnemonicPrefix,srcPrefix,dstPrefix},
    // diff.external, and per-path textconv attributes. It always emits raw
    // content with default `a/`..`b/` prefixes, which is exactly what
    // extract_b_path / parse_diff_tree_output expect. (Contrast diff_added_lines
    // in repository.rs, which DOES run `git diff` and therefore must force
    // InternalGitProfile::PatchParse.)
    let mut args = repo.global_args_for_exec();
    args.extend([
        "diff-tree".to_string(),
        "--stdin".to_string(),
        "-p".to_string(),
        "-U0".to_string(),
        "-M".to_string(),
        "--no-color".to_string(),
        "-r".to_string(),
    ]);

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse output: each pair's result starts with a "tree1 tree2\n" separator line
    parse_batched_diff_tree_output(&stdout, &tree_pair_keys)
}

/// Parse the output of `git diff-tree --stdin` which produces multiple results
/// separated by "tree1 tree2" header lines.
fn parse_batched_diff_tree_output(
    output: &str,
    tree_pair_keys: &[(&str, &str)],
) -> Result<Vec<DiffTreeResult>, GitAiError> {
    let mut results: Vec<DiffTreeResult> = Vec::with_capacity(tree_pair_keys.len());
    let mut current_chunk = String::new();
    let mut seen_first_header = false;

    for line in output.lines() {
        // Separator lines are exactly "tree_sha1 tree_sha2" (two 40-char hex SHAs separated by space)
        if is_tree_pair_separator(line) {
            if seen_first_header {
                results.push(parse_diff_tree_output(&current_chunk));
                current_chunk.clear();
            }
            seen_first_header = true;
        } else if seen_first_header {
            current_chunk.push_str(line);
            current_chunk.push('\n');
        }
    }

    // Push final chunk
    if seen_first_header {
        results.push(parse_diff_tree_output(&current_chunk));
    }

    // If git produced fewer results than pairs, pad with empty results
    // (happens when trees are identical — no separator line emitted)
    while results.len() < tree_pair_keys.len() {
        results.push(DiffTreeResult {
            hunks_by_file: HashMap::new(),
            renames: Vec::new(),
        });
    }

    Ok(results)
}

fn is_tree_pair_separator(line: &str) -> bool {
    // "tree1 tree2" — two git OIDs separated by a single space. Validate both
    // halves structurally via is_valid_git_oid so this accepts both the 81-byte
    // SHA-1 separator and the 129-byte SHA-256 separator (rather than a
    // hard-coded length).
    let Some((old, new)) = line.split_once(' ') else {
        return false;
    };
    is_valid_git_oid(old) && is_valid_git_oid(new)
}

fn parse_diff_tree_output(output: &str) -> DiffTreeResult {
    let mut hunks_by_file: HashMap<String, Vec<DiffHunk>> = HashMap::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_rename_from: Option<String> = None;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Extract the b/ path from "a/old b/new"
            current_file = extract_b_path(rest);
            current_rename_from = None;
        } else if let Some(from_path) = line.strip_prefix("rename from ") {
            current_rename_from = Some(from_path.to_string());
        } else if let Some(to_path) = line.strip_prefix("rename to ") {
            if let Some(from_path) = current_rename_from.take() {
                renames.push((from_path, to_path.to_string()));
            }
        } else if line.starts_with("@@")
            && let Some(ref file) = current_file
            && let Some(hunk) = parse_hunk_header(line)
        {
            hunks_by_file.entry(file.clone()).or_default().push(hunk);
        }
    }

    DiffTreeResult {
        hunks_by_file,
        renames,
    }
}

fn extract_b_path(diff_header: &str) -> Option<String> {
    // Format: "a/path b/path" or "a/path with spaces b/path with spaces"
    // The b/ path starts after the last occurrence of " b/"
    let marker = " b/";
    let pos = diff_header.rfind(marker)?;
    Some(diff_header[pos + marker.len()..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_b_path_simple() {
        assert_eq!(
            extract_b_path("a/src/main.rs b/src/main.rs"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn test_extract_b_path_rename() {
        assert_eq!(
            extract_b_path("a/src/old.rs b/src/new.rs"),
            Some("src/new.rs".to_string())
        );
    }

    #[test]
    fn test_extract_b_path_with_spaces() {
        assert_eq!(
            extract_b_path("a/path with spaces b/another path"),
            Some("another path".to_string())
        );
    }

    #[test]
    fn test_parse_diff_tree_output_simple() {
        let output = "\
diff --git a/src/foo.rs b/src/foo.rs
index abc123..def456 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +10,5 @@ fn foo()
+added line 1
+added line 2
";
        let result = parse_diff_tree_output(output);
        assert!(result.renames.is_empty());
        assert_eq!(result.hunks_by_file.len(), 1);
        let hunks = &result.hunks_by_file["src/foo.rs"];
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 10);
        assert_eq!(hunks[0].old_count, 3);
        assert_eq!(hunks[0].new_start, 10);
        assert_eq!(hunks[0].new_count, 5);
    }

    #[test]
    fn test_parse_diff_tree_output_with_rename() {
        let output = "\
diff --git a/src/old.rs b/src/new.rs
similarity index 90%
rename from src/old.rs
rename to src/new.rs
index abc123..def456 100644
--- a/src/old.rs
+++ b/src/new.rs
@@ -5,2 +5,3 @@ fn bar()
+new line
";
        let result = parse_diff_tree_output(output);
        assert_eq!(result.renames.len(), 1);
        assert_eq!(
            result.renames[0],
            ("src/old.rs".to_string(), "src/new.rs".to_string())
        );
        let hunks = &result.hunks_by_file["src/new.rs"];
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 5);
        assert_eq!(hunks[0].old_count, 2);
        assert_eq!(hunks[0].new_start, 5);
        assert_eq!(hunks[0].new_count, 3);
    }

    #[test]
    fn test_parse_diff_tree_output_multiple_files() {
        let output = "\
diff --git a/file1.rs b/file1.rs
index aaa..bbb 100644
--- a/file1.rs
+++ b/file1.rs
@@ -1,2 +1,3 @@
+line
diff --git a/file2.rs b/file2.rs
index ccc..ddd 100644
--- a/file2.rs
+++ b/file2.rs
@@ -10,0 +11,2 @@
+line1
+line2
";
        let result = parse_diff_tree_output(output);
        assert_eq!(result.hunks_by_file.len(), 2);
        assert_eq!(result.hunks_by_file["file1.rs"].len(), 1);
        assert_eq!(result.hunks_by_file["file2.rs"].len(), 1);
        assert_eq!(result.hunks_by_file["file2.rs"][0].old_start, 10);
        assert_eq!(result.hunks_by_file["file2.rs"][0].old_count, 0);
        assert_eq!(result.hunks_by_file["file2.rs"][0].new_start, 11);
        assert_eq!(result.hunks_by_file["file2.rs"][0].new_count, 2);
    }

    #[test]
    fn test_parse_diff_tree_output_binary() {
        let output = "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
";
        let result = parse_diff_tree_output(output);
        // No hunks for binary files
        assert!(
            result
                .hunks_by_file
                .get("image.png")
                .is_none_or(|h| h.is_empty())
        );
    }

    #[test]
    fn test_parse_diff_tree_empty_output() {
        let result = parse_diff_tree_output("");
        assert!(result.hunks_by_file.is_empty());
        assert!(result.renames.is_empty());
    }

    #[test]
    fn test_find_next_sha_rejects_non_oid_length_runs() {
        // A hex run that is neither 40 nor 64 chars is not an OID and must be
        // skipped (e.g. a short abbreviated hash or an index blob fragment).
        assert!(find_next_sha("deadbeef not a full oid").is_none());
        // 39 and 41 chars (off-by-one around SHA-1) are rejected.
        assert!(find_next_sha(&"a".repeat(39)).is_none());
        let nearly = format!("{} x", "a".repeat(41));
        assert!(find_next_sha(&nearly).is_none());
    }

    #[test]
    fn test_parse_range_diff_output_matched_equal() {
        let output = " 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Some commit subject\n";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(mappings[0].1, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn test_parse_range_diff_output_matched_bang() {
        let output = " 2:  1111111111111111111111111111111111111111 ! 3:  2222222222222222222222222222222222222222 Modified commit\n";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, "1111111111111111111111111111111111111111");
        assert_eq!(mappings[0].1, "2222222222222222222222222222222222222222");
    }

    #[test]
    fn test_parse_range_diff_output_dropped_and_new() {
        let output = "\
 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa < -:  0000000000000000000000000000000000000000 Dropped commit
 -:  0000000000000000000000000000000000000000 > 1:  cccccccccccccccccccccccccccccccccccccccc New commit
";
        let mappings = parse_range_diff_output(output);
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_parse_range_diff_output_dropped_then_matched_maps_both_to_destination() {
        let output = "\
1:  1111111111111111111111111111111111111111 < -:  ---------------------------------------- Add Python joke
2:  2222222222222222222222222222222222222222 ! 1:  3333333333333333333333333333333333333333 Add Rust joke
";
        let mappings = parse_range_diff_output(output);
        assert_eq!(
            mappings,
            vec![
                (
                    "1111111111111111111111111111111111111111".to_string(),
                    "3333333333333333333333333333333333333333".to_string()
                ),
                (
                    "2222222222222222222222222222222222222222".to_string(),
                    "3333333333333333333333333333333333333333".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_parse_range_diff_output_matched_then_dropped_maps_all_to_destination() {
        let output = "\
1:  1111111111111111111111111111111111111111 ! 1:  4444444444444444444444444444444444444444 AI commit 1
2:  2222222222222222222222222222222222222222 < -:  ---------------------------------------- AI commit 2
3:  3333333333333333333333333333333333333333 < -:  ---------------------------------------- AI commit 3
";
        let mappings = parse_range_diff_output(output);
        assert_eq!(
            mappings,
            vec![
                (
                    "1111111111111111111111111111111111111111".to_string(),
                    "4444444444444444444444444444444444444444".to_string()
                ),
                (
                    "2222222222222222222222222222222222222222".to_string(),
                    "4444444444444444444444444444444444444444".to_string()
                ),
                (
                    "3333333333333333333333333333333333333333".to_string(),
                    "4444444444444444444444444444444444444444".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_parse_range_diff_output_null_shas_skipped() {
        let output = " 1:  0000000000000000000000000000000000000000 = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Subject\n";
        let mappings = parse_range_diff_output(output);
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_parse_range_diff_output_multiple_lines() {
        let output = "\
 1:  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1:  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb First commit
 2:  cccccccccccccccccccccccccccccccccccccccc ! 2:  dddddddddddddddddddddddddddddddddddddddd Second commit
 3:  eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee = 3:  ffffffffffffffffffffffffffffffffffffffff Third commit
";
        let mappings = parse_range_diff_output(output);
        assert_eq!(mappings.len(), 3);
        assert_eq!(
            mappings[0],
            (
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
            )
        );
        assert_eq!(
            mappings[1],
            (
                "cccccccccccccccccccccccccccccccccccccccc".to_string(),
                "dddddddddddddddddddddddddddddddddddddddd".to_string()
            )
        );
        assert_eq!(
            mappings[2],
            (
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                "ffffffffffffffffffffffffffffffffffffffff".to_string()
            )
        );
    }

    #[test]
    fn test_parse_range_diff_output_empty() {
        let mappings = parse_range_diff_output("");
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_is_tree_pair_separator_valid() {
        let line =
            "1778ed95466977076f4e5908e6500789be732d2e 471b7bbf5998ffa15a81b17ee9f6854a357a2a6a";
        assert!(is_tree_pair_separator(line));
    }

    #[test]
    fn test_is_tree_pair_separator_invalid() {
        assert!(!is_tree_pair_separator("diff --git a/foo b/foo"));
        assert!(!is_tree_pair_separator("@@ -1,2 +1,3 @@"));
        assert!(!is_tree_pair_separator(""));
        assert!(!is_tree_pair_separator("short"));
        // Missing space
        assert!(!is_tree_pair_separator(
            "1778ed95466977076f4e5908e6500789be732d2e471b7bbf5998ffa15a81b17ee9f6854a357a2a6a"
        ));
    }

    #[test]
    fn test_find_next_sha_does_not_panic_on_multibyte_subject() {
        // Regression (#1): find_next_sha sliced `&s[i..i+40]` by byte index. A
        // commit subject with a multibyte char ('é' at bytes 3..5) makes a
        // byte-window boundary land inside the char and panics
        // ("byte index 4 is not a char boundary; inside 'é'"). It must scan
        // safely and still find the trailing SHA.
        let sha = "a".repeat(40);
        let input = format!("Café commit subject {}", sha);
        let (found, rest) = find_next_sha(&input).expect("should find the trailing SHA");
        assert_eq!(found, sha);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_find_next_sha_returns_full_sha256_oid() {
        // Regression (#10): a 64-char SHA-256 OID must be returned in full, not
        // truncated to the first 40 chars.
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(sha256.len(), 64);
        let input = format!("{} trailing", sha256);
        let (found, rest) = find_next_sha(&input).expect("should find the 64-char OID");
        assert_eq!(found, sha256);
        assert_eq!(rest, " trailing");
    }

    #[test]
    fn test_is_tree_pair_separator_accepts_sha256_pair() {
        // Regression (#10): a SHA-256 tree-pair separator is "64hex 64hex"
        // (129 bytes), not the hard-coded 81-byte SHA-1 shape.
        let a = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        let line = format!("{} {}", a, b);
        assert_eq!(line.len(), 129);
        assert!(is_tree_pair_separator(&line));
    }

    #[test]
    fn test_parse_range_diff_output_sha256() {
        // Regression (#10): range-diff with 64-char OIDs must map the full OIDs,
        // not 40-char truncations.
        let old = "1111111111111111111111111111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222222222222222222222222222";
        let output = format!(" 1:  {} = 1:  {} Some subject\n", old, new);
        let mappings = parse_range_diff_output(&output);
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].0, old);
        assert_eq!(mappings[0].1, new);
    }

    #[test]
    fn test_parse_batched_diff_tree_output_single_pair() {
        let output = "\
1778ed95466977076f4e5908e6500789be732d2e 471b7bbf5998ffa15a81b17ee9f6854a357a2a6a
diff --git a/f.txt b/f.txt
index a29bdeb..c0d0fb4 100644
--- a/f.txt
+++ b/f.txt
@@ -1,0 +2 @@ line1
+line2
";
        let keys = [(
            "1778ed95466977076f4e5908e6500789be732d2e",
            "471b7bbf5998ffa15a81b17ee9f6854a357a2a6a",
        )];
        let results = parse_batched_diff_tree_output(output, &keys).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hunks_by_file.len(), 1);
        assert_eq!(results[0].hunks_by_file["f.txt"][0].new_count, 1);
    }

    #[test]
    fn test_parse_batched_diff_tree_output_multiple_pairs() {
        let output = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
diff --git a/f.txt b/f.txt
index a29bdeb..c0d0fb4 100644
--- a/f.txt
+++ b/f.txt
@@ -1,0 +2 @@ line1
+line2
cccccccccccccccccccccccccccccccccccccccc dddddddddddddddddddddddddddddddddddddddd
diff --git a/g.txt b/g.txt
index eee..fff 100644
--- a/g.txt
+++ b/g.txt
@@ -5,2 +5,3 @@
+new line
";
        let keys = [
            (
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            (
                "cccccccccccccccccccccccccccccccccccccccc",
                "dddddddddddddddddddddddddddddddddddddddd",
            ),
        ];
        let results = parse_batched_diff_tree_output(output, &keys).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].hunks_by_file.len(), 1);
        assert!(results[0].hunks_by_file.contains_key("f.txt"));
        assert_eq!(results[1].hunks_by_file.len(), 1);
        assert!(results[1].hunks_by_file.contains_key("g.txt"));
    }

    #[test]
    fn test_parse_batched_diff_tree_output_identical_trees() {
        let output = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
";
        let keys = [(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )];
        let results = parse_batched_diff_tree_output(output, &keys).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].hunks_by_file.is_empty());
        assert!(results[0].renames.is_empty());
    }

    #[test]
    fn test_parse_batched_diff_tree_output_mixed_identical_and_changed() {
        let output = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
diff --git a/f.txt b/f.txt
@@ -1,0 +2 @@
+x
cccccccccccccccccccccccccccccccccccccccc cccccccccccccccccccccccccccccccccccccccc
dddddddddddddddddddddddddddddddddddddddd eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
diff --git a/g.txt b/g.txt
@@ -3,1 +3,2 @@
+y
";
        let keys = [
            (
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            (
                "cccccccccccccccccccccccccccccccccccccccc",
                "cccccccccccccccccccccccccccccccccccccccc",
            ),
            (
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            ),
        ];
        let results = parse_batched_diff_tree_output(output, &keys).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].hunks_by_file.len(), 1);
        assert!(results[1].hunks_by_file.is_empty());
        assert_eq!(results[2].hunks_by_file.len(), 1);
    }

    #[test]
    fn test_parse_batched_diff_tree_output_empty() {
        let results = parse_batched_diff_tree_output("", &[]).unwrap();
        assert!(results.is_empty());
    }
}
