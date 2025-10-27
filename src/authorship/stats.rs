use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::transcript::Message;
use crate::error::GitAiError;
use crate::git::refs::get_authorship;
use crate::git::repository::Repository;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolModelHeadlineStats {
    #[serde(default)]
    pub ai_additions: u32,
    #[serde(default)]
    pub mixed_additions: u32,
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub ai_deletions: u32,
    #[serde(default)]
    pub time_waiting_for_ai: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AuthorshipAnalysis {
    pub human_additions: u32,
    pub mixed_additions: u32,
    pub ai_additions: u32,
    pub ai_accepted: u32,
    pub time_waiting_for_ai: u64,
    pub ai_deletions: u32,
    pub tool_model_breakdown: BTreeMap<String, ToolModelHeadlineStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStats {
    #[serde(default)]
    pub human_additions: u32, // Lines written only by humans
    #[serde(default)]
    pub mixed_additions: u32, // AI-generated lines that were edited by humans
    #[serde(default)]
    pub ai_additions: u32, // AI-generated lines with no human editingso
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub time_waiting_for_ai: u64, // seconds
    #[serde(default)]
    pub git_diff_deleted_lines: u32,
    #[serde(default)]
    pub git_diff_added_lines: u32,
    #[serde(default)]
    pub human_deletions: u32,
    #[serde(default)]
    pub ai_deletions: u32,
    #[serde(default)]
    pub tool_model_breakdown: BTreeMap<String, ToolModelHeadlineStats>,
}

pub fn stats_command(
    repo: &Repository,
    commit_sha: Option<&str>,
    json: bool,
) -> Result<(), GitAiError> {
    let (target, refname) = if let Some(sha) = commit_sha {
        // Validate that the commit exists using revparse_single
        match repo.revparse_single(sha) {
            Ok(commit_obj) => {
                // For a specific commit, we don't have a refname, so use the commit SHA
                let full_sha = commit_obj.id();
                (full_sha, format!("{}", sha))
            }
            Err(GitAiError::GitCliError { .. }) => {
                return Err(GitAiError::Generic(format!("No commit found: {}", sha)));
            }
            Err(e) => return Err(e),
        }
    } else {
        // Default behavior: use current HEAD
        let head = repo.head()?;
        let target = head.target()?;
        let name = head.name().unwrap_or("HEAD").to_string();
        (target, name)
    };

    eprintln!(
        "[DEBUG] Stats command found commit: {} refname: {}",
        target, refname
    );

    let stats = stats_for_commit_stats(repo, &target, &refname)?;

    if json {
        let json_str = serde_json::to_string(&stats)?;
        println!("{}", json_str);
    } else {
        write_stats_to_terminal(&stats, true);
    }

    Ok(())
}

pub fn write_stats_to_terminal(stats: &CommitStats, print: bool) -> String {
    let mut output = String::new();

    // Set maximum bar width to 40 characters
    let bar_width: usize = 40;

    // Handle deletion-only commits (no additions)
    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        // Show gray bar for deletion-only commit
        let mut progress_bar = String::new();
        progress_bar.push_str("you  ");
        progress_bar.push_str("\x1b[90m"); // Gray color
        progress_bar.push_str(&" ".repeat(bar_width)); // Gray bar
        progress_bar.push_str("\x1b[0m"); // Reset color
        progress_bar.push_str(" ai");

        output.push_str(&progress_bar);
        output.push('\n');
        if print {
            println!("{}", progress_bar);
        }

        // Show "(no additions)" message below the bar
        let no_additions_msg = format!("     \x1b[90m{:^40}\x1b[0m", "(no additions)");
        output.push_str(&no_additions_msg);
        output.push('\n');
        if print {
            println!("{}", no_additions_msg);
        }
        // No percentage line or AI stats for deletion-only commits
        return output;
    }

    // Calculate total additions for the progress bar
    // Total = pure human + mixed (AI-edited-by-human) + pure AI
    let total_additions = stats.human_additions + stats.ai_additions;

    // Calculate AI acceptance percentage (capped at 100%)
    // It can go higher because AI can write on top of AI code. This feels reasonable for now
    let _ai_acceptance_percentage = if stats.ai_additions > 0 {
        ((stats.ai_accepted as f64 / stats.ai_additions as f64) * 100.0).min(100.0)
    } else {
        0.0
    };

    // Create progress bar with three categories
    // Pure human = human_additions - mixed_additions (overridden lines)
    let pure_human = stats.human_additions.saturating_sub(stats.mixed_additions);

    let pure_human_bars = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    #[allow(unused_variables)]
    let mixed_bars = if total_additions > 0 {
        ((stats.mixed_additions as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    #[allow(unused_variables)]
    let ai_bars = if total_additions > 0 {
        ((stats.ai_additions as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    // Ensure human contributions get at least 2 visible blocks if they have more than 1 line
    let min_human_bars = if stats.human_additions > 1 { 2 } else { 0 };
    let final_pure_human_bars = if stats.human_additions > 1 {
        pure_human_bars.max(min_human_bars)
    } else {
        pure_human_bars
    };

    // Adjust other bars if we had to give more space to human
    let remaining_width = bar_width.saturating_sub(final_pure_human_bars);
    let total_other_additions = stats.mixed_additions + stats.ai_additions;

    let final_mixed_bars = if total_other_additions > 0 {
        ((stats.mixed_additions as f64 / total_other_additions as f64) * remaining_width as f64)
            as usize
    } else {
        0
    };

    let final_ai_bars = remaining_width.saturating_sub(final_mixed_bars);

    // Build the progress bar with three categories
    let mut progress_bar = String::new();
    progress_bar.push_str("you  ");

    // Pure human bars (darkest)
    progress_bar.push_str(&"█".repeat(final_pure_human_bars));

    // Mixed bars (medium) - AI-generated but human-edited
    progress_bar.push_str(&"▒".repeat(final_mixed_bars));

    // AI bars (lightest) - pure AI, untouched
    progress_bar.push_str(&"░".repeat(final_ai_bars));

    progress_bar.push_str(" ai");

    // Format time waiting for AI
    #[allow(unused_variables)]
    let waiting_time_str = if stats.time_waiting_for_ai > 0 {
        let minutes = stats.time_waiting_for_ai / 60;
        let seconds = stats.time_waiting_for_ai % 60;
        if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
    } else {
        "0s".to_string()
    };

    // Calculate percentages for display
    let pure_human_percentage = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let mixed_percentage = if total_additions > 0 {
        ((stats.mixed_additions as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let ai_percentage = if total_additions > 0 {
        ((stats.ai_additions as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    // Print the stats
    output.push_str(&progress_bar);
    output.push('\n');
    if print {
        println!("{}", progress_bar);
    }
    // Print percentage line with proper spacing (40 columns total)
    // "you  " (5) + 40 chars + " ai" (3) = 48 total
    // Human% left-aligned at left edge of bar, AI% right-aligned at right edge of bar
    if mixed_percentage > 0 {
        // Show all three: human, mixed, ai
        // Human% at left edge, mixed% in middle, AI% at right edge
        let percentage_line = format!(
            "     {:<3}{:>12}mixed {:>3}%{:>12}{:>3}%",
            format!("{}%", pure_human_percentage),
            "",
            mixed_percentage,
            "",
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
        if print {
            println!("{}", percentage_line);
        }
    } else {
        // No mixed, just show human and ai at bar edges
        let percentage_line = format!(
            "     {:<3}{:>33}{:>3}%",
            format!("{}%", pure_human_percentage),
            "",
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
        if print {
            println!("{}", percentage_line);
        }
    }

    // Only show AI stats if there was actually AI code
    if stats.ai_additions > 0 {
        let waiting_time_str = if stats.time_waiting_for_ai > 0 {
            let minutes = stats.time_waiting_for_ai / 60;
            let seconds = stats.time_waiting_for_ai % 60;
            if minutes > 0 {
                format!(" | waited {}m for ai", minutes)
            } else {
                format!(" | waited {}s for ai", seconds)
            }
        } else {
            "".to_string()
        };

        let ai_acceptance_str = format!(
            "     \x1b[90m{:.0}% AI code accepted{}\x1b[0m",
            _ai_acceptance_percentage, waiting_time_str
        );
        output.push_str(&ai_acceptance_str);
        output.push('\n');
        if print {
            println!("{}", ai_acceptance_str);
        }
    }
    return output;
}

/// Format stats into a Markdown string for display
#[allow(dead_code)]
pub fn write_stats_to_markdown(stats: &CommitStats) -> String {
    let mut output = String::new();

    // Set maximum bar width to 40 characters
    let bar_width: usize = 40;

    // Handle deletion-only commits (no additions)
    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        // Show gray bar for deletion-only commit
        let mut progress_bar = String::new();
        progress_bar.push_str("you&nbsp;&nbsp;");
        progress_bar.push_str(&"&nbsp;".repeat(bar_width)); // Gray bar
        progress_bar.push_str("&nbsp;ai");

        output.push_str(&progress_bar);
        output.push('\n');

        // Show "(no additions)" message below the bar
        let no_additions_msg = format!("{}{:^40}", "&nbsp;".repeat(6), "(no additions)");
        output.push_str(&no_additions_msg);
        output.push('\n');
        // No percentage line or AI stats for deletion-only commits
        return output;
    }

    // Calculate total additions for the progress bar
    // Total = pure human + mixed (AI-edited-by-human) + pure AI
    let total_additions = stats.human_additions + stats.ai_additions;

    // Calculate AI acceptance percentage (capped at 100%)
    // It can go higher because AI can write on top of AI code. This feels reasonable for now
    let _ai_acceptance_percentage = if stats.ai_additions > 0 {
        ((stats.ai_accepted as f64 / stats.ai_additions as f64) * 100.0).min(100.0)
    } else {
        0.0
    };

    // Create progress bar with three categories
    // Pure human = human_additions - mixed_additions (overridden lines)
    let pure_human = stats.human_additions.saturating_sub(stats.mixed_additions);

    let pure_human_bars = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    #[allow(unused_variables)]
    let mixed_bars = if total_additions > 0 {
        ((stats.mixed_additions as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    #[allow(unused_variables)]
    let ai_bars = if total_additions > 0 {
        ((stats.ai_additions as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    // Ensure human contributions get at least 2 visible blocks if they have more than 1 line
    let min_human_bars = if stats.human_additions > 1 { 2 } else { 0 };
    let final_pure_human_bars = if stats.human_additions > 1 {
        pure_human_bars.max(min_human_bars)
    } else {
        pure_human_bars
    };

    // Adjust other bars if we had to give more space to human
    let remaining_width = bar_width.saturating_sub(final_pure_human_bars);
    let total_other_additions = stats.mixed_additions + stats.ai_additions;

    let final_mixed_bars = if total_other_additions > 0 {
        ((stats.mixed_additions as f64 / total_other_additions as f64) * remaining_width as f64)
            as usize
    } else {
        0
    };

    let final_ai_bars = remaining_width.saturating_sub(final_mixed_bars);

    // Build the progress bar with three categories
    let mut progress_bar = String::new();
    progress_bar.push_str("you&nbsp;&nbsp;");

    // Pure human bars (darkest)
    progress_bar.push_str(&"█".repeat(final_pure_human_bars));

    // Mixed bars (medium) - AI-generated but human-edited
    progress_bar.push_str(&"▒".repeat(final_mixed_bars));

    // AI bars (lightest) - pure AI, untouched
    progress_bar.push_str(&"░".repeat(final_ai_bars));

    progress_bar.push_str("&nbsp;ai");

    // Format time waiting for AI
    #[allow(unused_variables)]
    let waiting_time_str = if stats.time_waiting_for_ai > 0 {
        let minutes = stats.time_waiting_for_ai / 60;
        let seconds = stats.time_waiting_for_ai % 60;
        if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
    } else {
        "0s".to_string()
    };

    // Calculate percentages for display
    let pure_human_percentage = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let mixed_percentage = if total_additions > 0 {
        ((stats.mixed_additions as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let ai_percentage = if total_additions > 0 {
        ((stats.ai_additions as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    // Print the stats
    output.push_str(&progress_bar);
    output.push('\n');
    // Print percentage line with proper spacing
    // Block characters render much wider in markdown, so we need lots of spacing
    if mixed_percentage > 0 {
        // Show all three: human, mixed, ai
        // Human% at left edge, mixed% in middle, AI% at right edge
        let percentage_line = format!(
            "{}{}{}mixed&nbsp;{}%{}{}%",
            "&nbsp;".repeat(6),
            format!("{}%", pure_human_percentage),
            "&nbsp;".repeat(40),
            mixed_percentage,
            "&nbsp;".repeat(40),
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
    } else {
        // No mixed, just show human and ai at bar edges
        let percentage_line = format!(
            "{}{}{}{}%",
            "&nbsp;".repeat(6),
            format!("{}%", pure_human_percentage),
            "&nbsp;".repeat(105),
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
    }

    return output;
}

pub fn stats_for_commit_stats(
    repo: &Repository,
    commit_sha: &str,
    _refname: &str,
) -> Result<CommitStats, GitAiError> {
    // Step 1: get the diff between this commit and its parent ON refname (if more than one parent)
    // If initial than everything is additions
    // We want the count here git shows +111 -55
    let (git_diff_added_lines, git_diff_deleted_lines) = get_git_diff_stats(repo, commit_sha)?;

    // Step 2: get the authorship log for this commit
    let authorship_log = get_authorship(repo, &commit_sha);

    // Step 3: For prompts with > 1 messages, sum all the time between user messages and AI messages.
    // if the last message is a human message, don't count anything
    let analysis = if let Some(log) = &authorship_log {
        analyze_authorship_log(log)?
    } else {
        AuthorshipAnalysis::default()
    };

    // Calculate human additions as the difference between total git diff and AI additions
    // This handles cases where there are no AI-authored lines (authorship log is empty)
    let human_additions = if git_diff_added_lines >= analysis.ai_additions {
        git_diff_added_lines - analysis.ai_additions
    } else {
        analysis.human_additions
    };

    let human_deletions = git_diff_deleted_lines.saturating_sub(analysis.ai_deletions);

    Ok(CommitStats {
        human_additions,
        mixed_additions: analysis.mixed_additions,
        ai_additions: analysis.ai_additions,
        ai_accepted: analysis.ai_accepted,
        time_waiting_for_ai: analysis.time_waiting_for_ai,
        git_diff_deleted_lines,
        git_diff_added_lines,
        human_deletions,
        ai_deletions: analysis.ai_deletions,
        tool_model_breakdown: analysis.tool_model_breakdown,
    })
}

/// Get git diff statistics between commit and its parent
fn get_git_diff_stats(repo: &Repository, commit_sha: &str) -> Result<(u32, u32), GitAiError> {
    // Use git show --numstat to get diff statistics
    let mut args = repo.global_args_for_exec();
    args.push("show".to_string());
    args.push("--numstat".to_string());
    args.push("--format=".to_string()); // No format, just the numstat
    args.push(commit_sha.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    let mut added_lines = 0u32;
    let mut deleted_lines = 0u32;

    // Parse numstat output
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Skip the commit message lines (they don't start with numbers)
        if !line.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            continue;
        }

        // Parse numstat format: "added\tdeleted\tfilename"
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 {
            // Parse added lines
            if let Ok(added) = parts[0].parse::<u32>() {
                added_lines += added;
            }

            // Parse deleted lines (handle "-" for binary files)
            if parts[1] != "-" {
                if let Ok(deleted) = parts[1].parse::<u32>() {
                    deleted_lines += deleted;
                }
            }
        }
    }

    Ok((added_lines, deleted_lines))
}

/// Analyze authorship log to extract statistics
pub fn analyze_authorship_log(
    authorship_log: &AuthorshipLog,
) -> Result<AuthorshipAnalysis, GitAiError> {
    let mut analysis = AuthorshipAnalysis::default();

    // Count lines by author type
    for file_attestation in &authorship_log.attestations {
        for entry in &file_attestation.entries {
            // Count lines in this entry
            let lines_in_entry: u32 = entry
                .line_ranges
                .iter()
                .map(|range| match range {
                    LineRange::Single(_) => 1,
                    LineRange::Range(start, end) => end - start + 1,
                })
                .sum();

            // Check if this is an AI-generated entry
            if let Some(prompt_record) = authorship_log.metadata.prompts.get(&entry.hash) {
                // This is AI-generated code
                // Check if it was overridden (edited by humans)
                if prompt_record.overriden_lines > 0 {
                    // Mixed: AI-generated but edited by humans
                    // Ensure we don't have more overridden lines than total lines
                    let overriden_lines =
                        std::cmp::min(prompt_record.overriden_lines, lines_in_entry);
                    analysis.mixed_additions += overriden_lines;
                    analysis.ai_additions += lines_in_entry - overriden_lines;
                } else {
                    // Pure AI: no human editing
                    analysis.ai_additions += lines_in_entry;
                }

                // Count accepted lines (this is a simplified approach)
                // In a real implementation, you might want to track acceptance more precisely
                analysis.ai_accepted += lines_in_entry; // For now, assume all AI lines are accepted

                let key = format!(
                    "{}::{}",
                    prompt_record.agent_id.tool, prompt_record.agent_id.model
                );
                let tool_stats = analysis.tool_model_breakdown.entry(key).or_default();

                if prompt_record.overriden_lines > 0 {
                    let overriden_lines =
                        std::cmp::min(prompt_record.overriden_lines, lines_in_entry);
                    tool_stats.mixed_additions += overriden_lines;
                    tool_stats.ai_additions += lines_in_entry - overriden_lines;
                } else {
                    tool_stats.ai_additions += lines_in_entry;
                }
                tool_stats.ai_accepted += lines_in_entry;

                // Calculate time waiting for AI from transcript
                // Create a transcript from the messages
                let transcript = crate::authorship::transcript::AiTranscript {
                    messages: prompt_record.messages.clone(),
                };
                let waiting = calculate_waiting_time(&transcript);
                analysis.time_waiting_for_ai += waiting;
                tool_stats.time_waiting_for_ai += waiting;
            } else {
                // Human-authored lines
                analysis.human_additions += lines_in_entry;
            }
        }
    }

    for prompt_record in authorship_log.metadata.prompts.values() {
        let key = format!(
            "{}::{}",
            prompt_record.agent_id.tool, prompt_record.agent_id.model
        );
        let tool_stats = analysis.tool_model_breakdown.entry(key).or_default();
        tool_stats.ai_deletions += prompt_record.total_deletions;
        analysis.ai_deletions += prompt_record.total_deletions;
    }

    Ok(analysis)
}

/// Calculate time waiting for AI from transcript messages
fn calculate_waiting_time(transcript: &crate::authorship::transcript::AiTranscript) -> u64 {
    let mut total_waiting_time = 0u64;
    let messages = transcript.messages();

    if messages.len() <= 1 {
        return 0;
    }

    // Check if last message is from human (don't count time if so)
    let last_message_is_human = matches!(messages.last(), Some(Message::User { .. }));
    if last_message_is_human {
        return 0;
    }

    // Sum time between user and AI messages
    let mut i = 0;
    while i < messages.len() - 1 {
        if let (
            Message::User {
                timestamp: Some(user_ts),
                ..
            },
            Message::Assistant {
                timestamp: Some(ai_ts),
                ..
            },
        ) = (&messages[i], &messages[i + 1])
        {
            // Parse timestamps and calculate difference
            if let (Ok(user_time), Ok(ai_time)) = (
                chrono::DateTime::parse_from_rfc3339(user_ts),
                chrono::DateTime::parse_from_rfc3339(ai_ts),
            ) {
                let duration = ai_time.signed_duration_since(user_time);
                if duration.num_seconds() > 0 {
                    total_waiting_time += duration.num_seconds() as u64;
                }
            }

            i += 2; // Skip to next user message
        } else {
            i += 1;
        }
    }

    total_waiting_time
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;

    use super::*;
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_terminal_stats_display() {
        // Test with mixed human/AI stats
        let stats = CommitStats {
            human_additions: 50,
            mixed_additions: 40,
            ai_additions: 100,
            ai_accepted: 25,
            time_waiting_for_ai: 72009, // 1 minute 30 seconds
            git_diff_deleted_lines: 15,
            git_diff_added_lines: 80,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let mixed_output = write_stats_to_terminal(&stats, true);
        assert_debug_snapshot!(mixed_output);

        // Test with AI-only stats
        let ai_stats = CommitStats {
            human_additions: 0,
            mixed_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            time_waiting_for_ai: 45,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let ai_only_output = write_stats_to_terminal(&ai_stats, true);
        assert_debug_snapshot!(ai_only_output);

        // Test with human-only stats
        let human_stats = CommitStats {
            human_additions: 75,
            mixed_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 10,
            git_diff_added_lines: 75,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let human_only_output = write_stats_to_terminal(&human_stats, true);
        assert_debug_snapshot!(human_only_output);

        // Test with minimal human contribution (should get at least 2 blocks)
        let minimal_human_stats = CommitStats {
            human_additions: 2,
            mixed_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            time_waiting_for_ai: 30,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 102,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let minimal_human_output = write_stats_to_terminal(&minimal_human_stats, true);
        assert_debug_snapshot!(minimal_human_output);

        // Test with deletion-only commit (no additions)
        let deletion_only_stats = CommitStats {
            human_additions: 0,
            mixed_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 25,
            git_diff_added_lines: 0,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let deletion_only_output = write_stats_to_terminal(&deletion_only_stats, true);
        assert_debug_snapshot!(deletion_only_output);
    }

    #[test]
    fn test_markdown_stats_display() {
        // Test with mixed human/AI stats
        let stats = CommitStats {
            human_additions: 50,
            mixed_additions: 40,
            ai_additions: 100,
            ai_accepted: 25,
            time_waiting_for_ai: 72009, // 1 minute 30 seconds
            git_diff_deleted_lines: 15,
            git_diff_added_lines: 80,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let mixed_output = write_stats_to_markdown(&stats);
        assert_debug_snapshot!(mixed_output);

        // Test with AI-only stats
        let ai_stats = CommitStats {
            human_additions: 0,
            mixed_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            time_waiting_for_ai: 45,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let ai_only_output = write_stats_to_markdown(&ai_stats);
        assert_debug_snapshot!(ai_only_output);

        // Test with human-only stats
        let human_stats = CommitStats {
            human_additions: 75,
            mixed_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 10,
            git_diff_added_lines: 75,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let human_only_output = write_stats_to_markdown(&human_stats);
        assert_debug_snapshot!(human_only_output);

        // Test with minimal human contribution (should get at least 2 blocks)
        let minimal_human_stats = CommitStats {
            human_additions: 2,
            mixed_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            time_waiting_for_ai: 30,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 102,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let minimal_human_output = write_stats_to_markdown(&minimal_human_stats);
        assert_debug_snapshot!(minimal_human_output);

        // Test with deletion-only commit (no additions)
        let deletion_only_stats = CommitStats {
            human_additions: 0,
            mixed_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 25,
            git_diff_added_lines: 0,
            human_deletions: 0,
            ai_deletions: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let deletion_only_output = write_stats_to_markdown(&deletion_only_stats);
        assert_debug_snapshot!(deletion_only_output);
    }

    #[test]
    fn test_stats_for_simple_ai_commit() {
        let tmp_repo = TmpRepo::new().unwrap();

        let mut file = tmp_repo.write_file("test.txt", "Line1\n", true).unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        tmp_repo.commit_with_message("Initial commit").unwrap();

        // AI adds 2 lines
        file.append("Line 2\nLine 3\n").unwrap();

        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();

        tmp_repo.commit_with_message("AI adds lines").unwrap();

        // Get the commit SHA for the AI commit
        let head_sha = tmp_repo.get_head_commit_sha().unwrap();

        // Test our stats function
        let stats = stats_for_commit_stats(&tmp_repo.gitai_repo(), &head_sha, "HEAD").unwrap();

        // Verify the stats
        assert_eq!(
            stats.human_additions, 0,
            "No human additions in AI-only commit"
        );
        assert_eq!(stats.ai_additions, 2, "AI added 2 lines");
        assert_eq!(stats.ai_accepted, 2, "AI lines were accepted");
        assert_eq!(
            stats.git_diff_added_lines, 2,
            "Git diff shows 2 added lines"
        );
        assert_eq!(
            stats.git_diff_deleted_lines, 0,
            "Git diff shows 0 deleted lines"
        );
        assert_eq!(
            stats.time_waiting_for_ai, 0,
            "No waiting time recorded (no timestamps in test)"
        );
    }

    #[test]
    fn test_stats_for_mixed_commit() {
        let tmp_repo = TmpRepo::new().unwrap();

        let mut file = tmp_repo
            .write_file("test.txt", "Base line\n", true)
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        tmp_repo.commit_with_message("Initial commit").unwrap();

        // AI adds lines
        file.append("AI line 1\nAI line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("claude-3-sonnet"), Some("cursor"))
            .unwrap();

        // Human adds lines
        file.append("Human line 1\nHuman line 2\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        tmp_repo.commit_with_message("Mixed commit").unwrap();

        let head_sha = tmp_repo.get_head_commit_sha().unwrap();
        let stats = stats_for_commit_stats(&tmp_repo.gitai_repo(), &head_sha, "HEAD").unwrap();

        // Verify the stats
        assert_eq!(stats.human_additions, 2, "Human added 2 lines");
        assert_eq!(stats.ai_additions, 2, "AI added 2 lines");
        assert_eq!(stats.ai_accepted, 2, "AI lines were accepted");
        assert_eq!(
            stats.git_diff_added_lines, 4,
            "Git diff shows 4 added lines total"
        );
        assert_eq!(
            stats.git_diff_deleted_lines, 0,
            "Git diff shows 0 deleted lines"
        );
    }

    #[test]
    fn test_stats_for_initial_commit() {
        let tmp_repo = TmpRepo::new().unwrap();

        let _file = tmp_repo
            .write_file("test.txt", "Line1\nLine2\nLine3\n", true)
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        tmp_repo.commit_with_message("Initial commit").unwrap();

        let head_sha = tmp_repo.get_head_commit_sha().unwrap();
        let stats = stats_for_commit_stats(&tmp_repo.gitai_repo(), &head_sha, "HEAD").unwrap();

        // For initial commit, everything should be additions
        assert_eq!(
            stats.human_additions, 3,
            "Human authored 3 lines in initial commit"
        );
        assert_eq!(stats.ai_additions, 0, "No AI additions in initial commit");
        assert_eq!(stats.ai_accepted, 0, "No AI lines to accept");
        assert_eq!(
            stats.git_diff_added_lines, 3,
            "Git diff shows 3 added lines (initial commit)"
        );
        assert_eq!(
            stats.git_diff_deleted_lines, 0,
            "Git diff shows 0 deleted lines"
        );
    }
}
