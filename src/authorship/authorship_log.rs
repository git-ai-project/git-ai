use crate::authorship::transcript::Message;
use crate::authorship::working_log::AgentId;
use serde::{Deserialize, Serialize};
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

impl LineRange {
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
            if line == current_end + 1 {
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

/// Prompt session details stored in the top-level prompts map keyed by short hash (agent_id + tool)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub total_additions: u32,
    #[serde(default)]
    pub total_deletions: u32,
    #[serde(default)]
    pub accepted_lines: u32,
    #[serde(default)]
    pub overriden_lines: u32,
    /// Full URL to CAS-stored messages (format: {api_base_url}/cas/{hash})
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_url: Option<String>,
    /// Hash of the parent prompt record (for subagent transcripts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

impl Eq for PromptRecord {}

impl PartialOrd for PromptRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PromptRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Sort oldest to newest based on messages, additions, or deletions.
        // Uses lexicographic comparison to ensure a valid total ordering.
        self.messages
            .len()
            .cmp(&other.messages.len())
            .then_with(|| self.total_additions.cmp(&other.total_additions))
            .then_with(|| self.total_deletions.cmp(&other.total_deletions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_prompt_record(messages: usize, additions: u32, deletions: u32) -> PromptRecord {
        let agent_id = AgentId {
            tool: "test".to_string(),
            id: "test-id".to_string(),
            model: "test-model".to_string(),
        };

        let message_list = (0..messages)
            .map(|_| Message::user("test message".to_string(), None))
            .collect();

        PromptRecord {
            agent_id,
            human_author: None,
            messages: message_list,
            total_additions: additions,
            total_deletions: deletions,
            accepted_lines: 0,
            overriden_lines: 0,
            messages_url: None,
            parent_id: None,
        }
    }

    #[test]
    fn test_prompt_record_ord_equality() {
        // Two records with identical messages.len(), total_additions, and
        // total_deletions should compare as Equal even when other fields differ.
        let mut a = create_prompt_record(3, 10, 5);
        a.agent_id.tool = "tool_a".to_string();
        a.agent_id.id = "id_a".to_string();
        a.human_author = Some("alice".to_string());

        let mut b = create_prompt_record(3, 10, 5);
        b.agent_id.tool = "tool_b".to_string();
        b.agent_id.id = "id_b".to_string();
        b.human_author = Some("bob".to_string());

        assert_eq!(
            a.cmp(&b),
            std::cmp::Ordering::Equal,
            "Records with same messages.len(), total_additions, and total_deletions \
             should compare as Equal regardless of other fields"
        );
    }

    #[test]
    fn test_prompt_record_sorting() {
        let mut records = [
            create_prompt_record(5, 10, 5), // newest - has messages, additions, deletions
            create_prompt_record(0, 0, 0),  // oldest - empty
            create_prompt_record(2, 5, 3),  // middle
            create_prompt_record(0, 10, 0), // has additions
            create_prompt_record(0, 0, 5),  // has deletions
        ];

        records.sort();

        // After sorting, oldest (empty) should be first
        assert_eq!(records[0].messages.len(), 0);
        assert_eq!(records[0].total_additions, 0);
        assert_eq!(records[0].total_deletions, 0);

        // Records with activity should come after
        assert!(
            !records[1].messages.is_empty()
                || records[1].total_additions > 0
                || records[1].total_deletions > 0
        );
    }

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

    // --- PromptRecord::Ord transitivity test ---

    #[test]
    fn test_prompt_record_ord_transitivity() {
        let a = create_prompt_record(1, 0, 0); // 1 message
        let b = create_prompt_record(2, 0, 0); // 2 messages
        let c = create_prompt_record(3, 0, 0); // 3 messages

        assert!(a < b, "a should be less than b");
        assert!(b < c, "b should be less than c");
        assert!(a < c, "transitivity: a should be less than c");
    }
}
