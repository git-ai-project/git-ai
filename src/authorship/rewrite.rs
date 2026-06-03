use std::collections::HashMap;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::hunk_shift::{DiffHunk, parse_hunk_header};
use crate::error::GitAiError;
use crate::git::notes_api;
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
        } => {
            let result = derive_mappings_from_range_diff(repo, old_tip, new_tip, onto.as_deref())?;
            match result {
                RangeDiffResult::Squash { base } => {
                    handle_squash_merge(repo, old_tip, new_tip, &base)
                }
                RangeDiffResult::Mappings(mappings) => {
                    if mappings.is_empty() {
                        return Ok(());
                    }
                    let source_shas: Vec<String> =
                        mappings.iter().map(|(src, _)| src.clone()).collect();
                    crate::git::sync_authorship::fetch_missing_notes_for_commits(
                        repo,
                        &source_shas,
                    );
                    shift_authorship_notes(repo, &mappings)
                }
            }
        }
        RewriteEvent::CherryPickComplete {
            sources,
            new_commits,
        } => {
            let mappings: Vec<(String, String)> = sources.into_iter().zip(new_commits).collect();
            if mappings.is_empty() {
                return Ok(());
            }
            let source_shas: Vec<String> = mappings.iter().map(|(src, _)| src.clone()).collect();
            crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &source_shas);
            shift_authorship_notes(repo, &mappings)
        }
    }
}

fn handle_squash_merge(
    repo: &Repository,
    source_head: &str,
    squash_commit: &str,
    onto: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::hunk_shift::apply_hunk_shifts_to_file_attestation;

    // Check if target already has non-empty attestations (e.g. from post-commit hook)
    let target_notes = notes_api::read_notes_batch(repo, &[squash_commit.to_string()])?;
    if let Some(existing_raw) = target_notes.get(squash_commit)
        && let Ok(existing_log) = AuthorshipLog::deserialize_from_string(existing_raw)
        && !existing_log.attestations.is_empty()
    {
        return Ok(());
    }

    let base = find_merge_base(repo, source_head, onto).unwrap_or_else(|| onto.to_string());
    let source_commits = list_commits_in_range(repo, &base, source_head);
    let sources = if source_commits.is_empty() {
        vec![source_head.to_string()]
    } else {
        source_commits
    };

    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &sources);

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
        return Ok(());
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

    let serialized = final_log.serialize_to_string().map_err(|e| {
        GitAiError::Generic(format!("failed to serialize squash authorship log: {}", e))
    })?;
    notes_api::write_notes_batch(repo, &[(squash_commit.to_string(), serialized)])?;
    Ok(())
}

