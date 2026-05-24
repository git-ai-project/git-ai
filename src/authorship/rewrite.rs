use std::collections::HashMap;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::hunk_shift::{DiffHunk, parse_hunk_header};
use crate::authorship::rewrite_reset::file_content_at_commit;
use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git, exec_git_allow_nonzero};

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
        _ => {
            let mappings = match event {
                RewriteEvent::NonFastForward {
                    ref old_tip,
                    ref new_tip,
                    ref onto,
                } => derive_mappings_from_range_diff(repo, old_tip, new_tip, onto.as_deref())?,
                RewriteEvent::CherryPickComplete {
                    sources,
                    new_commits,
                } => sources.into_iter().zip(new_commits).collect(),
                RewriteEvent::SquashMerge { .. } => unreachable!(),
            };
            if mappings.is_empty() {
                return Ok(());
            }
            let source_shas: Vec<String> = mappings.iter().map(|(src, _)| src.clone()).collect();
            crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &source_shas);
            shift_authorship_notes(repo, &mappings)?;
            migrate_working_log_if_needed(repo, &mappings)?;
            Ok(())
        }
    }
}

fn handle_squash_merge(
    repo: &Repository,
    source_head: &str,
    squash_commit: &str,
    onto: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, FileAttestation};
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};
    use std::collections::HashSet;

    let base = find_merge_base(repo, source_head, onto).unwrap_or_else(|| onto.to_string());
    let source_commits = list_commits_in_range(repo, &base, source_head);
    let sources = if source_commits.is_empty() {
        vec![source_head.to_string()]
    } else {
        source_commits
    };

    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &sources);

    // For each source commit, shift its attributions to the squash commit's line numbering,
    // then merge all shifted logs together. Each commit's note uses line numbers relative to
    // that commit's file state, so we must shift individually.
    let mut result_log: Option<AuthorshipLog> = None;

    // Pre-compute which lines in the squash commit are new (not in onto).
    // Cache per file path to avoid re-computing for each source commit.
    let mut new_lines_cache: HashMap<String, HashSet<u32>> = HashMap::new();

    for src_sha in &sources {
        let Some(raw) = read_authorship_note(repo, src_sha)? else {
            continue;
        };
        let Ok(log) = AuthorshipLog::deserialize_from_string(&raw) else {
            continue;
        };

        let shifted: Vec<_> = log
            .attestations
            .iter()
            .filter_map(|fa| {
                let src_content = file_content_at_commit(repo, src_sha, &fa.file_path);
                let dst_content = file_content_at_commit(repo, squash_commit, &fa.file_path);

                if dst_content.is_empty() {
                    return None;
                }

                // Get or compute new lines for this file
                let new_lines_in_squash = new_lines_cache
                    .entry(fa.file_path.clone())
                    .or_insert_with(|| {
                        let onto_content = file_content_at_commit(repo, onto, &fa.file_path);
                        let onto_lines: Vec<&str> = onto_content.lines().collect();
                        let dst_lines: Vec<&str> = dst_content.lines().collect();
                        let diff_ops = capture_diff_slices(&onto_lines, &dst_lines);
                        let mut new_lines: HashSet<u32> = HashSet::new();
                        for op in &diff_ops {
                            match op {
                                DiffOp::Insert {
                                    new_index, new_len, ..
                                }
                                | DiffOp::Replace {
                                    new_index, new_len, ..
                                } => {
                                    for i in 0..*new_len {
                                        new_lines.insert((*new_index + i + 1) as u32);
                                    }
                                }
                                _ => {}
                            }
                        }
                        new_lines
                    });

                // Build mapping from this source commit's line numbers to squash commit
                let src_lines: Vec<&str> = src_content.lines().collect();
                let dst_lines: Vec<&str> = dst_content.lines().collect();
                let old_to_new = if src_content == dst_content {
                    (1..=src_lines.len() as u32)
                        .map(|i| (i, i))
                        .collect::<HashMap<u32, u32>>()
                } else if src_content.is_empty() {
                    HashMap::new()
                } else {
                    let ops = capture_diff_slices(&src_lines, &dst_lines);
                    let mut map = HashMap::new();
                    for op in &ops {
                        if let DiffOp::Equal {
                            old_index,
                            new_index,
                            len,
                        } = op
                        {
                            for i in 0..*len {
                                map.insert(
                                    (*old_index + i + 1) as u32,
                                    (*new_index + i + 1) as u32,
                                );
                            }
                        }
                    }
                    map
                };

                // Transfer entries, filtering to only new lines in squash
                let mut new_entries: Vec<AttestationEntry> = Vec::new();
                for entry in &fa.entries {
                    let mut new_ranges: Vec<LineRange> = Vec::new();
                    for range in &entry.line_ranges {
                        let (start, end) = match range {
                            LineRange::Single(l) => (*l, *l),
                            LineRange::Range(s, e) => (*s, *e),
                        };
                        for line in start..=end {
                            if let Some(&new_line) = old_to_new.get(&line)
                                && new_lines_in_squash.contains(&new_line)
                            {
                                new_ranges.push(LineRange::Single(new_line));
                            }
                        }
                    }
                    if !new_ranges.is_empty() {
                        let compacted = compact_line_ranges(new_ranges);
                        new_entries.push(AttestationEntry {
                            hash: entry.hash.clone(),
                            line_ranges: compacted,
                        });
                    }
                }

                if new_entries.is_empty() {
                    None
                } else {
                    Some(FileAttestation {
                        file_path: fa.file_path.clone(),
                        entries: new_entries,
                    })
                }
            })
            .collect();

        let mut shifted_log = log.clone();
        shifted_log.attestations = shifted;
        shifted_log.metadata.base_commit_sha = squash_commit.to_string();

        match result_log.as_mut() {
            Some(existing) => merge_authorship_logs(existing, &shifted_log),
            None => result_log = Some(shifted_log),
        }
    }

    let Some(final_log) = result_log else {
        return Ok(());
    };

    let serialized = final_log.serialize_to_string().map_err(|e| {
        GitAiError::Generic(format!("failed to serialize squash authorship log: {}", e))
    })?;
    write_authorship_note(repo, squash_commit, &serialized)?;
    Ok(())
}

