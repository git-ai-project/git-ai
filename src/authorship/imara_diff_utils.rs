//! Helper utilities wrapping imara-diff to provide a similar API to the `similar` crate.
//!
//! imara-diff matches git's diff output more closely than `similar`, which is important
//! for accurate line attribution tracking.

use imara_diff::{Algorithm, Diff, InternedInput, TokenSource};
use std::hash::Hash;

// ============================================================================
// Byte-level diff types (replacing diff_match_patch_rs)
// ============================================================================

/// Operation type for byte-level diffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteDiffOp {
    /// Content is equal in both old and new.
    Equal,
    /// Content was deleted from old.
    Delete,
    /// Content was inserted in new.
    Insert,
}

/// A single diff segment containing an operation and the associated byte data.
#[derive(Debug, Clone)]
pub struct ByteDiff {
    op: ByteDiffOp,
    data: Vec<u8>,
}

impl ByteDiff {
    /// Create a new ByteDiff with the given operation and data.
    pub fn new(op: ByteDiffOp, data: &[u8]) -> Self {
        ByteDiff {
            op,
            data: data.to_vec(),
        }
    }

    /// Returns the operation type for this diff segment.
    pub fn op(&self) -> ByteDiffOp {
        self.op
    }

    /// Returns the byte data for this diff segment.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

// ============================================================================
// DiffOp types (for line/token level diffs)
// ============================================================================

/// Represents a diff operation, similar to `similar::DiffOp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// A segment of equal elements.
    Equal {
        /// Index in old sequence where equal segment starts.
        old_index: usize,
        /// Index in new sequence where equal segment starts.
        new_index: usize,
        /// Length of the equal segment.
        len: usize,
    },
    /// A segment of deleted elements (present in old, absent in new).
    Delete {
        /// Index in old sequence where deletion starts.
        old_index: usize,
        /// Number of elements deleted.
        old_len: usize,
        /// Corresponding position in new sequence.
        new_index: usize,
    },
    /// A segment of inserted elements (absent in old, present in new).
    Insert {
        /// Corresponding position in old sequence.
        old_index: usize,
        /// Index in new sequence where insertion starts.
        new_index: usize,
        /// Number of elements inserted.
        new_len: usize,
    },
    /// A segment where elements were replaced.
    Replace {
        /// Index in old sequence where replacement starts.
        old_index: usize,
        /// Number of elements removed from old.
        old_len: usize,
        /// Index in new sequence where replacement starts.
        new_index: usize,
        /// Number of elements added to new.
        new_len: usize,
    },
}

/// A token source adapter for slices, enabling imara_diff to work with arbitrary slices.
struct SliceTokenSource<'a, T> {
    slice: &'a [T],
}

impl<'a, T> SliceTokenSource<'a, T> {
    fn new(slice: &'a [T]) -> Self {
        SliceTokenSource { slice }
    }
}

impl<'a, T: Clone + Hash + Eq> TokenSource for SliceTokenSource<'a, T> {
    type Token = T;
    type Tokenizer = std::iter::Cloned<std::slice::Iter<'a, T>>;

    fn tokenize(&self) -> Self::Tokenizer {
        self.slice.iter().cloned()
    }

    fn estimate_tokens(&self) -> u32 {
        self.slice.len() as u32
    }
}

/// Computes the diff between two slices and returns a vector of diff operations.
///
/// This function uses imara-diff with the Myers algorithm.
///
/// # Arguments
/// * `old` - The original slice
/// * `new` - The new slice
///
/// # Returns
/// A vector of `DiffOp` representing the changes between old and new.
pub fn capture_diff_slices<T: Hash + Eq + Clone>(old: &[T], new: &[T]) -> Vec<DiffOp> {
    let input = InternedInput::new(SliceTokenSource::new(old), SliceTokenSource::new(new));
    let diff = Diff::compute(Algorithm::Myers, &input);
    hunks_to_diff_ops(&diff, old.len(), new.len())
}

/// Represents a change in a line-based diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineChangeTag {
    /// Line was inserted.
    Insert,
    /// Line was deleted.
    Delete,
    /// Line was unchanged.
    Equal,
}

/// A single line change from a diff.
#[derive(Debug, Clone)]
pub struct LineChange<'a> {
    tag: LineChangeTag,
    value: &'a str,
}

