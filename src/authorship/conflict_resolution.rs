use std::collections::{HashMap, HashSet};

use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};
use crate::git::repository::Repository;

fn normalize_line_ranges(ranges: &[LineRange]) -> Vec<LineRange> {
    let mut lines: Vec<u32> = ranges.iter().flat_map(LineRange::expand).collect();
    lines.sort_unstable();
    lines.dedup();
    LineRange::compress_lines(&lines)
}

fn subtract_line_ranges(ranges: &[LineRange], covered: &[LineRange]) -> Vec<LineRange> {
    let mut remaining = ranges.to_vec();
    for covered_range in covered {
        remaining = remaining
            .iter()
            .flat_map(|range| range.remove(covered_range))
            .collect();
        if remaining.is_empty() {
            break;
        }
    }
    normalize_line_ranges(&remaining)
}

fn line_coverage_by_file(log: &AuthorshipLog) -> HashMap<String, Vec<LineRange>> {
    let mut coverage: HashMap<String, Vec<LineRange>> = HashMap::new();
    for attestation in &log.attestations {
        let file_coverage = coverage.entry(attestation.file_path.clone()).or_default();
        for entry in &attestation.entries {
            file_coverage.extend(entry.line_ranges.clone());
        }
    }
    for ranges in coverage.values_mut() {
        *ranges = normalize_line_ranges(ranges);
    }
    coverage
}

fn attestation_metadata_key(hash: &str) -> &str {
    hash.split("::").next().unwrap_or(hash)
}

fn retain_referenced_metadata(log: &mut AuthorshipLog) {
    let mut prompt_keys = HashSet::new();
    let mut human_keys = HashSet::new();
    let mut session_keys = HashSet::new();

    for attestation in &log.attestations {
        for entry in &attestation.entries {
            let key = attestation_metadata_key(&entry.hash).to_string();
            if key.starts_with("h_") {
                human_keys.insert(key);
            } else if key.starts_with("s_") {
                session_keys.insert(key);
            } else {
                prompt_keys.insert(key);
            }
        }
    }

    log.metadata
        .prompts
        .retain(|key, _| prompt_keys.contains(key));
    log.metadata
        .humans
        .retain(|key, _| human_keys.contains(key));
    log.metadata
        .sessions
        .retain(|key, _| session_keys.contains(key));
}

