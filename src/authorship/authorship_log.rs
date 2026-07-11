use crate::authorship::working_log::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub username: String,
    pub email: String,
}

/// Represents either a single line or a range of lines
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LineRange {
    Single(u32),
    Range(u32, u32), // start, end (inclusive)
}

pub const MAX_MATERIALIZED_LINE_COUNT: u64 = 1_000_000;

impl LineRange {
    pub fn inclusive_bounds(&self) -> (u32, u32) {
        match self {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        }
    }

    pub fn covered_line_count(&self) -> u64 {
        let (start, end) = self.inclusive_bounds();
        if start == 0 || start > end {
            0
        } else {
            u64::from(end) - u64::from(start) + 1
        }
    }

    pub fn normalize(ranges: &[LineRange]) -> Vec<LineRange> {
        let mut intervals = ranges
            .iter()
            .map(LineRange::inclusive_bounds)
            .filter(|(start, end)| *start > 0 && start <= end)
            .collect::<Vec<_>>();
        intervals.sort_unstable();

        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in intervals {
            if let Some((_, current_end)) = merged.last_mut()
                && start <= current_end.saturating_add(1)
            {
                *current_end = (*current_end).max(end);
            } else {
                merged.push((start, end));
            }
        }

        merged
            .into_iter()
            .map(|(start, end)| {
                if start == end {
                    LineRange::Single(start)
                } else {
                    LineRange::Range(start, end)
                }
            })
            .collect()
    }

    pub fn subtract_all(ranges: &[LineRange], covered: &[LineRange]) -> Vec<LineRange> {
        let ranges = LineRange::normalize(ranges);
        let covered = LineRange::normalize(covered);
        let mut remaining = Vec::new();
        let mut covered_index = 0usize;

        for range in ranges {
            let (start, end) = range.inclusive_bounds();
            while covered_index < covered.len()
                && covered[covered_index].inclusive_bounds().1 < start
            {
                covered_index += 1;
            }

            let mut cursor = Some(start);
            let mut scan_index = covered_index;
            while let Some(current) = cursor
                && scan_index < covered.len()
            {
                let (covered_start, covered_end) = covered[scan_index].inclusive_bounds();
                if covered_start > end {
                    break;
                }
                if covered_start > current {
                    let remaining_end = end.min(covered_start - 1);
                    remaining.push(if current == remaining_end {
                        LineRange::Single(current)
                    } else {
                        LineRange::Range(current, remaining_end)
                    });
                }
                cursor = if covered_end >= end || covered_end == u32::MAX {
                    None
                } else {
                    Some(current.max(covered_end + 1))
                };
                scan_index += 1;
            }

            if let Some(current) = cursor
                && current <= end
            {
                remaining.push(if current == end {
                    LineRange::Single(current)
                } else {
                    LineRange::Range(current, end)
                });
            }
        }
        remaining
    }

    pub fn intersect_all(left: &[LineRange], right: &[LineRange]) -> Vec<LineRange> {
        let left = LineRange::normalize(left);
        let right = LineRange::normalize(right);
        let mut intersections = Vec::new();
        let (mut left_index, mut right_index) = (0usize, 0usize);
        while left_index < left.len() && right_index < right.len() {
            let (left_start, left_end) = left[left_index].inclusive_bounds();
            let (right_start, right_end) = right[right_index].inclusive_bounds();
            let start = left_start.max(right_start);
            let end = left_end.min(right_end);
            if start <= end {
                intersections.push(if start == end {
                    LineRange::Single(start)
                } else {
                    LineRange::Range(start, end)
                });
            }
            if left_end <= right_end {
                left_index += 1;
            } else {
                right_index += 1;
            }
        }
        intersections
    }

    pub fn covered_lines_before(&self, line: u32) -> u64 {
        let (start, end) = self.inclusive_bounds();
        if start == 0 || start > end || line <= start {
            0
        } else {
            u64::from(end.min(line.saturating_sub(1))) - u64::from(start) + 1
        }
    }

    pub fn contains(&self, line: u32) -> bool {
        match self {
            LineRange::Single(l) => *l == line,
            LineRange::Range(start, end) => line >= *start && line <= *end,
        }
    }

    #[allow(dead_code)]
    pub fn overlaps(&self, other: &LineRange) -> bool {
        match (self, other) {
            (LineRange::Single(l1), LineRange::Single(l2)) => l1 == l2,
            (LineRange::Single(l), LineRange::Range(start, end)) => *l >= *start && *l <= *end,
            (LineRange::Range(start, end), LineRange::Single(l)) => *l >= *start && *l <= *end,
            (LineRange::Range(start1, end1), LineRange::Range(start2, end2)) => {
                start1 <= end2 && start2 <= end1
            }
        }
    }

