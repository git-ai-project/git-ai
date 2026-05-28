use crate::authorship::working_log::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub username: String,
    pub email: String,
}

/// Per tool::model contributor stats breakdown
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolModelContributorStats {
    #[serde(default)]
    pub ai_additions: u32,
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub mixed_additions: u32,
    #[serde(default)]
    pub ai_acceptance_rate: f64,
}

/// Per-developer contribution stats, keyed by email in the contributors map
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContributorStats {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub human_additions: u32,
    #[serde(default)]
    pub manual_additions: u32,
    #[serde(default)]
    pub ai_additions: u32,
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub mixed_additions: u32,
    #[serde(default)]
    pub ai_acceptance_rate: f64,
    #[serde(default)]
    pub ai_contribution_rate: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_model_breakdown: BTreeMap<String, ToolModelContributorStats>,
}

impl ContributorStats {
    /// Merge another ContributorStats into this one by summing all numeric fields.
    /// Acceptance rates are NOT summed — they must be recalculated after all merges.
    pub fn merge_from(&mut self, other: &ContributorStats) {
        if self.name.is_empty() {
            self.name = other.name.clone();
        }
        self.human_additions += other.human_additions;
        self.manual_additions += other.manual_additions;
        self.ai_additions += other.ai_additions;
        self.ai_accepted += other.ai_accepted;
        self.mixed_additions += other.mixed_additions;
        // ai_acceptance_rate recalculated after all merges, not summed

        for (key, other_tm) in &other.tool_model_breakdown {
            let tm = self.tool_model_breakdown.entry(key.clone()).or_default();
            tm.ai_additions += other_tm.ai_additions;
            tm.ai_accepted += other_tm.ai_accepted;
            tm.mixed_additions += other_tm.mixed_additions;
        }
    }

    /// Recalculate acceptance rates from current totals.
    /// Must be called after all merge_from operations.
    pub fn recalculate_acceptance_rates(&mut self) {
        if self.ai_additions > 0 {
            self.ai_acceptance_rate =
                ((self.ai_accepted as f64 / self.ai_additions as f64) * 10000.0).round() / 100.0;
        } else {
            self.ai_acceptance_rate = 0.0;
        }

        // ai_contribution_rate = ai_accepted / (ai_accepted + human_additions) * 100
        // Since human_additions = manual_additions + mixed_additions and
        // ai_additions = ai_accepted + mixed_additions, the denominator equals
        // total_additions (manual + ai_accepted + mixed) — no double-counting.
        let contribution_denom = self.ai_accepted + self.human_additions;
        if contribution_denom > 0 {
            self.ai_contribution_rate =
                ((self.ai_accepted as f64 / contribution_denom as f64) * 10000.0).round() / 100.0;
        } else {
            self.ai_contribution_rate = 0.0;
        }

        for tm_stats in self.tool_model_breakdown.values_mut() {
            if tm_stats.ai_additions > 0 {
                tm_stats.ai_acceptance_rate =
                    ((tm_stats.ai_accepted as f64 / tm_stats.ai_additions as f64) * 10000.0)
                        .round()
                        / 100.0;
            } else {
                tm_stats.ai_acceptance_rate = 0.0;
            }
        }
    }
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

    // --- recalculate_acceptance_rates tests ---

    fn make_stats(ai_additions: u32, ai_accepted: u32) -> ContributorStats {
        let mut s = ContributorStats {
            ai_additions,
            ai_accepted,
            ..Default::default()
        };
        s.recalculate_acceptance_rates();
        s
    }

    #[test]
    fn test_rate_perfect_acceptance() {
        // 4/4 = 100.0
        assert_eq!(make_stats(4, 4).ai_acceptance_rate, 100.0);
    }

    #[test]
    fn test_rate_zero_when_no_ai_additions() {
        // 0 additions → rate is 0.0, no division
        assert_eq!(make_stats(0, 0).ai_acceptance_rate, 0.0);
    }

    #[test]
    fn test_rate_partial_acceptance() {
        // 3/4 = 75.0
        assert_eq!(make_stats(4, 3).ai_acceptance_rate, 75.0);
    }

