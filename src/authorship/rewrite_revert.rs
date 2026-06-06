use std::collections::{HashMap, HashSet};

use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, FileAttestation,
};
use crate::authorship::hunk_shift::apply_hunk_shifts_to_file_attestation;
use crate::authorship::rewrite::compute_diff_trees_batch;
use crate::error::GitAiError;
use crate::git::notes_api;
use crate::git::repository::{Repository, exec_git};

/// Handle a `git revert` commit by reconstructing attribution for re-introduced lines.
///
/// Uses `git-ai blame` on the grandparent to determine correct attribution for
/// lines that the revert re-introduces. This ensures human-overridden lines are
/// correctly identified as human even if older commits had AI attestation.
pub fn handle_revert_commit(
    repo: &Repository,
    revert_commit: &str,
    parent: Option<&str>,
    reverted_commit: Option<&str>,
) -> Result<(), GitAiError> {
    let parent_sha = match parent {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            let mut args = repo.global_args_for_exec();
            args.extend_from_slice(&["rev-parse".to_string(), format!("{}~1", revert_commit)]);
            let output = exec_git(&args)?;
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
    };

    let source_base_sha = if let Some(reverted_commit) = reverted_commit {
        match first_parent_sha(repo, reverted_commit) {
            Ok(parent) => parent,
            Err(_) => return Ok(()),
        }
    } else {
        // Compatibility for older normalized commands that did not carry the
        // reverted source commit. This is only correct for `git revert HEAD`.
        match first_parent_sha(repo, &parent_sha) {
            Ok(parent) => parent,
            Err(_) => return Ok(()),
        }
    };

    if source_base_sha.is_empty() {
        return Ok(());
    }

    // Find lines added by the revert relative to its parent
    let added_lines = repo.diff_added_lines(&parent_sha, revert_commit, None)?;
    if added_lines.is_empty() {
        return Ok(());
    }

    let notes = notes_api::read_notes_batch(repo, std::slice::from_ref(&source_base_sha))?;
    let Some(source_note) = notes.get(&source_base_sha) else {
        return Ok(());
    };
    let mut log = AuthorshipLog::deserialize_from_string(source_note)
        .map_err(|error| GitAiError::Generic(format!("invalid source revert note: {}", error)))?;

    let diff_results = compute_diff_trees_batch(
        repo,
        &[(source_base_sha.clone(), revert_commit.to_string())],
    )?;
    let Some(diff_result) = diff_results.first() else {
        return Ok(());
    };
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

    log.metadata.base_commit_sha = revert_commit.to_string();
    log.attestations = log
        .attestations
        .iter()
        .filter_map(|file| clip_file_attestation_to_lines(file, &added_lines))
        .collect();
    if log.attestations.is_empty() {
        return Ok(());
    }

    let note_str = log.serialize_to_string().map_err(|_| {
        GitAiError::Generic("Failed to serialize revert authorship log".to_string())
    })?;

    notes_api::write_notes_batch(repo, &[(revert_commit.to_string(), note_str)])?;
    Ok(())
}

fn clip_file_attestation_to_lines(
    file: &FileAttestation,
    added_lines: &HashMap<String, Vec<u32>>,
) -> Option<FileAttestation> {
    let target_lines = added_lines.get(&file.file_path)?;
    let target_lines = target_lines.iter().copied().collect::<HashSet<_>>();
    let mut entries = Vec::new();

    for entry in &file.entries {
        let mut lines = entry
            .line_ranges
            .iter()
            .flat_map(LineRange::expand)
            .filter(|line| target_lines.contains(line))
            .collect::<Vec<_>>();
        if lines.is_empty() {
            continue;
        }
        lines.sort_unstable();
        lines.dedup();
        entries.push(AttestationEntry::new(
            entry.hash.clone(),
            LineRange::compress_lines(&lines),
        ));
    }

    (!entries.is_empty()).then(|| FileAttestation {
        file_path: file.file_path.clone(),
        entries,
    })
}

fn first_parent_sha(repo: &Repository, commit_sha: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "rev-parse".to_string(),
        "--verify".to_string(),
        format!("{}^1", commit_sha),
    ]);
    let output = exec_git(&args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