    /// Remove a line or range from this range, returning the remaining parts
    #[allow(dead_code)]
    pub fn remove(&self, to_remove: &LineRange) -> Vec<LineRange> {
        match (self, to_remove) {
            (LineRange::Single(l), LineRange::Single(r)) => {
                if l == r {
                    vec![]
                } else {
                    vec![self.clone()]
                }
            }
            (LineRange::Single(l), LineRange::Range(start, end)) => {
                if *l >= *start && *l <= *end {
                    vec![]
                } else {
                    vec![self.clone()]
                }
            }
            (LineRange::Range(start, end), LineRange::Single(r)) => {
                if *r < *start || *r > *end {
                    vec![self.clone()]
                } else if *r == *start && *r == *end {
                    vec![]
                } else if *r == *start {
                    vec![LineRange::Range(*start + 1, *end)]
                } else if *r == *end {
                    vec![LineRange::Range(*start, *end - 1)]
                } else {
                    vec![
                        LineRange::Range(*start, *r - 1),
                        LineRange::Range(*r + 1, *end),
                    ]
                }
            }
            (LineRange::Range(start1, end1), LineRange::Range(start2, end2)) => {
                if *start2 > *end1 || *end2 < *start1 {
                    // No overlap
                    vec![self.clone()]
                } else {
                    let mut result = Vec::new();
                    // Left part
                    if *start1 < *start2 {
                        result.push(LineRange::Range(*start1, *start2 - 1));
                    }
                    // Right part
                    if *end1 > *end2 {
                        result.push(LineRange::Range(*end2 + 1, *end1));
                    }
                    result
                }
            }
        }
    }

    /// Convert a sorted list of line numbers into compressed ranges
    pub fn compress_lines(lines: &[u32]) -> Vec<LineRange> {
        if lines.is_empty() {
            return vec![];
        }

        let mut ranges = Vec::new();
        let mut current_start = lines[0];
        let mut current_end = lines[0];

        for &line in &lines[1..] {
            if line == current_end.saturating_add(1) {
                current_end = line;
            } else {
                // End current range and start new one
                if current_start == current_end {
                    ranges.push(LineRange::Single(current_start));
                } else {
                    ranges.push(LineRange::Range(current_start, current_end));
                }
                current_start = line;
                current_end = line;
            }
        }

        // Add the last range
        if current_start == current_end {
            ranges.push(LineRange::Single(current_start));
        } else {
            ranges.push(LineRange::Range(current_start, current_end));
        }

        ranges
    }

    #[allow(dead_code)]
    pub fn expand(&self) -> Vec<u32> {
        let covered_lines = self.covered_line_count();
        if covered_lines == 0 || covered_lines > MAX_MATERIALIZED_LINE_COUNT {
            return Vec::new();
        }
        match self {
            LineRange::Single(l) => vec![*l],
            LineRange::Range(start, end) => (*start..=*end).collect(),
        }
    }

    /// Shift line numbers by a given offset
    /// - For insertions: offset is positive (shift lines down/forward)
    /// - For deletions: offset is negative (shift lines up/backward)
    /// - insertion_point: the line number where the change occurred
    #[allow(dead_code)]
    pub fn shift(&self, insertion_point: u32, offset: i32) -> Option<LineRange> {
        // Helper: apply offset to a line number, returning None if result is negative
        let apply_offset = |line: u32| -> Option<u32> {
            if line >= insertion_point {
                let shifted = (line as i64) + (offset as i64);
                if shifted >= 0 && shifted <= u32::MAX as i64 {
                    Some(shifted as u32)
                } else {
                    None
                }
            } else {
                Some(line)
            }
        };

        match self {
            LineRange::Single(l) => {
                let new_line = apply_offset(*l)?;
                Some(LineRange::Single(new_line))
            }
            LineRange::Range(start, end) => {
                let new_start = apply_offset(*start)?;
                let new_end = apply_offset(*end)?;

                // Ensure the range is still valid
                if new_start <= new_end {
                    if new_start == new_end {
                        Some(LineRange::Single(new_start))
                    } else {
                        Some(LineRange::Range(new_start, new_end))
                    }
                } else {
                    None
                }
            }
        }
    }
}