fn filter_resolution_log_to_uncovered_lines(
    mut resolution_log: AuthorshipLog,
    shifted_log: &AuthorshipLog,
) -> AuthorshipLog {
    let shifted_coverage = line_coverage_by_file(shifted_log);

    for attestation in &mut resolution_log.attestations {
        let covered = shifted_coverage
            .get(&attestation.file_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for entry in &mut attestation.entries {
            entry.line_ranges = subtract_line_ranges(&entry.line_ranges, covered);
        }
        attestation
            .entries
            .retain(|entry| !entry.line_ranges.is_empty());
    }

    resolution_log
        .attestations
        .retain(|attestation| !attestation.entries.is_empty());
    retain_referenced_metadata(&mut resolution_log);
    resolution_log
}

fn merge_file_attestations(target: &mut AuthorshipLog, source: &AuthorshipLog) {
    for source_attestation in &source.attestations {
        let target_attestation = target.get_or_create_file(&source_attestation.file_path);
        for source_entry in &source_attestation.entries {
            if let Some(target_entry) = target_attestation
                .entries
                .iter_mut()
                .find(|entry| entry.hash == source_entry.hash)
            {
                target_entry
                    .line_ranges
                    .extend(source_entry.line_ranges.clone());
                target_entry.line_ranges = normalize_line_ranges(&target_entry.line_ranges);
            } else {
                let mut entry = source_entry.clone();
                entry.line_ranges = normalize_line_ranges(&entry.line_ranges);
                target_attestation.entries.push(entry);
            }
        }
    }
}

fn merge_authorship_metadata(target: &mut AuthorshipLog, source: &AuthorshipLog) {
    for (key, record) in &source.metadata.prompts {
        target
            .metadata
            .prompts
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
    for (key, record) in &source.metadata.sessions {
        target
            .metadata
            .sessions
            .entry(key.clone())
            .or_insert_with(|| record.clone());
    }
}

fn equal_line_mapping_between_commits(
    repo: &Repository,
    source_sha: &str,
    destination_sha: &str,
    file_path: &str,
) -> Option<HashMap<u32, u32>> {
    let source_content =
        String::from_utf8(repo.get_file_content(file_path, source_sha).ok()?).ok()?;
    let destination_content =
        String::from_utf8(repo.get_file_content(file_path, destination_sha).ok()?).ok()?;
    let source_lines: Vec<String> = source_content.lines().map(str::to_string).collect();
    let destination_lines: Vec<String> = destination_content.lines().map(str::to_string).collect();
    let diff_ops = capture_diff_slices(&source_lines, &destination_lines);

    let mut mapping = HashMap::new();
    for op in diff_ops {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for offset in 0..len {
                mapping.insert(
                    (old_index + offset + 1) as u32,
                    (new_index + offset + 1) as u32,
                );
            }
        }
    }
    Some(mapping)
}

fn recover_exact_source_lines_from_mapping(
    repo: &Repository,
    target: &mut AuthorshipLog,
    source_sha: &str,
    destination_sha: &str,
) {
    let Some(source_raw) = crate::git::notes_api::read_note(repo, source_sha) else {
        return;
    };
    let Ok(source_log) = AuthorshipLog::deserialize_from_string(&source_raw) else {
        return;
    };

    let mut recovered_log = AuthorshipLog::new();
    recovered_log.metadata = source_log.metadata.clone();
    let mut target_coverage = line_coverage_by_file(target);

    for source_attestation in &source_log.attestations {
        let Some(line_mapping) = equal_line_mapping_between_commits(
            repo,
            source_sha,
            destination_sha,
            &source_attestation.file_path,
        ) else {
            continue;
        };

        for source_entry in &source_attestation.entries {
            let mut mapped_lines = Vec::new();
            for source_line in source_entry.line_ranges.iter().flat_map(LineRange::expand) {
                if let Some(destination_line) = line_mapping.get(&source_line) {
                    mapped_lines.push(*destination_line);
                }
            }

            if mapped_lines.is_empty() {
                continue;
            }

            mapped_lines.sort_unstable();
            mapped_lines.dedup();
            let mapped_ranges = LineRange::compress_lines(&mapped_lines);
            let current_coverage = target_coverage
                .get(&source_attestation.file_path)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let missing_ranges = subtract_line_ranges(&mapped_ranges, current_coverage);
            if missing_ranges.is_empty() {
                continue;
            }

            target_coverage
                .entry(source_attestation.file_path.clone())
                .or_default()
                .extend(missing_ranges.clone());
            let file = recovered_log.get_or_create_file(&source_attestation.file_path);
            file.add_entry(AttestationEntry::new(
                source_entry.hash.clone(),
                missing_ranges,
            ));
        }
    }

    recovered_log
        .attestations
        .retain(|attestation| !attestation.entries.is_empty());
    retain_referenced_metadata(&mut recovered_log);
    merge_file_attestations(target, &recovered_log);
    merge_authorship_metadata(target, &recovered_log);
}

pub fn merge_conflict_resolution_authorship(
    repo: &Repository,
    existing_shifted_log: Option<AuthorshipLog>,
    resolution_log: AuthorshipLog,
    source_shas: &[String],
    commit_sha: &str,
) -> AuthorshipLog {
    let mut merged = existing_shifted_log.unwrap_or_default();
    for source_sha in source_shas {
        recover_exact_source_lines_from_mapping(repo, &mut merged, source_sha, commit_sha);
    }
    let resolution_log = filter_resolution_log_to_uncovered_lines(resolution_log, &merged);

    merge_file_attestations(&mut merged, &resolution_log);
    merge_authorship_metadata(&mut merged, &resolution_log);
    merged.metadata.base_commit_sha = commit_sha.to_string();
    merged
}