impl<'a> LineChange<'a> {
    /// Returns the tag indicating what kind of change this is.
    pub fn tag(&self) -> &LineChangeTag {
        &self.tag
    }

    /// Returns the line content (including trailing newline if present).
    pub fn value(&self) -> &'a str {
        self.value
    }
}

/// Computes line changes between two strings, similar to `TextDiff::iter_all_changes`.
///
/// Uses imara-diff with Myers algorithm and git-like post-processing.
///
/// # Arguments
/// * `old` - The original string
/// * `new` - The new string
///
/// # Returns
/// A vector of `LineChange` representing each line's change status.
pub fn compute_line_changes<'a>(old: &'a str, new: &'a str) -> Vec<LineChange<'a>> {
    let old_lines: Vec<&str> = split_lines_with_terminators(old);
    let new_lines: Vec<&str> = split_lines_with_terminators(new);

    // Use imara_diff with &str which implements TokenSource (tokenizes by lines)
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_lines(&input);

    let mut changes = Vec::new();
    let mut old_idx: usize = 0;
    let mut new_idx: usize = 0;

    for hunk in diff.hunks() {
        let hunk_old_start = hunk.before.start as usize;
        let hunk_old_end = hunk.before.end as usize;
        let hunk_new_start = hunk.after.start as usize;
        let hunk_new_end = hunk.after.end as usize;

        // Add equal lines before this hunk
        while old_idx < hunk_old_start && new_idx < hunk_new_start {
            if let Some(line) = new_lines.get(new_idx) {
                changes.push(LineChange {
                    tag: LineChangeTag::Equal,
                    value: line,
                });
            }
            old_idx += 1;
            new_idx += 1;
        }

        // Add deleted lines
        for i in hunk_old_start..hunk_old_end {
            if let Some(line) = old_lines.get(i) {
                changes.push(LineChange {
                    tag: LineChangeTag::Delete,
                    value: line,
                });
            }
        }

        // Add inserted lines
        for i in hunk_new_start..hunk_new_end {
            if let Some(line) = new_lines.get(i) {
                changes.push(LineChange {
                    tag: LineChangeTag::Insert,
                    value: line,
                });
            }
        }

        old_idx = hunk_old_end;
        new_idx = hunk_new_end;
    }

    // Add remaining equal lines after last hunk
    while new_idx < new_lines.len() {
        if let Some(line) = new_lines.get(new_idx) {
            changes.push(LineChange {
                tag: LineChangeTag::Equal,
                value: line,
            });
        }
        new_idx += 1;
    }

    changes
}

/// Splits a string into lines, preserving line terminators.
fn split_lines_with_terminators(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;

    for (idx, ch) in s.char_indices() {
        if ch == '\n' {
            lines.push(&s[start..idx + 1]);
            start = idx + 1;
        }
    }

    // Handle last line without trailing newline
    if start < s.len() {
        lines.push(&s[start..]);
    }

    lines
}