impl fmt::Display for LineRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineRange::Single(l) => write!(f, "{}", l),
            LineRange::Range(start, end) => write!(f, "[{}, {}]", start, end),
        }
    }
}

/// Identity record for a known human author attested by an IDE extension
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanRecord {
    /// Git committer identity: "Alice Smith <alice@example.com>"
    pub author: String,
}

/// Prompt session details stored in the top-level prompts map keyed by short hash (agent_id + tool)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_url: Option<String>,
    #[serde(default)]
    pub total_additions: u32,
    #[serde(default)]
    pub total_deletions: u32,
    #[serde(default)]
    pub accepted_lines: u32,
    #[serde(default)]
    pub overriden_lines: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}

/// Session record for lightweight session tracking without stats
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}

impl SessionRecord {
    /// Convert to a PromptRecord (with zeroed stats) for backwards-compatible lookup
    pub fn to_prompt_record(&self) -> PromptRecord {
        PromptRecord {
            agent_id: self.agent_id.clone(),
            human_author: self.human_author.clone(),
            messages_url: None,
            total_additions: 0,
            total_deletions: 0,
            accepted_lines: 0,
            overriden_lines: 0,
            custom_attributes: self.custom_attributes.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- LineRange::shift regression tests ---

    #[test]
    fn test_shift_single_underflow_returns_none() {
        // Single(5) at insertion_point=3 with offset=-10: 5 >= 3, so shifted = 5 + (-10) = -5 => None
        let result = LineRange::Single(5).shift(3, -10);
        assert_eq!(result, None);
    }

    #[test]
    fn test_shift_range_zero_offset_identity() {
        // Zero offset should be the identity transform
        let result = LineRange::Range(10, 20).shift(5, 0);
        assert_eq!(result, Some(LineRange::Range(10, 20)));
    }

    #[test]
    fn test_shift_range_partial_underflow() {
        // Range(2, 10) at insertion_point=0, offset=-5:
        //   start: 2 >= 0, so 2 + (-5) = -3 => None (apply_offset fails on start)
        let result = LineRange::Range(2, 10).shift(0, -5);
        assert_eq!(result, None);
    }

    #[test]
    fn test_shift_range_collapses_to_single() {
        // Range(10, 11) at insertion_point=11, offset=-1:
        //   start: 10 < 11, so stays 10
        //   end:   11 >= 11, so 11 + (-1) = 10
        //   10 == 10 => collapses to Single(10)
        let result = LineRange::Range(10, 11).shift(11, -1);
        assert_eq!(result, Some(LineRange::Single(10)));
    }

    #[test]
    fn test_shift_single_below_insertion_unchanged() {
        // Single(3) with insertion_point=5: 3 < 5, so line is unchanged
        let result = LineRange::Single(3).shift(5, 10);
        assert_eq!(result, Some(LineRange::Single(3)));
    }

    #[test]
    fn test_shift_single_large_value_i64_arithmetic() {
        // Single(u32::MAX) at insertion_point=0, offset=1:
        //   u32::MAX >= 0, so shifted = (u32::MAX as i64) + 1 = 4294967296
        //   shifted >= 0, so Some(4294967296 as u32) which wraps to 0
        //   This verifies the i64 arithmetic path doesn't panic.
        let result = LineRange::Single(u32::MAX).shift(0, 1);
        assert_eq!(
            result, None,
            "u32::MAX + 1 should overflow u32 and return None"
        );
    }

    #[test]
    fn oversized_line_range_expansion_fails_closed() {
        assert!(LineRange::Range(1, 1_000_001).expand().is_empty());
    }

    #[test]
    fn line_range_set_operations_stay_compact_at_u32_boundary() {
        let normalized = LineRange::normalize(&[
            LineRange::Range(10, 20),
            LineRange::Range(1, 12),
            LineRange::Single(u32::MAX),
        ]);
        assert_eq!(
            normalized,
            vec![LineRange::Range(1, 20), LineRange::Single(u32::MAX)]
        );

        assert_eq!(
            LineRange::subtract_all(
                &[LineRange::Range(1, u32::MAX)],
                &[LineRange::Range(2, u32::MAX - 1)],
            ),
            vec![LineRange::Single(1), LineRange::Single(u32::MAX)]
        );
        assert_eq!(
            LineRange::intersect_all(
                &[LineRange::Range(1, 10), LineRange::Range(20, 30)],
                &[LineRange::Range(5, 25)],
            ),
            vec![LineRange::Range(5, 10), LineRange::Range(20, 25)]
        );
    }
}