    #[test]
    fn test_rate_half_acceptance() {
        // 1/2 = 50.0
        assert_eq!(make_stats(2, 1).ai_acceptance_rate, 50.0);
    }

    #[test]
    fn test_rate_two_thirds_rounds_to_two_decimals() {
        // 2/3 * 100 = 66.666... → rounded to 66.67
        assert_eq!(make_stats(3, 2).ai_acceptance_rate, 66.67);
    }

    #[test]
    fn test_rate_non_trivial_two_decimals() {
        // 59/90 = 65.555... → 65.56
        assert_eq!(make_stats(90, 59).ai_acceptance_rate, 65.56);
    }

    #[test]
    fn test_rate_tool_model_breakdown_recalculated() {
        let mut s = ContributorStats {
            ai_additions: 10,
            ai_accepted: 10,
            ..Default::default()
        };
        let tm = ToolModelContributorStats {
            ai_additions: 3,
            ai_accepted: 2,
            ..Default::default()
        };
        // pre-condition: rate not yet set
        assert_eq!(tm.ai_acceptance_rate, 0.0);
        s.tool_model_breakdown
            .insert("cursor::gpt-4o".to_string(), tm);
        s.recalculate_acceptance_rates();

        assert_eq!(s.ai_acceptance_rate, 100.0);
        assert_eq!(
            s.tool_model_breakdown["cursor::gpt-4o"].ai_acceptance_rate,
            66.67
        );
    }

    // --- ai_contribution_rate tests ---

    fn make_stats_full(
        ai_accepted: u32,
        mixed_additions: u32,
        manual_additions: u32,
    ) -> ContributorStats {
        let mut s = ContributorStats {
            ai_accepted,
            ai_additions: ai_accepted + mixed_additions,
            mixed_additions,
            manual_additions,
            human_additions: manual_additions + mixed_additions,
            ..Default::default()
        };
        s.recalculate_acceptance_rates();
        s
    }

    #[test]
    fn test_contribution_rate_pure_ai() {
        // ai_accepted=10, human_additions=0 -> 10/10 = 100.0
        let s = make_stats_full(10, 0, 0);
        assert_eq!(s.ai_contribution_rate, 100.0);
    }

    #[test]
    fn test_contribution_rate_pure_manual() {
        // ai_accepted=0, human_additions=10 -> 0/10 = 0.0
        let s = make_stats_full(0, 0, 10);
        assert_eq!(s.ai_contribution_rate, 0.0);
    }

    #[test]
    fn test_contribution_rate_mixed_lowers_ratio() {
        // ai_accepted=50, mixed=20, manual=30 -> human_additions=50
        // 50 / (50 + 50) = 50.0
        let s = make_stats_full(50, 20, 30);
        assert_eq!(s.ai_contribution_rate, 50.0);
        // Sanity: ai_acceptance_rate treats mixed differently: 50/70 = 71.43
        assert_eq!(s.ai_acceptance_rate, 71.43);
    }

    #[test]
    fn test_contribution_rate_rounding() {
        // ai_accepted=1, human_additions=2 -> 1/3 * 100 = 33.333... -> 33.33
        let s = make_stats_full(1, 0, 2);
        assert_eq!(s.ai_contribution_rate, 33.33);
    }

    #[test]
    fn test_contribution_rate_zero_denominator() {
        // Empty commit (all deletions): no additions at all
        let mut s = ContributorStats::default();
        s.recalculate_acceptance_rates();
        assert_eq!(s.ai_contribution_rate, 0.0);
    }

    #[test]
    fn test_rate_tool_model_zero_additions() {
        let mut s = ContributorStats {
            ai_additions: 0,
            ai_accepted: 0,
            ..Default::default()
        };
        s.tool_model_breakdown.insert(
            "cursor::gpt-4o".to_string(),
            ToolModelContributorStats {
                ai_additions: 0,
                ai_accepted: 0,
                ..Default::default()
            },
        );
        s.recalculate_acceptance_rates();

        assert_eq!(s.ai_acceptance_rate, 0.0);
        assert_eq!(
            s.tool_model_breakdown["cursor::gpt-4o"].ai_acceptance_rate,
            0.0
        );
    }
}

