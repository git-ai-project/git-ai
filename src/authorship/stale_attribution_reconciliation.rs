//! Reconciles stale checkpoint attributions against immutable committed-tree content.

use crate::authorship::authorship_log::{HumanRecord, LineRange};
use crate::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, generate_human_short_hash,
};
use crate::authorship::imara_diff_utils::content_eq_ignoring_line_endings;
use crate::authorship::virtual_attribution::diff_hunks_between_contents;
use std::collections::{HashMap, HashSet};

/// Reconcile attributed checkpoint content against the content committed by Git.
///
/// A checkpoint records the file contents when an agent finishes an edit. If a human replaces
/// part of that edit before committing without firing a checkpoint, the working log otherwise
/// projects the old AI attribution onto the replacement. Remove attestation entries covering
/// the changed committed lines and explicitly attest them as known human so attribution recovery
/// cannot infer the adjacent AI ownership back onto them.
///
/// `committed_hunks` bounds reconciliation to the commit being finalized. This is important for
/// delayed daemon processing where the working-log base can predate unrelated ref movement.
pub(crate) fn reconcile_stale_attributions(
    authorship_log: &mut AuthorshipLog,
    observed_snapshot: &HashMap<String, String>,
    checkpointed_contents: &HashMap<String, Vec<String>>,
    parent_snapshot: &HashMap<String, String>,
    committed_snapshot: &HashMap<String, String>,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
    human_author: &str,
) {
    let mut human_lines_by_file = Vec::new();

    for file_attestation in &mut authorship_log.attestations {
        let Some(observed_content) = observed_snapshot.get(&file_attestation.file_path) else {
            continue;
        };
        let Some(committed_content) = committed_snapshot.get(&file_attestation.file_path) else {
            continue;
        };
        let Some(parent_content) = parent_snapshot.get(&file_attestation.file_path) else {
            continue;
        };
        if content_eq_ignoring_line_endings(observed_content, committed_content) {
            continue;
        }
        if checkpointed_contents
            .get(&file_attestation.file_path)
            .is_some_and(|contents| {
                contents
                    .iter()
                    .any(|content| content_eq_ignoring_line_endings(content, committed_content))
            })
        {
            continue;
        }

        let hunks = diff_hunks_between_contents(observed_content, committed_content);
        if hunks.is_empty() {
            continue;
        }
        let Some(committed_ranges) = committed_hunks.get(&file_attestation.file_path) else {
            continue;
        };
        let committed_lines: HashSet<u32> = committed_ranges
            .iter()
            .flat_map(LineRange::expand)
            .collect();
        let modified_lines: HashSet<u32> =
            diff_hunks_between_contents(parent_content, committed_content)
                .into_iter()
                .filter(|hunk| hunk.old_count > 0)
                .flat_map(|hunk| hunk.new_start..hunk.new_start.saturating_add(hunk.new_count))
                .collect();
        let changed_lines = hunks
            .iter()
            .filter(|hunk| hunk.old_count > 0 && hunk.old_count == hunk.new_count)
            .flat_map(|hunk| hunk.new_start..hunk.new_start.saturating_add(hunk.new_count))
            .filter(|line| committed_lines.contains(line))
            .filter(|line| modified_lines.contains(line))
            .collect::<Vec<_>>();
        if !changed_lines.is_empty() {
            let changed_ranges = LineRange::compress_lines(&changed_lines);
            for entry in &mut file_attestation.entries {
                entry.remove_line_ranges(&changed_ranges);
            }
            file_attestation
                .entries
                .retain(|entry| !entry.line_ranges.is_empty());
            human_lines_by_file.push((file_attestation.file_path.clone(), changed_lines));
        }
    }

    authorship_log
        .attestations
        .retain(|file_attestation| !file_attestation.entries.is_empty());

    if human_lines_by_file.is_empty() {
        return;
    }

    let human_id = generate_human_short_hash(human_author);
    authorship_log
        .metadata
        .humans
        .entry(human_id.clone())
        .or_insert_with(|| HumanRecord {
            author: human_author.to_string(),
        });
    for (file_path, lines) in human_lines_by_file {
        let ranges = LineRange::compress_lines(&lines);
        if ranges.is_empty() {
            continue;
        }
        authorship_log
            .get_or_create_file(&file_path)
            .add_entry(AttestationEntry::new(human_id.clone(), ranges));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_checkpointed_ai_line_becomes_known_human() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("test.txt")
            .add_entry(AttestationEntry::new(
                "ai".to_string(),
                vec![LineRange::Single(1), LineRange::Single(2)],
            ));
        let observed = HashMap::from([("test.txt".to_string(), "AI one\nAI two\n".to_string())]);
        let parent = HashMap::from([("test.txt".to_string(), "base\nAI two\n".to_string())]);
        let committed =
            HashMap::from([("test.txt".to_string(), "human one\nAI two\n".to_string())]);
        let hunks = HashMap::from([("test.txt".to_string(), vec![LineRange::Single(1)])]);
        let checkpointed = HashMap::new();

        reconcile_stale_attributions(
            &mut log,
            &observed,
            &checkpointed,
            &parent,
            &committed,
            &hunks,
            "Jane Doe",
        );

        let entries = &log.attestations[0].entries;
        assert_eq!(entries[0].line_ranges, vec![LineRange::Single(2)]);
        assert_eq!(entries[1].hash, generate_human_short_hash("Jane Doe"));
        assert_eq!(entries[1].line_ranges, vec![LineRange::Single(1)]);
    }
}
