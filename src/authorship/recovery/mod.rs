//! Post-commit attribution recovery.
//!
//! After the initial `AuthorshipLog` is built, some committed lines may still
//! have no attestation ("unknown"/untracked). This module runs an ordered
//! pipeline of [`RecoverySolver`]s that attribute those lines using
//! out-of-band signals (bash-checkpoint timing, adjacency to AI code, …).
//!
//! The shared [`unknown_lines`] helper computes, per file, the committed line
//! numbers that have no attestation entry. It is used both here and by
//! `background_agent` so the two stay in lock-step.

use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use std::collections::{HashMap, HashSet};

/// Per-file committed line numbers that have no attestation entry ("unknown").
///
/// `committed_hunks` is the set of added lines per file in the commit; any line
/// in there that is not covered by an existing `AttestationEntry` is returned
/// (sorted, deduped). Files with no unknown lines are omitted from the result.
pub fn unknown_lines(
    authorship_log: &AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> HashMap<String, Vec<u32>> {
    let mut attributed: HashMap<&str, HashSet<u32>> = HashMap::new();
    for fa in &authorship_log.attestations {
        let set = attributed.entry(fa.file_path.as_str()).or_default();
        for entry in &fa.entries {
            for range in &entry.line_ranges {
                for line in range.expand() {
                    set.insert(line);
                }
            }
        }
    }

    let mut out = HashMap::new();
    for (file, ranges) in committed_hunks {
        let existing = attributed.get(file.as_str());
        let mut unknown: Vec<u32> = Vec::new();
        for range in ranges {
            for line in range.expand() {
                if existing.is_none_or(|s| !s.contains(&line)) {
                    unknown.push(line);
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort_unstable();
            unknown.dedup();
            out.insert(file.clone(), unknown);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
    use std::collections::HashMap;

    #[test]
    fn test_unknown_lines_subtracts_attributed() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs")
            .add_entry(AttestationEntry::new("hash1".into(), vec![LineRange::Range(1, 3)]));
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 5)]);
        let unknown = unknown_lines(&log, &committed);
        assert_eq!(unknown.get("a.rs").unwrap(), &vec![4, 5]);
    }

    #[test]
    fn test_unknown_lines_all_attributed_omits_file() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs")
            .add_entry(AttestationEntry::new("hash1".into(), vec![LineRange::Range(1, 5)]));
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 5)]);
        let unknown = unknown_lines(&log, &committed);
        assert!(unknown.is_empty());
    }
}