pub fn shift_authorship_notes(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, FileAttestation};
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};

    tracing::debug!("shift_authorship_notes: {} mappings", mappings.len());

    let mut pending_logs: HashMap<String, AuthorshipLog> = HashMap::new();
    let mut raw_fallbacks: HashMap<String, String> = HashMap::new();

    for (source_sha, new_sha) in mappings {
        // Don't overwrite existing notes on the target commit that have real attestations.
        // Empty notes (no attestations) may come from post-commit on squash merges
        // and should be overwritable by the transfer.
        if let Some(existing_raw) = read_authorship_note(repo, new_sha)? {
            if let Ok(existing_log) = AuthorshipLog::deserialize_from_string(&existing_raw) {
                if !existing_log.attestations.is_empty() {
                    continue;
                }
            } else {
                continue;
            }
        }
        let Some(raw_note) = read_authorship_note(repo, source_sha)? else {
            continue;
        };

        let Ok(mut log) = AuthorshipLog::deserialize_from_string(&raw_note) else {
            raw_fallbacks.entry(new_sha.clone()).or_insert(raw_note);
            continue;
        };

        // Detect renames via diff-tree
        let diff_result = compute_diff_tree(repo, source_sha, new_sha).ok();
        if let Some(ref dr) = diff_result {
            for (old_path, new_path) in &dr.renames {
                for attestation in &mut log.attestations {
                    if attestation.file_path == *old_path {
                        attestation.file_path = new_path.clone();
                    }
                }
            }
        }

        // Content-based attribution transfer: for each file, read content at both
        // commits and use line-level diff to carry attributions to matching lines.
        let shifted: Vec<_> = log
            .attestations
            .iter()
            .filter_map(|fa| {
                let old_content = file_content_at_commit(repo, source_sha, &fa.file_path);
                let new_content = file_content_at_commit(repo, new_sha, &fa.file_path);

                if old_content.is_empty() && new_content.is_empty() {
                    return None;
                }
                // If file unchanged, keep attestation as-is
                if old_content == new_content {
                    return Some(fa.clone());
                }
                // If file was deleted in new commit, drop attestation
                if new_content.is_empty() {
                    return None;
                }

                let old_lines: Vec<&str> = old_content.lines().collect();
                let new_lines: Vec<&str> = new_content.lines().collect();
                let diff_ops = capture_diff_slices(&old_lines, &new_lines);

                // Build mapping: old_line_number (1-based) → new_line_number (1-based)
                let mut old_to_new: HashMap<u32, u32> = HashMap::new();
                for op in &diff_ops {
                    if let DiffOp::Equal {
                        old_index,
                        new_index,
                        len,
                    } = op
                    {
                        for i in 0..*len {
                            let old_line = (*old_index + i + 1) as u32;
                            let new_line = (*new_index + i + 1) as u32;
                            old_to_new.insert(old_line, new_line);
                        }
                    }
                }

                // Transfer attestation entries using the mapping
                let mut new_entries: Vec<AttestationEntry> = Vec::new();
                for entry in &fa.entries {
                    let mut new_ranges: Vec<LineRange> = Vec::new();
                    for range in &entry.line_ranges {
                        let (start, end) = match range {
                            LineRange::Single(l) => (*l, *l),
                            LineRange::Range(s, e) => (*s, *e),
                        };
                        for line in start..=end {
                            if let Some(&new_line) = old_to_new.get(&line) {
                                new_ranges.push(LineRange::Single(new_line));
                            }
                        }
                    }
                    if !new_ranges.is_empty() {
                        // Compact consecutive singles into ranges
                        let compacted = compact_line_ranges(new_ranges);
                        new_entries.push(AttestationEntry {
                            hash: entry.hash.clone(),
                            line_ranges: compacted,
                        });
                    }
                }

                if new_entries.is_empty() {
                    None
                } else {
                    Some(FileAttestation {
                        file_path: fa.file_path.clone(),
                        entries: new_entries,
                    })
                }
            })
            .collect();
        log.attestations = shifted;

        log.metadata.base_commit_sha = new_sha.clone();

        if let Some(existing) = pending_logs.get_mut(new_sha) {
            merge_authorship_logs(existing, &log);
        } else {
            pending_logs.insert(new_sha.clone(), log);
        }
    }

    for (sha, log) in &pending_logs {
        match log.serialize_to_string() {
            Ok(serialized) => write_authorship_note(repo, sha, &serialized)?,
            Err(_) => {
                if let Some(raw) = raw_fallbacks.get(sha) {
                    write_authorship_note(repo, sha, raw)?;
                }
            }
        }
    }

    for (sha, raw) in &raw_fallbacks {
        if !pending_logs.contains_key(sha) {
            write_authorship_note(repo, sha, raw)?;
        }
    }

    Ok(())
}

