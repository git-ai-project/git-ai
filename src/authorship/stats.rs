use crate::authorship::authorship_log::LineRange;
use crate::authorship::ignore::{build_ignore_matcher, should_ignore_file_with_matcher};
use crate::error::GitAiError;
use crate::git::notes_api::read_authorship;
use crate::git::repository::{Repository, exec_git};
use crate::mdm::spinner::Spinner;
use crate::utils::is_interactive_terminal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RECENT_COMMIT_WINDOW: Duration = Duration::from_secs(60);
const AUTHORSHIP_NOTE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const AUTHORSHIP_NOTE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WAIT_MESSAGE: &str = "Waiting for git-ai to process this commit";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolModelHeadlineStats {
    #[serde(default)]
    pub ai_additions: u32, // Number of lines committed with AI attribution
    #[serde(default)]
    pub ai_accepted: u32, // Number of AI-generated lines that were accepted by the user without any human edits
    #[serde(default)]
    pub ai_additions_chars: u64, // Character count of committed AI-attributed lines
    #[serde(default)]
    pub ai_additions_bytes: u64, // Byte count of committed AI-attributed lines
    #[serde(default)]
    pub ai_accepted_chars: u64, // Character count of AI lines accepted without human edits
    #[serde(default)]
    pub ai_accepted_bytes: u64, // Byte count of AI lines accepted without human edits
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommitStats {
    #[serde(default)]
    pub human_additions: u32, // Number of lines committed with human attribution
    #[serde(default)]
    pub unknown_additions: u32, // Number of lines with no attestation at all
    #[serde(default)]
    pub ai_additions: u32, // Number of lines committed with AI attribution
    #[serde(default)]
    pub ai_accepted: u32, // Number of AI-generated lines that were accepted by the user without any human edits
    #[serde(default)]
    pub git_diff_deleted_lines: u32,
    #[serde(default)]
    pub git_diff_added_lines: u32,
    #[serde(default)]
    pub human_additions_chars: u64, // Character count of lines committed with human attribution
    #[serde(default)]
    pub human_additions_bytes: u64, // Byte count of lines committed with human attribution
    #[serde(default)]
    pub unknown_additions_chars: u64, // Character count of lines with no attestation at all
    #[serde(default)]
    pub unknown_additions_bytes: u64, // Byte count of lines with no attestation at all
    #[serde(default)]
    pub ai_additions_chars: u64, // Character count of lines committed with AI attribution
    #[serde(default)]
    pub ai_additions_bytes: u64, // Byte count of lines committed with AI attribution
    #[serde(default)]
    pub ai_accepted_chars: u64, // Character count of AI lines accepted without human edits
    #[serde(default)]
    pub ai_accepted_bytes: u64, // Byte count of AI lines accepted without human edits
    #[serde(default)]
    pub git_diff_added_chars: u64, // Character count of all added lines in the git diff
    #[serde(default)]
    pub git_diff_added_bytes: u64, // Byte count of all added lines in the git diff
    #[serde(default)]
    pub git_diff_deleted_chars: u64, // Character count of all deleted lines in the git diff
    #[serde(default)]
    pub git_diff_deleted_bytes: u64, // Byte count of all deleted lines in the git diff
    #[serde(default)]
    pub tool_model_breakdown: BTreeMap<String, ToolModelHeadlineStats>,
}

pub fn stats_command(
    repo: &Repository,
    commit_sha: Option<&str>,
    json: bool,
    ignore_patterns: &[String],
) -> Result<(), GitAiError> {
    let (target, refname) = if let Some(sha) = commit_sha {
        // Validate that the commit exists using revparse_single
        match repo.revparse_single(sha) {
            Ok(commit_obj) => {
                // For a specific commit, we don't have a refname, so use the commit SHA
                let full_sha = commit_obj.id();
                (full_sha, sha.to_string())
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

    tracing::debug!(
        "Stats command found commit: {} refname: {}",
        target,
        refname
    );

    let authorship_log = wait_for_recent_authorship(repo, &target)?;
    let stats = stats_for_commit_stats_with_authorship(
        repo,
        &target,
        ignore_patterns,
        authorship_log.as_ref(),
    )?;

    if json {
        let json_str = serde_json::to_string(&stats)?;
        println!("{}", json_str);
    } else {
        write_stats_to_terminal(&stats, true);
    }

    Ok(())
}

fn wait_for_recent_authorship(
    repo: &Repository,
    commit_sha: &str,
) -> Result<Option<crate::authorship::authorship_log_serialization::AuthorshipLog>, GitAiError> {
    if let Some(authorship_log) = read_authorship(repo, commit_sha) {
        return Ok(Some(authorship_log));
    }

    let commit_timestamp = commit_timestamp(repo, commit_sha)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            GitAiError::Generic(format!("System clock is before Unix epoch: {error}"))
        })?
        .as_secs();
    if now.abs_diff(commit_timestamp) > RECENT_COMMIT_WINDOW.as_secs() {
        return Ok(None);
    }

    let force_tty = std::env::var_os("GIT_AI_TEST_FORCE_TTY").is_some();
    let spinner = if force_tty {
        // Indicatif intentionally hides itself when stderr is captured. Keep the
        // existing test-only TTY override useful for subprocess assertions.
        eprintln!("{WAIT_MESSAGE}");
        None
    } else {
        is_interactive_terminal().then(|| Spinner::new(WAIT_MESSAGE))
    };
    let started = Instant::now();
    let authorship_log = loop {
        let remaining = AUTHORSHIP_NOTE_WAIT_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break None;
        }
        std::thread::sleep(AUTHORSHIP_NOTE_POLL_INTERVAL.min(remaining));
        if let Some(authorship_log) = read_authorship(repo, commit_sha) {
            break Some(authorship_log);
        }
    };
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    Ok(authorship_log)
}

fn commit_timestamp(repo: &Repository, commit_sha: &str) -> Result<u64, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "show".to_string(),
        "-s".to_string(),
        "--no-notes".to_string(),
        "--format=%ct".to_string(),
        commit_sha.to_string(),
    ]);
    let output = exec_git(&args)?;
    String::from_utf8(output.stdout)?
        .trim()
        .parse()
        .map_err(|error| {
            GitAiError::Generic(format!(
                "Invalid commit timestamp for {commit_sha}: {error}"
            ))
        })
}

