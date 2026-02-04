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
        match self {
            LineRange::Single(l) => {
                if *l >= insertion_point {
                    let new_line = (*l as i32 + offset) as u32;
                    Some(LineRange::Single(new_line))
                } else {
                    Some(LineRange::Single(*l))
                }
            }
            LineRange::Range(start, end) => {
                let new_start = if *start >= insertion_point {
                    (*start as i32 + offset) as u32
                } else {
                    *start
                };
                let new_end = if *end >= insertion_point {
                    (*end as i32 + offset) as u32
                } else {
                    *end
                };

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
}

impl Eq for PromptRecord {}

impl PartialOrd for PromptRecord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PromptRecord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Sort oldest to newest based on messages, additions, or deletions
        if self.messages.len() > other.messages.len()
            || self.total_additions > other.total_additions
            || self.total_deletions > other.total_deletions
        {
            std::cmp::Ordering::Greater // self is newer
        } else if other.messages.len() > self.messages.len()
            || other.total_additions > self.total_additions
            || other.total_deletions > self.total_deletions
        {
            std::cmp::Ordering::Less // other is newer
        } else {
            std::cmp::Ordering::Equal
        }
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
        }
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
}