pub fn shift_authorship_notes(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    use crate::authorship::hunk_shift::apply_hunk_shifts_to_file_attestation;

    tracing::debug!("shift_authorship_notes: {} mappings", mappings.len());

    if let Ok(debug_path) = std::env::var("GIT_AI_DEBUG_FILE") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_path)
        {
            for (src, dst) in mappings {
                let _ = writeln!(
                    f,
                    "[shift_notes] {}→{}",
                    &src[..8.min(src.len())],
                    &dst[..8.min(dst.len())]
                );
            }
        }
    }

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

    for (source_sha, new_sha) in mappings {
        // Skip if target already has non-empty attestations
        if let Some(existing_raw) = notes_map.get(new_sha) {
            if let Ok(existing_log) = AuthorshipLog::deserialize_from_string(existing_raw) {
                if !existing_log.attestations.is_empty() {
                    continue;
                }
            } else {
                continue;
            }
        }

        let Some(raw_note) = notes_map.get(source_sha) else {
            continue;
        };

        let Ok(log) = AuthorshipLog::deserialize_from_string(raw_note) else {
            verbatim_writes.push((new_sha.clone(), raw_note.clone()));
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
    let mut merged_by_target: HashMap<String, AuthorshipLog> = HashMap::new();

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

enum RangeDiffResult {
    Mappings(Vec<(String, String)>),
    Squash { base: String },
}

fn derive_mappings_from_range_diff(
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
    onto_hint: Option<&str>,
) -> Result<RangeDiffResult, GitAiError> {
    let Some(base) = find_merge_base(repo, old_tip, new_tip) else {
        return Ok(RangeDiffResult::Mappings(Vec::new()));
    };

    // Rewind: branch moved backward
    if base == new_tip {
        crate::authorship::rewrite_reset::reconstruct_working_log_after_backward_reset(
            repo, old_tip, new_tip,
        )?;
        return Ok(RangeDiffResult::Mappings(Vec::new()));
    }

    // Fast-forward: no rewrite happened
    if base == old_tip {
        return Ok(RangeDiffResult::Mappings(Vec::new()));
    }

    // Full squash: all old commits collapsed into one new commit.
    // Signal to caller so it can delegate to handle_squash_merge.
    if is_full_squash(repo, &base, old_tip, new_tip, onto_hint) {
        return Ok(RangeDiffResult::Squash { base });
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

    Ok(RangeDiffResult::Mappings(mappings))
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

fn is_full_squash(
    repo: &Repository,
    base: &str,
    old_tip: &str,
    new_tip: &str,
    onto_hint: Option<&str>,
) -> bool {
    let old_count = count_commits_in_range(repo, base, old_tip);
    if old_count <= 1 {
        return false;
    }

    // If we have a valid onto hint, count commits between onto and new_tip (the rebased commits)
    let valid_onto = onto_hint
        .filter(|hint| *hint != new_tip && *hint != old_tip && is_ancestor(repo, hint, new_tip));
    let new_rebased_count = if let Some(onto) = valid_onto {
        count_commits_in_range(repo, onto, new_tip)
    } else {
        // Fallback: count commits unique to new side using three-dot symmetric diff
        let mut args = repo.global_args_for_exec();
        args.extend([
            "rev-list".to_string(),
            "--count".to_string(),
            "--right-only".to_string(),
            format!("{}...{}", old_tip, new_tip),
        ]);
        exec_git_allow_nonzero(&args)
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(0)
    };

    new_rebased_count == 1
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

fn count_commits_in_range(repo: &Repository, base: &str, tip: &str) -> usize {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--count".to_string(),
        format!("{}..{}", base, tip),
    ]);
    exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0)
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
                    pending_dropped.push(old_sha);
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

fn find_next_sha(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 40 <= bytes.len() {
        let candidate = &s[i..i + 40];
        if is_hex_sha(candidate) {
            return Some((candidate.to_string(), &s[i + 40..]));
        }
        i += 1;
    }
    None
}

fn is_hex_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

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

    let mut merge_mappings: Vec<(String, String)> = Vec::new();

    for old_merge in &old_merges {
        if !commits_with_notes.contains(old_merge) {
            continue;
        }

        let old_parents = get_commit_parents(repo, old_merge);
        if old_parents.is_empty() {
            continue;
        }

        for new_merge in &new_merges {
            if merge_mappings.iter().any(|(_, n)| n == new_merge) {
                continue;
            }

            let new_parents = get_commit_parents(repo, new_merge);
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

fn get_commit_parents(repo: &Repository, sha: &str) -> Vec<String> {
    let mut args = repo.global_args_for_exec();
    args.extend(["rev-parse".to_string(), format!("{}^@", sha)]);

    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
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

    // Single git diff-tree --stdin call
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
    let bytes = line.as_bytes();
    bytes.len() == 81
        && bytes[40] == b' '
        && line[..40].bytes().all(|b| b.is_ascii_hexdigit())
        && line[41..].bytes().all(|b| b.is_ascii_hexdigit())
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
    fn test_is_hex_sha_valid() {
        assert!(is_hex_sha("a".repeat(40).as_str()));
        assert!(is_hex_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_hex_sha("ABCDEF0123456789abcdef0123456789abcdef01"));
    }

    #[test]
    fn test_is_hex_sha_invalid() {
        assert!(!is_hex_sha("short"));
        assert!(!is_hex_sha("g123456789abcdef0123456789abcdef01234567"));
        assert!(!is_hex_sha("0123456789abcdef0123456789abcdef0123456")); // 39 chars
        assert!(!is_hex_sha("0123456789abcdef0123456789abcdef012345678")); // 41 chars
        assert!(!is_hex_sha(""));
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