fn compact_line_ranges(
    ranges: Vec<crate::authorship::authorship_log::LineRange>,
) -> Vec<crate::authorship::authorship_log::LineRange> {
    use crate::authorship::authorship_log::LineRange;
    if ranges.is_empty() {
        return ranges;
    }
    let mut lines: Vec<u32> = ranges
        .iter()
        .flat_map(|r| match r {
            LineRange::Single(l) => vec![*l],
            LineRange::Range(s, e) => (*s..=*e).collect(),
        })
        .collect();
    lines.sort_unstable();
    lines.dedup();

    let mut result = Vec::new();
    let mut start = lines[0];
    let mut end = lines[0];
    for &line in &lines[1..] {
        if line == end + 1 {
            end = line;
        } else {
            if start == end {
                result.push(LineRange::Single(start));
            } else {
                result.push(LineRange::Range(start, end));
            }
            start = line;
            end = line;
        }
    }
    if start == end {
        result.push(LineRange::Single(start));
    } else {
        result.push(LineRange::Range(start, end));
    }
    result
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

pub fn derive_mappings_from_range_diff(
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

    // Full squash: all old commits collapsed into one new commit
    if is_full_squash(repo, &base, old_tip, new_tip, onto_hint) {
        // Map ALL old commits to new_tip so their notes get merged
        let old_commits = list_commits_in_range(repo, &base, old_tip);
        if old_commits.is_empty() {
            return Ok(vec![(old_tip.to_string(), new_tip.to_string())]);
        }
        return Ok(old_commits
            .into_iter()
            .map(|src| (src, new_tip.to_string()))
            .collect());
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

    let mut merge_mappings: Vec<(String, String)> = Vec::new();

    for old_merge in &old_merges {
        // Only map merges that have authorship notes
        let has_note = read_authorship_note(repo, old_merge)?.is_some();
        if !has_note {
            continue;
        }

        let old_parents = get_commit_parents(repo, old_merge);
        if old_parents.is_empty() {
            continue;
        }

        // For each new merge, check if its parents are the mapped equivalents of old_merge's parents
        for new_merge in &new_merges {
            // Skip if already used in a mapping
            if merge_mappings.iter().any(|(_, n)| n == new_merge) {
                continue;
            }

            let new_parents = get_commit_parents(repo, new_merge);
            if new_parents.len() != old_parents.len() {
                continue;
            }

            let all_match = old_parents.iter().zip(new_parents.iter()).all(|(op, np)| {
                // Check in existing_mappings
                if existing_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                // Check in already-matched merge_mappings
                if merge_mappings.iter().any(|(o, n)| o == op && n == np) {
                    return true;
                }
                // Unmapped parent that stayed the same (e.g., shared ancestor)
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

pub fn migrate_working_log_if_needed(
    repo: &Repository,
    mappings: &[(String, String)],
) -> Result<(), GitAiError> {
    let working_logs_dir = &repo.storage.working_logs;

    // Get current HEAD to identify the tip mapping
    let current_head = {
        let mut args = repo.global_args_for_exec();
        args.extend(["rev-parse".to_string(), "HEAD".to_string()]);
        exec_git_allow_nonzero(&args)
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };

    for (source, new_sha) in mappings {
        let old_dir = working_logs_dir.join(source);
        if !old_dir.exists() {
            continue;
        }

        if *new_sha == current_head {
            // Tip mapping — migrate
            let new_dir = working_logs_dir.join(new_sha);
            if old_dir == new_dir {
                continue;
            }

            let diff_result = compute_diff_tree(repo, source, new_sha);
            match diff_result {
                Ok(ref dr) if dr.hunks_by_file.is_empty() && dr.renames.is_empty() => {
                    // No content changes — simple rename
                    let _ = std::fs::rename(&old_dir, &new_dir);
                }
                Ok(dr) => {
                    let _ = migrate_working_log_with_shifts(&old_dir, &new_dir, &dr);
                    let _ = std::fs::remove_dir_all(&old_dir);
                }
                Err(_) => {
                    // diff-tree failed — simple rename as fallback
                    let _ = std::fs::rename(&old_dir, &new_dir);
                }
            }
        } else {
            // Intermediate commit — remove stale working log
            let _ = std::fs::remove_dir_all(&old_dir);
        }
    }

    Ok(())
}

fn migrate_working_log_with_shifts(
    old_dir: &std::path::Path,
    new_dir: &std::path::Path,
    diff_result: &DiffTreeResult,
) -> Result<(), GitAiError> {
    use crate::authorship::hunk_shift::apply_hunk_shifts_to_line_attributions;
    use crate::git::repo_storage::InitialAttributions;

    let initial_path = old_dir.join("INITIAL");
    if !initial_path.exists() {
        // No INITIAL — just rename the directory
        std::fs::rename(old_dir, new_dir)?;
        return Ok(());
    }

    let content = std::fs::read_to_string(&initial_path)?;
    let mut initial: InitialAttributions = serde_json::from_str(&content)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse INITIAL: {}", e)))?;

    // Apply renames to file keys
    for (old_path, new_path) in &diff_result.renames {
        if let Some(attrs) = initial.files.remove(old_path) {
            initial.files.insert(new_path.clone(), attrs);
        }
        if let Some(blob) = initial.file_blobs.remove(old_path) {
            initial.file_blobs.insert(new_path.clone(), blob);
        }
    }

    // Shift line attributions for files with hunks
    for (file_path, hunks) in &diff_result.hunks_by_file {
        if let Some(attrs) = initial.files.get_mut(file_path) {
            *attrs = apply_hunk_shifts_to_line_attributions(attrs, hunks);
        }
        // Clear stale blob SHA
        initial.file_blobs.remove(file_path);
    }

    // Write to new directory
    std::fs::create_dir_all(new_dir)?;
    let serialized = serde_json::to_string(&initial)
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize INITIAL: {}", e)))?;
    std::fs::write(new_dir.join("INITIAL"), serialized)?;

    // Copy checkpoints.jsonl and blobs/ as-is
    let checkpoints_src = old_dir.join("checkpoints.jsonl");
    if checkpoints_src.exists() {
        let _ = std::fs::copy(&checkpoints_src, new_dir.join("checkpoints.jsonl"));
    }

    let blobs_src = old_dir.join("blobs");
    if blobs_src.exists() {
        copy_dir_recursive(&blobs_src, &new_dir.join("blobs"))?;
    }

    Ok(())
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), GitAiError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

fn compute_diff_tree(
    repo: &Repository,
    source_sha: &str,
    new_sha: &str,
) -> Result<DiffTreeResult, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "diff-tree".to_string(),
        "-p".to_string(),
        "-U0".to_string(),
        "-M".to_string(),
        "--no-color".to_string(),
        source_sha.to_string(),
        new_sha.to_string(),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_diff_tree_output(&stdout))
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

fn read_authorship_note(repo: &Repository, sha: &str) -> Result<Option<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "notes".to_string(),
        "--ref=ai".to_string(),
        "show".to_string(),
        sha.to_string(),
    ]);

    let output = exec_git_allow_nonzero(&args)?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        Ok(None)
    }
}

fn write_authorship_note(repo: &Repository, sha: &str, content: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "notes".to_string(),
        "--ref=ai".to_string(),
        "add".to_string(),
        "-f".to_string(),
        "-m".to_string(),
        content.to_string(),
        sha.to_string(),
    ]);

    exec_git(&args)?;
    Ok(())
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
}