pub fn write_stats_to_terminal(stats: &CommitStats, is_interactive: bool) -> String {
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
        if is_interactive {
            println!("{}", progress_bar);
        }

        // Show "(no additions)" message below the bar
        let no_additions_msg = format!("     \x1b[90m{:^40}\x1b[0m", "(no additions)");
        output.push_str(&no_additions_msg);
        output.push('\n');
        if is_interactive {
            println!("{}", no_additions_msg);
        }
        // No percentage line or AI stats for deletion-only commits
        return output;
    }

    // Calculate total additions: known human + unknown (untracked) + AI
    let total_additions = stats.human_additions + stats.unknown_additions + stats.ai_additions;

    // (ai_additions == ai_accepted after mixed removal, so acceptance is always 100%)

    // Determine whether to show the untracked segment (raw float check, before rounding)
    let untracked_pct_raw = if total_additions > 0 {
        stats.unknown_additions as f64 / total_additions as f64 * 100.0
    } else {
        0.0
    };
    let show_untracked = untracked_pct_raw > 1.0;

    // Calculate human bar segment
    let human_bars = if total_additions > 0 {
        ((stats.human_additions as f64 / total_additions as f64) * bar_width as f64) as usize
    } else {
        0
    };

    // Ensure human contributions get at least 2 visible blocks if they have more than 1 line
    let min_human_bars = if stats.human_additions > 1 { 2 } else { 0 };
    let final_human_bars = human_bars.max(min_human_bars);

    // Distribute remaining width between untracked and AI proportionally.
    // When untracked is below the 1% threshold, all remaining width goes to AI.
    let remaining_width = bar_width.saturating_sub(final_human_bars);
    let (final_untracked_bars, final_ai_bars) = if show_untracked {
        let total_other = stats.unknown_additions + stats.ai_additions;
        let untracked_bars = if total_other > 0 {
            ((stats.unknown_additions as f64 / total_other as f64) * remaining_width as f64)
                as usize
        } else {
            0
        };
        (
            untracked_bars,
            remaining_width.saturating_sub(untracked_bars),
        )
    } else {
        (0, remaining_width)
    };

    // Build the progress bar
    let mut progress_bar = String::new();
    progress_bar.push_str("you  ");
    progress_bar.push_str(&"█".repeat(final_human_bars)); // known human (attested)
    progress_bar.push_str(&"·".repeat(final_untracked_bars)); // untracked (no attestation)
    progress_bar.push_str(&"░".repeat(final_ai_bars)); // AI
    progress_bar.push_str(" ai");

    // Calculate percentages for display
    let human_percentage = if total_additions > 0 {
        ((stats.human_additions as f64 / total_additions as f64) * 100.0).round() as u32
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
    if is_interactive {
        println!("{}", progress_bar);
    }

    // Percentage line: three anchors (human / untracked / AI) when untracked is visible,
    // two anchors (human / AI) otherwise.
    if show_untracked {
        let untracked_percentage = untracked_pct_raw.round() as u32;
        // When interactive, wrap "untracked" in an OSC 8 hyperlink so it is clickable in
        // supporting terminals (iTerm2, Warp, etc.). Spaces are constructed manually —
        // not via format-width padding on the label — so that invisible escape bytes do
        // not misalign the output.
        let untracked_label = if is_interactive {
            "\x1b]8;;https://usegitai.com/docs/cli/untracked\x1b\\\x1b[4muntracked\x1b[24m\x1b]8;;\x1b\\"
                .to_string()
        } else {
            "untracked".to_string()
        };
        let percentage_line = format!(
            "     {:<3}{:>10}{} {:>3}%{:>10}{:>3}%",
            format!("{}%", human_percentage),
            "",
            untracked_label,
            untracked_percentage,
            "",
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
        if is_interactive {
            println!("{}", percentage_line);
        }
    } else {
        let percentage_line = format!(
            "     {:<3}{:>33}{:>3}%",
            format!("{}%", human_percentage),
            "",
            ai_percentage
        );
        output.push_str(&percentage_line);
        output.push('\n');
        if is_interactive {
            println!("{}", percentage_line);
        }
    }

    output
}

#[allow(dead_code)]
pub fn write_stats_to_markdown(stats: &CommitStats) -> String {
    let mut output = String::new();

    // Set maximum bar width to 20 characters
    let bar_width: usize = 20;

    // Handle deletion-only commits (no additions)
    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        output.push_str("(no additions)");
        output.push('\n');
        return output;
    }

    // Calculate total additions for the progress bar
    let total_additions = stats.git_diff_added_lines;

    // Human additions: known-human attested + unattested
    let pure_human = stats.human_additions + stats.unknown_additions;
    // AI = AI lines accepted
    let pure_ai = stats.ai_accepted;

    // Calculate percentages for display
    let pure_human_percentage = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let ai_percentage = if total_additions > 0 {
        ((pure_ai as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    // Calculate bar sizes
    let pure_human_bars = if total_additions > 0 {
        let calculated =
            ((pure_human as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        // Ensure at least 1 block if value > 0
        if pure_human > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    let ai_bars = if total_additions > 0 {
        let calculated =
            ((pure_ai as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        // Ensure at least 1 block if value > 0
        if pure_ai > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    output.push_str("Stats powered by [Git AI](https://github.com/git-ai-project/git-ai)\n\n");
    // Build the fenced code block
    output.push_str("```text\n");

    // Human line: dark blocks for human, light blocks for rest
    output.push_str("🧠 you    ");
    output.push_str(&"█".repeat(pure_human_bars));
    output.push_str(&"░".repeat(bar_width.saturating_sub(pure_human_bars)));
    output.push_str(&format!("  {}%\n", pure_human_percentage));

    // AI line: light blocks for non-ai, dark blocks for ai
    output.push_str("🤖 ai     ");
    output.push_str(&"░".repeat(bar_width.saturating_sub(ai_bars)));
    output.push_str(&"█".repeat(ai_bars));
    output.push_str(&format!("  {}%\n", ai_percentage));

    output.push_str("```");

    // Add details section
    output.push_str("\n\n<details>\n");
    output.push_str("<summary>More stats</summary>\n\n");

    // Find top model by accepted lines
    if !stats.tool_model_breakdown.is_empty()
        && let Some((model_name, model_stats)) = stats
            .tool_model_breakdown
            .iter()
            .max_by_key(|(_, stats)| stats.ai_accepted)
    {
        output.push_str(&format!(
            "- Top model: {} ({} accepted lines)\n",
            model_name, model_stats.ai_accepted
        ));
    }

    output.push_str("\n</details>");

    output
}

/// Calculate commit stats from an authorship log
/// This helper can work with both fetched and in-memory authorship logs
pub fn stats_from_authorship_log(
    _authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    git_diff_added_lines: u32,
    git_diff_deleted_lines: u32,
    ai_accepted: u32,
    known_human_accepted: u32,
    ai_accepted_by_tool: &BTreeMap<String, u32>,
) -> CommitStats {
    let mut commit_stats = CommitStats {
        human_additions: 0,
        unknown_additions: 0,
        ai_additions: 0,
        ai_accepted,
        tool_model_breakdown: BTreeMap::new(),
        git_diff_deleted_lines,
        git_diff_added_lines,
        ..Default::default()
    };

    // Update tool-level accepted counts using diff-based attribution.
    for (tool_model, accepted) in ai_accepted_by_tool {
        let tool_stats = commit_stats
            .tool_model_breakdown
            .entry(tool_model.clone())
            .or_default();
        tool_stats.ai_accepted = *accepted;
    }

    // AI additions = ai_accepted (no mixed component)
    commit_stats.ai_additions = commit_stats.ai_accepted;

    // Set ai_additions for each tool: ai_additions = ai_accepted
    for tool_stats in commit_stats.tool_model_breakdown.values_mut() {
        tool_stats.ai_additions = tool_stats.ai_accepted;
    }

    // KnownHuman-attested additions (positively identified as human-authored)
    commit_stats.human_additions = known_human_accepted;

    // Unknown additions: lines with no attestation at all (not AI-accepted, not KnownHuman)
    commit_stats.unknown_additions = git_diff_added_lines
        .saturating_sub(commit_stats.ai_accepted)
        .saturating_sub(known_human_accepted);

    commit_stats
}

pub fn stats_for_commit_stats(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<CommitStats, GitAiError> {
    let authorship_log = read_authorship(repo, commit_sha);
    stats_for_commit_stats_with_authorship(
        repo,
        commit_sha,
        ignore_patterns,
        authorship_log.as_ref(),
    )
}

pub fn stats_for_commit_stats_with_authorship(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
) -> Result<CommitStats, GitAiError> {
    let commit_obj = repo.revparse_single(commit_sha)?.peel_to_commit()?;
    let parent_count = commit_obj.parent_count()?;

    if parent_count > 1 {
        return stats_for_commit_stats_from_hunks(
            repo,
            commit_sha,
            ignore_patterns,
            &[],
            authorship_log,
        );
    }

    let parent_sha = if parent_count == 0 {
        None
    } else {
        Some(commit_obj.parent(0)?.id())
    };

    stats_for_commit_stats_with_parent_and_authorship(
        repo,
        commit_sha,
        parent_sha.as_deref(),
        ignore_patterns,
        authorship_log,
    )
}

pub fn stats_for_commit_stats_with_parent_and_authorship(
    repo: &Repository,
    commit_sha: &str,
    parent_sha: Option<&str>,
    ignore_patterns: &[String],
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
) -> Result<CommitStats, GitAiError> {
    use crate::commands::diff::get_diff_with_line_numbers;

    let from_ref = parent_sha.unwrap_or("4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    let hunks = get_diff_with_line_numbers(repo, from_ref, commit_sha)?;
    stats_for_commit_stats_from_hunks(repo, commit_sha, ignore_patterns, &hunks, authorship_log)
}

#[doc(hidden)]
pub fn accepted_lines_from_attestations(
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    added_lines_by_file: &HashMap<String, Vec<u32>>,
    is_merge_commit: bool,
) -> (u32, u32, BTreeMap<String, u32>) {
    // returns (ai_accepted, known_human_accepted, per_tool_model)
    if is_merge_commit {
        return (0, 0, BTreeMap::new());
    }

    let mut total_ai_accepted = 0u32;
    let mut known_human_accepted = 0u32;
    let mut per_tool_model = BTreeMap::new();

    let Some(log) = authorship_log else {
        return (0, 0, per_tool_model);
    };

    for file_attestation in &log.attestations {
        let Some(added_lines) = added_lines_by_file.get(&file_attestation.file_path) else {
            continue;
        };

        for entry in &file_attestation.entries {
            // KnownHuman entries (h_ prefix): count as known-human-attested lines.
            if entry.hash.starts_with("h_") {
                let accepted = entry
                    .line_ranges
                    .iter()
                    .map(|line_range| line_range_overlap_len(line_range, added_lines))
                    .sum::<u32>();
                if accepted > 0 {
                    known_human_accepted += accepted;
                }
                continue;
            }

            let accepted = entry
                .line_ranges
                .iter()
                .map(|line_range| line_range_overlap_len(line_range, added_lines))
                .sum::<u32>();

            if accepted == 0 {
                continue;
            }

            total_ai_accepted += accepted;

            if let Some(tool_model) = entry_tool_model(log, &entry.hash) {
                *per_tool_model.entry(tool_model).or_insert(0) += accepted;
            }
        }
    }

    (total_ai_accepted, known_human_accepted, per_tool_model)
}

/// Resolve a non-`h_` attestation hash to its `tool::model` key: session
/// (`s_`) hashes through the sessions map, other hashes through the prompts map.
fn entry_tool_model(
    log: &crate::authorship::authorship_log_serialization::AuthorshipLog,
    hash: &str,
) -> Option<String> {
    let agent_id = if hash.starts_with("s_") {
        let session_key = hash.split("::").next().unwrap_or(hash);
        &log.metadata.sessions.get(session_key)?.agent_id
    } else {
        &log.metadata.prompts.get(hash)?.agent_id
    };
    Some(format!("{}::{}", agent_id.tool, agent_id.model))
}

#[doc(hidden)]
pub fn count_line_payload_volume(payloads: &[String]) -> (u64, u64) {
    // Returns (chars, bytes) for a set of counted line payloads.
    //
    // Each payload contributes `payload.chars().count() + 1` chars and
    // `payload.len() + 1` bytes, where the +1 accounts for the synthetic LF
    // (U+000A) that git-diff line semantics attach to every counted line.
    // CRLF is never reconstructed; a literal '\r' inside a payload is counted
    // verbatim. `DiffHunk` carries no "no newline at end of file" marker, so
    // the synthetic LF is always added.
    let mut chars = 0u64;
    let mut bytes = 0u64;
    for payload in payloads {
        chars = chars.saturating_add(payload.chars().count() as u64 + 1);
        bytes = bytes.saturating_add(payload.len() as u64 + 1);
    }
    (chars, bytes)
}

#[doc(hidden)]
pub fn line_range_overlap_len(range: &LineRange, added_lines: &[u32]) -> u32 {
    line_range_overlap_indices(range, added_lines).len() as u32
}

/// Index range of the sorted `added_lines` that overlaps `range` — the shared
/// overlap semantics behind `line_range_overlap_len` and the volume helpers.
fn line_range_overlap_indices(range: &LineRange, added_lines: &[u32]) -> std::ops::Range<usize> {
    match range {
        LineRange::Single(line) => match added_lines.binary_search(line) {
            Ok(index) => index..index + 1,
            Err(_) => 0..0,
        },
        LineRange::Range(start, end) => {
            let start_idx = added_lines.partition_point(|line| *line < *start);
            let end_idx = added_lines.partition_point(|line| *line <= *end);
            start_idx..end_idx.max(start_idx)
        }
    }
}

/// Payload+LF volume of the (line, payload) pairs, using the same rule as
/// `count_line_payload_volume`.
fn pair_payload_volume(pairs: &[(u32, &str)]) -> (u64, u64) {
    pairs
        .iter()
        .fold((0u64, 0u64), |(chars, bytes), (_, payload)| {
            (
                chars.saturating_add(payload.chars().count() as u64 + 1),
                bytes.saturating_add(payload.len() as u64 + 1),
            )
        })
}

/// Call `visit(entry hash, chars, bytes)` once per attestation entry with the
/// payload volume of the added lines that entry overlaps. Each entry is
/// measured on its own, like the per-entry line sums in
/// `accepted_lines_from_attestations`.
fn for_each_entry_overlap_volume(
    log: &crate::authorship::authorship_log_serialization::AuthorshipLog,
    added_payloads_by_file: &HashMap<String, Vec<(u32, &str)>>,
    mut visit: impl FnMut(&str, u64, u64),
) {
    for file_attestation in &log.attestations {
        let Some(pairs) = added_payloads_by_file.get(&file_attestation.file_path) else {
            continue;
        };
        let added_lines: Vec<u32> = pairs.iter().map(|(line, _)| *line).collect();
        for entry in &file_attestation.entries {
            let (chars, bytes) = entry.line_ranges.iter().fold((0u64, 0u64), |acc, range| {
                let overlap = &pairs[line_range_overlap_indices(range, &added_lines)];
                let (chars, bytes) = pair_payload_volume(overlap);
                (acc.0.saturating_add(chars), acc.1.saturating_add(bytes))
            });
            if chars > 0 || bytes > 0 {
                visit(&entry.hash, chars, bytes);
            }
        }
    }
}

/// Char/byte volume for the three added-line buckets, computed with the same
/// formula as the line buckets in `stats_from_authorship_log`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct AddedBucketVolume {
    pub human_chars: u64,
    pub human_bytes: u64,
    pub unknown_chars: u64,
    pub unknown_bytes: u64,
    pub ai_chars: u64,
    pub ai_bytes: u64,
}

impl AddedBucketVolume {
    /// Build the buckets from total, known-human, and AI (chars, bytes).
    /// Unknown is total - ai - human (saturating), the same formula as
    /// `unknown_additions`.
    pub(crate) fn from_totals(total: (u64, u64), human: (u64, u64), ai: (u64, u64)) -> Self {
        Self {
            human_chars: human.0,
            human_bytes: human.1,
            unknown_chars: total.0.saturating_sub(ai.0).saturating_sub(human.0),
            unknown_bytes: total.1.saturating_sub(ai.1).saturating_sub(human.1),
            ai_chars: ai.0,
            ai_bytes: ai.1,
        }
    }
}

/// Total payload+LF volume of every added line in `added_payloads_by_file`.
pub(crate) fn added_payload_volume(
    added_payloads_by_file: &HashMap<String, Vec<(u32, &str)>>,
) -> (u64, u64) {
    added_payloads_by_file
        .values()
        .map(|pairs| pair_payload_volume(pairs))
        .fold((0u64, 0u64), |acc, (chars, bytes)| {
            (acc.0.saturating_add(chars), acc.1.saturating_add(bytes))
        })
}

/// Sum payload+LF volume into the known-human / unknown / AI buckets.
/// `added_payloads_by_file` maps each file to its (line number, payload) pairs
/// sorted by line number. Known-human (`h_`) and AI (any other hash) volume is
/// summed per attestation entry, and unknown is total - ai - human, so every
/// bucket follows the same formula as its line field. Merge commits skip
/// attestation (matching the line-stat merge skip), so every bucket stays 0.
#[doc(hidden)]
pub fn added_bucket_volume_from_attestations(
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    added_payloads_by_file: &HashMap<String, Vec<(u32, &str)>>,
    is_merge_commit: bool,
) -> AddedBucketVolume {
    if is_merge_commit {
        return AddedBucketVolume::default();
    }

    let mut human = (0u64, 0u64);
    let mut ai = (0u64, 0u64);
    if let Some(log) = authorship_log {
        for_each_entry_overlap_volume(log, added_payloads_by_file, |hash, chars, bytes| {
            let bucket = if hash.starts_with("h_") {
                &mut human
            } else {
                &mut ai
            };
            bucket.0 = bucket.0.saturating_add(chars);
            bucket.1 = bucket.1.saturating_add(bytes);
        });
    }

    AddedBucketVolume::from_totals(added_payload_volume(added_payloads_by_file), human, ai)
}

/// Char/byte volume per tool/model, summed per attestation entry with the same
/// tool key lookup as the per-tool line counts in
/// `accepted_lines_from_attestations`. Returns tool key -> (chars, bytes).
/// Merge commits and missing authorship logs return an empty map, matching
/// the line headline behavior.
#[doc(hidden)]
pub fn tool_model_volume_from_attestations(
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    added_payloads_by_file: &HashMap<String, Vec<(u32, &str)>>,
    is_merge_commit: bool,
) -> BTreeMap<String, (u64, u64)> {
    let mut volume: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    if is_merge_commit {
        return volume;
    }
    let Some(log) = authorship_log else {
        return volume;
    };

    for_each_entry_overlap_volume(log, added_payloads_by_file, |hash, chars, bytes| {
        if hash.starts_with("h_") {
            return;
        }
        if let Some(tool_model) = entry_tool_model(log, hash) {
            let tool_volume = volume.entry(tool_model).or_default();
            tool_volume.0 = tool_volume.0.saturating_add(chars);
            tool_volume.1 = tool_volume.1.saturating_add(bytes);
        }
    });

    volume
}

/// AI (chars, bytes) and per-tool volume for the added lines blamed to an AI
/// tool. `ai_tool_model_by_line` maps file -> line -> tool key (as returned by
/// `diff_ai_accepted_stats`), so this volume follows the same blame-based line
/// set as range `ai_accepted` and its per-tool counts.
#[doc(hidden)]
pub fn ai_volume_from_line_tool_models(
    ai_tool_model_by_line: &HashMap<String, HashMap<u32, String>>,
    added_payloads_by_file: &HashMap<String, Vec<(u32, &str)>>,
) -> ((u64, u64), BTreeMap<String, (u64, u64)>) {
    let mut ai = (0u64, 0u64);
    let mut per_tool: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (file_path, pairs) in added_payloads_by_file {
        let Some(tool_by_line) = ai_tool_model_by_line.get(file_path) else {
            continue;
        };
        for pair in pairs {
            let Some(tool_model) = tool_by_line.get(&pair.0) else {
                continue;
            };
            let (chars, bytes) = pair_payload_volume(std::slice::from_ref(pair));
            ai.0 = ai.0.saturating_add(chars);
            ai.1 = ai.1.saturating_add(bytes);
            let tool_volume = per_tool.entry(tool_model.clone()).or_default();
            tool_volume.0 = tool_volume.0.saturating_add(chars);
            tool_volume.1 = tool_volume.1.saturating_add(bytes);
        }
    }
    (ai, per_tool)
}

/// Like `stats_for_commit_stats` but accepts pre-computed diff hunks and authorship log,
/// avoiding redundant git subprocess calls in the post-commit hook path.
pub fn stats_for_commit_stats_from_hunks(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
    hunks: &[crate::commands::diff::DiffHunk],
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
) -> Result<CommitStats, GitAiError> {
    let commit_obj = repo.revparse_single(commit_sha)?.peel_to_commit()?;
    let parent_count = commit_obj.parent_count()?;
    let is_merge_commit = parent_count > 1;

    Ok(stats_for_commit_stats_from_hunks_with_merge_flag(
        ignore_patterns,
        hunks,
        authorship_log,
        is_merge_commit,
    ))
}

pub(crate) fn stats_for_commit_stats_from_hunks_with_merge_flag(
    ignore_patterns: &[String],
    hunks: &[crate::commands::diff::DiffHunk],
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    is_merge_commit: bool,
) -> CommitStats {
    let fold = fold_diff_hunks(hunks, ignore_patterns, is_merge_commit);

    let (ai_accepted, known_human_accepted, ai_accepted_by_tool) = accepted_lines_from_attestations(
        authorship_log,
        &fold.added_lines_by_file,
        is_merge_commit,
    );

    let mut stats = stats_from_authorship_log(
        authorship_log,
        fold.git_diff_added_lines,
        fold.git_diff_deleted_lines,
        ai_accepted,
        known_human_accepted,
        &ai_accepted_by_tool,
    );
    fold.apply_git_diff_volume(&mut stats);
    apply_added_volume(
        &mut stats,
        added_bucket_volume_from_attestations(
            authorship_log,
            &fold.added_payloads_by_file,
            is_merge_commit,
        ),
        tool_model_volume_from_attestations(
            authorship_log,
            &fold.added_payloads_by_file,
            is_merge_commit,
        ),
    );
    stats
}

/// Line and char/byte totals folded from diff hunks, plus the per-file added
/// lines and (line, payload) pairs that attribution and bucket volume use.
pub(crate) struct DiffHunkFold<'a> {
    pub git_diff_added_lines: u32,
    pub git_diff_deleted_lines: u32,
    pub git_diff_added_chars: u64,
    pub git_diff_added_bytes: u64,
    pub git_diff_deleted_chars: u64,
    pub git_diff_deleted_bytes: u64,
    /// Sorted, deduplicated added line numbers per file. Empty for merges.
    pub added_lines_by_file: HashMap<String, Vec<u32>>,
    /// (line, payload) pairs sorted by line per file. Empty for merges and
    /// for files cap-skipped by `stats_max_file_bytes`.
    pub added_payloads_by_file: HashMap<String, Vec<(u32, &'a str)>>,
}

impl DiffHunkFold<'_> {
    pub(crate) fn apply_git_diff_volume(&self, stats: &mut CommitStats) {
        stats.git_diff_added_chars = self.git_diff_added_chars;
        stats.git_diff_added_bytes = self.git_diff_added_bytes;
        stats.git_diff_deleted_chars = self.git_diff_deleted_chars;
        stats.git_diff_deleted_bytes = self.git_diff_deleted_bytes;
    }
}

/// Write bucket and per-tool char/byte volume onto `stats`. Tools only appear
/// in `tool_volume` when they have accepted lines, so every key matches an
/// existing breakdown entry.
pub(crate) fn apply_added_volume(
    stats: &mut CommitStats,
    bucket_volume: AddedBucketVolume,
    tool_volume: BTreeMap<String, (u64, u64)>,
) {
    stats.human_additions_chars = bucket_volume.human_chars;
    stats.human_additions_bytes = bucket_volume.human_bytes;
    stats.unknown_additions_chars = bucket_volume.unknown_chars;
    stats.unknown_additions_bytes = bucket_volume.unknown_bytes;
    stats.ai_additions_chars = bucket_volume.ai_chars;
    stats.ai_additions_bytes = bucket_volume.ai_bytes;
    // ai_accepted mirrors ai_additions, matching the line fields.
    stats.ai_accepted_chars = bucket_volume.ai_chars;
    stats.ai_accepted_bytes = bucket_volume.ai_bytes;

    for (tool_model, (chars, bytes)) in tool_volume {
        if let Some(tool_stats) = stats.tool_model_breakdown.get_mut(&tool_model) {
            tool_stats.ai_additions_chars = chars;
            tool_stats.ai_additions_bytes = bytes;
            tool_stats.ai_accepted_chars = chars;
            tool_stats.ai_accepted_bytes = bytes;
        }
    }
}

/// Fold diff hunks into line and char/byte totals, skipping ignored files.
/// Files whose counted payload exceeds `stats_max_file_bytes` keep their line
/// counts but contribute no char/byte volume.
pub(crate) fn fold_diff_hunks<'a>(
    hunks: &'a [crate::commands::diff::DiffHunk],
    ignore_patterns: &[String],
    is_merge_commit: bool,
) -> DiffHunkFold<'a> {
    let ignore_matcher = build_ignore_matcher(ignore_patterns);

    // Per-hunk payload volume from the in-memory hunk text. Ignored files get
    // `None` so the main loop can skip them without re-running the matcher.
    struct HunkVolume {
        added_chars: u64,
        added_bytes: u64,
        deleted_chars: u64,
        deleted_bytes: u64,
    }
    let mut hunk_volumes: Vec<Option<HunkVolume>> = Vec::with_capacity(hunks.len());
    // Per-file counted UTF-8 payload bytes (added + deleted line payloads,
    // each counted line carrying one synthetic LF) for the cap check.
    let mut payload_bytes_by_file: HashMap<&str, u64> = HashMap::new();
    for hunk in hunks {
        if should_ignore_file_with_matcher(&hunk.file_path, &ignore_matcher) {
            hunk_volumes.push(None);
            continue;
        }
        // Same min-length zip semantics as the main fold: binary hunks carry
        // no contents and contribute 0 volume.
        let added_zip_len = hunk.added_lines.len().min(hunk.added_contents.len());
        let (added_chars, added_bytes) =
            count_line_payload_volume(&hunk.added_contents[..added_zip_len]);
        let deleted_zip_len = hunk.deleted_lines.len().min(hunk.deleted_contents.len());
        let (deleted_chars, deleted_bytes) =
            count_line_payload_volume(&hunk.deleted_contents[..deleted_zip_len]);
        let file_bytes = payload_bytes_by_file
            .entry(hunk.file_path.as_str())
            .or_insert(0);
        *file_bytes = file_bytes
            .saturating_add(added_bytes)
            .saturating_add(deleted_bytes);
        hunk_volumes.push(Some(HunkVolume {
            added_chars,
            added_bytes,
            deleted_chars,
            deleted_bytes,
        }));
    }

    // Files whose counted UTF-8 payload exceeds `stats_max_file_bytes` are
    // cap-skipped: they add 0 to every `*_chars` / `*_bytes` field but keep
    // their line counts. The skip is not an error.
    let max_file_bytes = crate::config::Config::get().stats_max_file_bytes() as u64;
    let mut cap_skipped_files: HashSet<&str> = HashSet::new();
    for (file_path, payload_bytes) in &payload_bytes_by_file {
        if *payload_bytes > max_file_bytes {
            cap_skipped_files.insert(file_path);
        }
    }
    if !cap_skipped_files.is_empty() {
        let skipped = cap_skipped_files
            .iter()
            .map(|path| (*path).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        tracing::debug!(
            "stats: skipping char/byte volume for {} over-cap file(s) (stats_max_file_bytes={}): {}",
            cap_skipped_files.len(),
            max_file_bytes,
            skipped
        );
    }

    let mut git_diff_added_lines = 0u32;
    let mut git_diff_deleted_lines = 0u32;
    let mut git_diff_added_chars = 0u64;
    let mut git_diff_added_bytes = 0u64;
    let mut git_diff_deleted_chars = 0u64;
    let mut git_diff_deleted_bytes = 0u64;
    let mut added_lines_by_file: HashMap<String, Vec<u32>> = HashMap::new();
    let mut added_payloads_by_file: HashMap<String, Vec<(u32, &str)>> = HashMap::new();

    for (hunk, volume) in hunks.iter().zip(hunk_volumes.iter()) {
        let Some(volume) = volume else {
            continue;
        };
        // Line counts are unaffected by the cap skip.
        git_diff_added_lines += hunk.added_lines.len() as u32;
        git_diff_deleted_lines += hunk.deleted_lines.len() as u32;

        let cap_skipped = cap_skipped_files.contains(hunk.file_path.as_str());
        if !cap_skipped {
            git_diff_added_chars = git_diff_added_chars.saturating_add(volume.added_chars);
            git_diff_added_bytes = git_diff_added_bytes.saturating_add(volume.added_bytes);
            git_diff_deleted_chars = git_diff_deleted_chars.saturating_add(volume.deleted_chars);
            git_diff_deleted_bytes = git_diff_deleted_bytes.saturating_add(volume.deleted_bytes);
        }

        if !is_merge_commit && !hunk.added_lines.is_empty() {
            added_lines_by_file
                .entry(hunk.file_path.clone())
                .or_default()
                .extend(hunk.added_lines.iter().copied());
            // Cap-skipped files keep their line buckets but add no char/byte
            // volume.
            if !cap_skipped {
                let added_zip_len = hunk.added_lines.len().min(hunk.added_contents.len());
                added_payloads_by_file
                    .entry(hunk.file_path.clone())
                    .or_default()
                    .extend(
                        hunk.added_lines[..added_zip_len].iter().copied().zip(
                            hunk.added_contents[..added_zip_len]
                                .iter()
                                .map(String::as_str),
                        ),
                    );
            }
        }
    }

    for lines in added_lines_by_file.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }
    for pairs in added_payloads_by_file.values_mut() {
        pairs.sort_unstable_by_key(|(line, _)| *line);
        pairs.dedup_by_key(|(line, _)| *line);
    }

    DiffHunkFold {
        git_diff_added_lines,
        git_diff_deleted_lines,
        git_diff_added_chars,
        git_diff_added_bytes,
        git_diff_deleted_chars,
        git_diff_deleted_bytes,
        added_lines_by_file,
        added_payloads_by_file,
    }
}

/// Get git diff statistics between commit and its parent
/// Uses the same diff engine as git ai diff to properly handle renames
pub fn get_git_diff_stats(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<(u32, u32), GitAiError> {
    use crate::commands::diff::get_diff_with_line_numbers;

    let commit_obj = repo.revparse_single(commit_sha)?.peel_to_commit()?;
    let parent_count = commit_obj.parent_count()?;

    // For merge commits, return (0, 0) to match the behavior of `git show --numstat`
    // which shows a combined diff (typically 0 lines for clean merges)
    if parent_count > 1 {
        return Ok((0, 0));
    }

    let from_ref = if parent_count == 0 {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    } else {
        commit_obj.parent(0)?.id()
    };

    // Use the diff engine which properly handles renames with --find-renames=1%
    let hunks = get_diff_with_line_numbers(repo, &from_ref, commit_sha)?;

    let ignore_matcher = build_ignore_matcher(ignore_patterns);
    let mut added_lines = 0u32;
    let mut deleted_lines = 0u32;

    for hunk in hunks {
        if should_ignore_file_with_matcher(&hunk.file_path, &ignore_matcher) {
            continue;
        }
        added_lines += hunk.added_lines.len() as u32;
        deleted_lines += hunk.deleted_lines.len() as u32;
    }

    Ok((added_lines, deleted_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_debug_snapshot;

    #[test]
    fn test_terminal_stats_display() {
        // Test with mixed human/AI stats
        let stats = CommitStats {
            human_additions: 50,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 25,
            git_diff_deleted_lines: 15,
            git_diff_added_lines: 80,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let mixed_output = write_stats_to_terminal(&stats, false);
        assert_debug_snapshot!(mixed_output);

        // Test with AI-only stats
        let ai_stats = CommitStats {
            human_additions: 0,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let ai_only_output = write_stats_to_terminal(&ai_stats, false);
        assert_debug_snapshot!(ai_only_output);

        // Test with human-only stats
        let human_stats = CommitStats {
            human_additions: 75,
            unknown_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 10,
            git_diff_added_lines: 75,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let human_only_output = write_stats_to_terminal(&human_stats, false);
        assert_debug_snapshot!(human_only_output);

        // Test with minimal human contribution (should get at least 2 blocks)
        let minimal_human_stats = CommitStats {
            human_additions: 2,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 102,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let minimal_human_output = write_stats_to_terminal(&minimal_human_stats, false);
        assert_debug_snapshot!(minimal_human_output);

        // Test with deletion-only commit (no additions)
        let deletion_only_stats = CommitStats {
            human_additions: 0,
            unknown_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 25,
            git_diff_added_lines: 0,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let deletion_only_output = write_stats_to_terminal(&deletion_only_stats, false);
        assert_debug_snapshot!(deletion_only_output);

        // --- New test cases for untracked segment ---

        // 18% human / 22% untracked / 60% AI — matches the design example
        let untracked_stats = CommitStats {
            human_additions: 180,
            unknown_additions: 220,
            ai_additions: 600,
            ai_accepted: 462,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 1000,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };
        let with_untracked_output = write_stats_to_terminal(&untracked_stats, false);
        assert_debug_snapshot!(with_untracked_output);

        // untracked exactly at the 1% threshold — should NOT show untracked segment
        let threshold_stats = CommitStats {
            human_additions: 49,
            unknown_additions: 1,
            ai_additions: 50,
            ai_accepted: 50,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };
        let untracked_at_threshold_output = write_stats_to_terminal(&threshold_stats, false);
        assert_debug_snapshot!(untracked_at_threshold_output);

        // untracked just above 1% threshold (~2%) — should show untracked segment
        let above_threshold_stats = CommitStats {
            human_additions: 97,
            unknown_additions: 2,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 99,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };
        let untracked_just_above_output = write_stats_to_terminal(&above_threshold_stats, false);
        assert_debug_snapshot!(untracked_just_above_output);

        // 100% untracked — entire bar is · chars
        let all_untracked_stats = CommitStats {
            human_additions: 0,
            unknown_additions: 100,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };
        let all_untracked_output = write_stats_to_terminal(&all_untracked_stats, false);
        assert_debug_snapshot!(all_untracked_output);

        // OSC 8 hyperlink emitted when is_interactive = true
        // Not a snapshot test — asserts presence of the escape sequence directly.
        let hyperlink_output = write_stats_to_terminal(&untracked_stats, true);
        assert!(
            hyperlink_output.contains("\x1b]8;;https://usegitai.com/docs/cli/untracked\x1b\\"),
            "Expected OSC 8 hyperlink in interactive output, got: {:?}",
            hyperlink_output
        );
        assert!(
            hyperlink_output.contains("untracked"),
            "Expected 'untracked' label in interactive output"
        );
    }

    #[test]
    fn test_markdown_stats_display() {
        // Test with mixed human/AI stats
        let stats = CommitStats {
            human_additions: 50,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 25,
            git_diff_deleted_lines: 15,
            git_diff_added_lines: 80,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let mixed_output = write_stats_to_markdown(&stats);
        assert_debug_snapshot!(mixed_output);

        // Test with AI-only stats
        let ai_stats = CommitStats {
            human_additions: 0,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 100,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let ai_only_output = write_stats_to_markdown(&ai_stats);
        assert_debug_snapshot!(ai_only_output);

        // Test with human-only stats
        let human_stats = CommitStats {
            human_additions: 75,
            unknown_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 10,
            git_diff_added_lines: 75,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let human_only_output = write_stats_to_markdown(&human_stats);
        assert_debug_snapshot!(human_only_output);

        // Test with minimal human contribution (should get at least 2 blocks)
        let minimal_human_stats = CommitStats {
            human_additions: 2,
            unknown_additions: 0,
            ai_additions: 100,
            ai_accepted: 95,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 102,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let minimal_human_output = write_stats_to_markdown(&minimal_human_stats);
        assert_debug_snapshot!(minimal_human_output);

        // Test with deletion-only commit (no additions)
        let deletion_only_stats = CommitStats {
            human_additions: 0,
            unknown_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            git_diff_deleted_lines: 25,
            git_diff_added_lines: 0,
            tool_model_breakdown: BTreeMap::new(),
            ..Default::default()
        };

        let deletion_only_output = write_stats_to_markdown(&deletion_only_stats);
        assert_debug_snapshot!(deletion_only_output);
    }
}
