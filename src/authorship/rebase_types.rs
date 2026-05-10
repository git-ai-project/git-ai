use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Default)]
pub struct PromptLineMetrics {
    pub accepted_lines: u32,
    pub overridden_lines: u32,
}

/// Pre-loaded note data for all commits involved in a rebase.
/// Eliminates redundant git subprocess calls by reading everything once upfront.
pub struct RebaseNoteCache {
    /// Which new commits already have authorship notes (to skip reprocessing)
    pub new_commits_with_notes: HashSet<String>,
    /// Note blob OIDs for original commits (commit_sha → blob_oid)
    pub original_note_blob_oids: HashMap<String, String>,
    /// Parsed note contents for original commits (commit_sha → raw_content)
    pub original_note_contents: HashMap<String, String>,
    /// AI-touched file paths extracted from original commit notes
    pub ai_touched_files: HashSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CommitTrackedDelta {
    pub changed_files: HashSet<String>,
    pub file_to_blob_oid: HashMap<String, Option<String>>,
}

#[derive(Debug, Default, Clone)]
pub struct CommitObjectMetadata {
    pub tree_oid: String,
}

pub type ChangedFileContents = (HashSet<String>, HashMap<String, String>);
pub type ChangedFileContentsByCommit = HashMap<String, ChangedFileContents>;

/// Result of parsing diff-tree output: per-commit deltas and the set of all blob OIDs needed.
pub struct DiffTreeResult {
    pub commit_deltas: Vec<(String, CommitTrackedDelta)>,
    pub all_blob_oids: Vec<String>, // sorted, deduplicated
}

impl DiffTreeResult {
    pub fn all_changed_files(&self) -> HashSet<String> {
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
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Content of `+` lines from the unified diff output for this hunk.
    /// Used by the hunk-based attribution path to stamp AI attribution on
    /// newly-inserted/replaced lines via content-matching.
    pub added_lines: Vec<String>,
}

/// Per-commit, per-file hunk information extracted from `git diff-tree -p -U0`.
/// Maps `commit_sha → file_path → Vec<DiffHunk>`.
pub type HunksByCommitAndFile = HashMap<String, HashMap<String, Vec<DiffHunk>>>;