/// Converts imara-diff hunks to a vector of DiffOp.
fn hunks_to_diff_ops(diff: &Diff, old_len: usize, _new_len: usize) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut old_idx: usize = 0;
    let mut new_idx: usize = 0;

    for hunk in diff.hunks() {
        let hunk_old_start = hunk.before.start as usize;
        let hunk_old_end = hunk.before.end as usize;
        let hunk_new_start = hunk.after.start as usize;
        let hunk_new_end = hunk.after.end as usize;

        // Add Equal operation for unchanged content before this hunk
        if old_idx < hunk_old_start {
            let equal_len = hunk_old_start - old_idx;
            ops.push(DiffOp::Equal {
                old_index: old_idx,
                new_index: new_idx,
                len: equal_len,
            });
            let _ = new_idx + equal_len;
        }

        // Determine the type of change in this hunk
        let old_hunk_len = hunk_old_end - hunk_old_start;
        let new_hunk_len = hunk_new_end - hunk_new_start;

        if old_hunk_len > 0 && new_hunk_len > 0 {
            // Replace: both old and new have content
            ops.push(DiffOp::Replace {
                old_index: hunk_old_start,
                old_len: old_hunk_len,
                new_index: hunk_new_start,
                new_len: new_hunk_len,
            });
        } else if old_hunk_len > 0 {
            // Delete: only old has content
            ops.push(DiffOp::Delete {
                old_index: hunk_old_start,
                old_len: old_hunk_len,
                new_index: hunk_new_start,
            });
        } else if new_hunk_len > 0 {
            // Insert: only new has content
            ops.push(DiffOp::Insert {
                old_index: hunk_old_start,
                new_index: hunk_new_start,
                new_len: new_hunk_len,
            });
        }

        old_idx = hunk_old_end;
        new_idx = hunk_new_end;
    }

    // Add final Equal operation for unchanged content after last hunk
    if old_idx < old_len {
        let remaining = old_len - old_idx;
        ops.push(DiffOp::Equal {
            old_index: old_idx,
            new_index: new_idx,
            len: remaining,
        });
    }

    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_diff_slices_simple() {
        let old = vec!["a", "b", "c"];
        let new = vec!["a", "x", "c"];

        let ops = capture_diff_slices(&old, &new);

        assert_eq!(ops.len(), 3);
        assert!(matches!(
            ops[0],
            DiffOp::Equal {
                old_index: 0,
                new_index: 0,
                len: 1
            }
        ));
        assert!(matches!(
            ops[1],
            DiffOp::Replace {
                old_index: 1,
                old_len: 1,
                new_index: 1,
                new_len: 1
            }
        ));
        assert!(matches!(
            ops[2],
            DiffOp::Equal {
                old_index: 2,
                new_index: 2,
                len: 1
            }
        ));
    }

    #[test]
    fn test_capture_diff_slices_insert() {
        let old = vec!["a", "c"];
        let new = vec!["a", "b", "c"];

        let ops = capture_diff_slices(&old, &new);

        assert_eq!(ops.len(), 3);
        assert!(matches!(
            ops[0],
            DiffOp::Equal {
                old_index: 0,
                new_index: 0,
                len: 1
            }
        ));
        assert!(matches!(
            ops[1],
            DiffOp::Insert {
                old_index: 1,
                new_index: 1,
                new_len: 1
            }
        ));
        assert!(matches!(
            ops[2],
            DiffOp::Equal {
                old_index: 1,
                new_index: 2,
                len: 1
            }
        ));
    }

    #[test]
    fn test_capture_diff_slices_delete() {
        let old = vec!["a", "b", "c"];
        let new = vec!["a", "c"];

        let ops = capture_diff_slices(&old, &new);

        assert_eq!(ops.len(), 3);
        assert!(matches!(
            ops[0],
            DiffOp::Equal {
                old_index: 0,
                new_index: 0,
                len: 1
            }
        ));
        assert!(matches!(
            ops[1],
            DiffOp::Delete {
                old_index: 1,
                old_len: 1,
                new_index: 1
            }
        ));
        assert!(matches!(
            ops[2],
            DiffOp::Equal {
                old_index: 2,
                new_index: 1,
                len: 1
            }
        ));
    }

    #[test]
    fn test_compute_line_changes() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\n";

        let changes = compute_line_changes(old, new);

        let tags: Vec<_> = changes.iter().map(|c| c.tag().clone()).collect();
        assert_eq!(
            tags,
            vec![
                LineChangeTag::Equal,
                LineChangeTag::Delete,
                LineChangeTag::Insert,
                LineChangeTag::Equal,
            ]
        );
    }

    #[test]
    fn test_compute_line_changes_insert_only() {
        let old = "line1\nline2\n";
        let new = "line1\nline2\nline3\n";

        let changes = compute_line_changes(old, new);

        let tags: Vec<_> = changes.iter().map(|c| c.tag().clone()).collect();
        assert_eq!(
            tags,
            vec![
                LineChangeTag::Equal,
                LineChangeTag::Equal,
                LineChangeTag::Insert,
            ]
        );
    }

    #[test]
    fn test_split_lines_with_terminators() {
        let s = "line1\nline2\nline3";
        let lines = split_lines_with_terminators(s);
        assert_eq!(lines, vec!["line1\n", "line2\n", "line3"]);

        let s_trailing = "line1\nline2\n";
        let lines_trailing = split_lines_with_terminators(s_trailing);
        assert_eq!(lines_trailing, vec!["line1\n", "line2\n"]);
    }
}
