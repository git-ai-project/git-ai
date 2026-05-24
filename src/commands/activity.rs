//! `git-ai usage` — local statistics from persisted metric events.

use crate::metrics::local_stats::{
    BucketGranularity, LocalActivityStats, RepoActivitySummary, compute_activity,
    compute_repo_summaries,
};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn handle_activity(args: &[String]) {
    let mut json = false;
    let mut period = "30d".to_string();
    let mut repo_filter: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--period" if i + 1 < args.len() => {
                period = args[i + 1].clone();
                i += 1;
            }
            "--repo" if i + 1 < args.len() => {
                // Normalize: strip protocol prefix so both "https://github.com/org/repo"
                // and "github.com/org/repo" resolve to the same substring match.
                repo_filter = Some(strip_protocol(args[i + 1].as_str()).to_string());
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                eprintln!("Run 'git-ai usage --help' for usage.");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let (since_ts, period_label, granularity) = match period.as_str() {
        "1d" => (
            days_ago(1),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        ),
        "3d" => (
            days_ago(3),
            "last 3 days".to_string(),
            BucketGranularity::Daily,
        ),
        "7d" => (
            days_ago(7),
            "last 7 days".to_string(),
            BucketGranularity::Daily,
        ),
        "30d" => (
            days_ago(30),
            "last 30 days".to_string(),
            BucketGranularity::Weekly,
        ),
        "60d" => (
            days_ago(60),
            "last 60 days".to_string(),
            BucketGranularity::Weekly,
        ),
        "all" => (0u32, "all time".to_string(), BucketGranularity::Monthly),
        other => {
            eprintln!(
                "Unknown period '{}'. Use 1d, 3d, 7d, 30d, 60d, or all.",
                other
            );
            std::process::exit(1);
        }
    };

    let stats = match compute_activity(since_ts, period_label, granularity, repo_filter.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };

    // Compute per-repo breakdown. When a filter is active this still runs so
    // we can surface how many repos matched (and who they are). The breakdown
    // is only rendered when more than one repo is present — a single-row table
    // adds nothing.
    let repos = compute_repo_summaries(since_ts, granularity, repo_filter.as_deref())
        .unwrap_or_default();

    // When filtering by repo, bail out early if nothing matched.
    // Include human_lines/diff_added_lines so human-only periods aren't
    // falsely reported as empty (commits.total only counts AI-involved commits).
    // Also include checkpoint lines so checkpoint-only activity isn't missed.
    let no_data = stats.commits.total == 0
        && stats.commits.human_lines == 0
        && stats.commits.diff_added_lines == 0
        && stats.sessions.total == 0
        && stats.checkpoints.ai_lines_added == 0
        && stats.checkpoints.human_lines_added == 0
        && stats.tokens.input + stats.tokens.output + stats.tokens.cache_read + stats.tokens.cache_creation == 0;
    if no_data {
        if let Some(ref filter) = repo_filter {
            eprintln!(
                "No data found for '{}' in the {} window.",
                filter, stats.period_label
            );
            eprintln!("Try a broader period (--period all) or a different substring.");
        } else {
            eprintln!("No activity data found for the {} window.", stats.period_label);
        }
        std::process::exit(1);
    }

    if json {
        match serde_json::to_string_pretty(&stats) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("error serializing JSON: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        print_terminal(&stats, &repos, repo_filter.as_deref());
    }
}

fn days_ago(days: u64) -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(days * 24 * 3600) as u32
}

fn print_help() {
    eprintln!("git-ai usage - Show local activity statistics");
    eprintln!();
    eprintln!("Usage: git-ai usage [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --period <1d|3d|7d|30d|60d|all>   Time window (default: 30d)");
    eprintln!("  --repo <url|substring>            Filter to a repository (substring match, https:// optional)");
    eprintln!("  --json                            Output as JSON");
    eprintln!("  --help                            Show this help");
    eprintln!();
    eprintln!("Statistics are sourced from locally recorded metric events.");
    eprintln!("Events accumulate over time and are never deleted from local storage.");
}

fn print_terminal(stats: &LocalActivityStats, repos: &[RepoActivitySummary], repo_filter: Option<&str>) {
    const GRAY: &str = "\x1b[90m";
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";
    const BAR_WIDTH: u32 = 20;

    if let Some(repo) = repo_filter {
        let display = strip_protocol(repo);
        if repos.len() > 1 {
            println!(
                "{BOLD}git-ai usage{RESET} {GRAY}— {} repos matching '{}'  ·  {}{RESET}",
                repos.len(),
                display,
                stats.period_label
            );
        } else {
            // Single match: show the full matched URL, not just the search term.
            let matched = repos
                .first()
                .map(|r| strip_protocol(&r.repo_url))
                .unwrap_or(display);
            println!(
                "{BOLD}git-ai usage{RESET} {GRAY}— {}  ·  {}{RESET}",
                matched, stats.period_label
            );
        }
    } else {
        println!(
            "{BOLD}git-ai usage{RESET} {GRAY}— {}{RESET}",
            stats.period_label
        );
    }

    // --- Top bar: AI vs Human split ---
    println!();
    let total_lines = stats.commits.ai_lines + stats.commits.human_lines;
    if let Some(ai_pct) = (stats.commits.ai_lines * 100).checked_div(total_lines) {
        let human_pct = 100 - ai_pct;
        println!(
            "  {}  {BOLD}AI{RESET} {:>3}% · {BOLD}Human{RESET} {:>3}%",
            bar(ai_pct, 40),
            ai_pct,
            human_pct,
        );
    }

    // --- Per-repo breakdown ---
    // Only shown when there are multiple repos — a single-row table adds nothing.
    if repos.len() > 1 {
        println!();
        println!("  {BOLD}Repositories{RESET}");

        // Pre-compute display strings for column alignment.
        let names: Vec<&str> = repos
            .iter()
            .map(|r| {
                let d = strip_protocol(&r.repo_url);
                if d.is_empty() { "unknown" } else { d }
            })
            .collect();
        let lines_strs: Vec<String> = repos.iter().map(|r| format_num(r.ai_lines)).collect();
        let commit_strs: Vec<String> = repos.iter().map(|r| format_num(r.commits)).collect();
        let session_strs: Vec<String> = repos.iter().map(|r| format_num(r.sessions)).collect();

        let max_name_w = names.iter().map(|n| n.len()).max().unwrap_or(0);
        let max_lines_w = lines_strs.iter().map(|s| s.len()).max().unwrap_or(0);
        let max_commits_w = commit_strs.iter().map(|s| s.len()).max().unwrap_or(0);
        let max_sessions_w = session_strs.iter().map(|s| s.len()).max().unwrap_or(0);

        for (i, r) in repos.iter().enumerate() {
            let name_col = format!("{:<width$}", names[i], width = max_name_w);
            let lines_col = format!("{:>width$}", lines_strs[i], width = max_lines_w);
            let commits_col = format!("{:>width$}", commit_strs[i], width = max_commits_w);
            let sessions_col = format!("{:>width$}", session_strs[i], width = max_sessions_w);
            // Pad singular labels to match the width of the plural so columns stay aligned.
            let commit_label = if r.commits == 1 { "commit " } else { "commits" };
            let session_label = if r.sessions == 1 { "session " } else { "sessions" };
            let cost_str = if r.estimated_cost_usd > 0.0 {
                format!("  {GRAY}{}{RESET}", format_cost(r.estimated_cost_usd))
            } else {
                String::new()
            };
            println!(
                "    {GRAY}{}  {} lines  {} {}  {} {}{}{RESET}",
                name_col,
                lines_col,
                commits_col,
                commit_label,
                sessions_col,
                session_label,
                cost_str,
            );
        }
    }

    // --- AI section ---
    println!();
    println!("  {BOLD}AI{RESET}");
    let yield_total = stats.sessions.yield_stats.shipped + stats.sessions.yield_stats.abandoned;
    if let Some(shipped_pct) = (stats.sessions.yield_stats.shipped * 100).checked_div(yield_total) {
        println!(
            "    Sessions          {:>6}  {GRAY}({} shipped · {} abandoned · {}% yield){RESET}",
            format_num(stats.sessions.total),
            format_num(stats.sessions.yield_stats.shipped),
            format_num(stats.sessions.yield_stats.abandoned),
            shipped_pct,
        );
    } else {
        println!(
            "    Sessions          {:>6}",
            format_num(stats.sessions.total)
        );
    }
    println!(
        "    Commits (AI)      {:>6}",
        format_num(stats.commits.total)
    );
    println!(
        "    Lines committed   {:>6}",
        format_num(stats.commits.ai_lines)
    );
    println!(
        "    Edits             {:>6}",
        format_num(stats.checkpoints.ai_lines_added)
    );
    // Show acceptance rate: range when multiple tools have valid data, single value otherwise.
    let valid_tool_rates: Vec<u32> = stats
        .commits
        .acceptance_by_tool
        .iter()
        .filter(|(_, pct)| *pct <= 100)
        .map(|(_, pct)| *pct)
        .collect();
    if valid_tool_rates.len() >= 2 {
        let min_r = *valid_tool_rates.iter().min().unwrap();
        let max_r = *valid_tool_rates.iter().max().unwrap();
        if min_r == max_r {
            println!("    Acceptance rate   {:>5}%", min_r);
        } else {
            println!("    Acceptance rate   {GRAY}{min_r}–{max_r}%{RESET}");
        }
    } else if let Some(acceptance_pct) =
        (stats.commits.ai_lines * 100).checked_div(stats.checkpoints.ai_lines_added)
    {
        if acceptance_pct <= 100 {
            println!("    Acceptance rate   {:>5}%", acceptance_pct);
        } else {
            // >100% means checkpoint data is incomplete (pre-backfill events).
            println!("    Acceptance rate     {GRAY}>100% (incomplete checkpoint data){RESET}");
        }
    }
    // Track which tools have already had their acceptance rate shown so we
    // don't repeat the same tool-level rate on every model variant line.
    let mut shown_accept: HashSet<&str> = HashSet::new();
    for (tool, count) in &stats.commits.by_tool {
        let tool_name = tool.split(" · ").next().unwrap_or(tool.as_str());
        let accept_str = if shown_accept.insert(tool_name) {
            // First line for this tool — show the acceptance rate once.
            stats
                .commits
                .acceptance_by_tool
                .iter()
                .find(|(t, _)| t == tool_name)
                .map(|(_, pct)| {
                    if *pct <= 100 {
                        format!("  {GRAY}({pct}% accept){RESET}")
                    } else {
                        format!("  {GRAY}(>100% accept — incomplete checkpoint data){RESET}")
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        println!(
            "    {GRAY}{}: {} lines{RESET}{}",
            tool,
            format_num(*count),
            accept_str
        );
    }

    // --- Human section ---
    println!();
    println!("  {BOLD}Human{RESET}");
    println!(
        "    Lines committed   {:>6}",
        format_num(stats.commits.human_lines)
    );
    println!(
        "    Edits             {:>6}",
        format_num(stats.checkpoints.human_lines_added)
    );

    // --- Tokens section ---
    let t = &stats.tokens;
    if t.input + t.output + t.cache_read + t.cache_creation > 0 {
        println!();
        println!("  {BOLD}Tokens{RESET} {GRAY}(estimated cost){RESET}");
        println!("    Input             {:>12}", format_num_u64(t.input));
        println!("    Output            {:>12}", format_num_u64(t.output));
        println!("    Cache read        {:>12}", format_num_u64(t.cache_read));
        println!(
            "    Cache write       {:>12}",
            format_num_u64(t.cache_creation)
        );
        if t.estimated_cost_usd > 0.0 {
            println!(
                "    {BOLD}Est. cost{RESET}         {:>12}",
                format_cost(t.estimated_cost_usd)
            );
        }
        if let Some(wow) = &t.wow_spend {
            // When last week had no spend, "new this week" is redundant — skip the label.
            let change_str = match (wow.new_this_week, wow.change_pct) {
                (true, _) => String::new(),
                (_, Some(change_pct)) if change_pct > 0.0 => {
                    format!("↑ {:.0}% vs last week", change_pct)
                }
                (_, Some(change_pct)) if change_pct < 0.0 => {
                    format!("↓ {:.0}% vs last week", change_pct.abs())
                }
                _ => "no change vs last week".to_string(),
            };
            // Avoid printing "$-0.00" when last week rounds to zero.
            let last_week_str = if wow.last_week_usd.abs() < 0.005 {
                "$0".to_string()
            } else {
                format_cost(wow.last_week_usd)
            };
            let trail = if change_str.is_empty() {
                String::new()
            } else {
                format!("  {change_str}")
            };
            println!(
                "    {GRAY}This week {} · Last week {}{}{RESET}",
                format_cost(wow.this_week_usd),
                last_week_str,
                trail,
            );
        }
        for m in &t.by_model {
            let cost = m
                .estimated_cost_usd
                .map(|c| format!("  {}", format_cost(c)))
                .unwrap_or_default();
            let cache = m
                .cache_hit_ratio
                .map(|r| format!("  cache {:.0}% hit", r * 100.0))
                .unwrap_or_default();
            let total = m.input + m.output + m.cache_read + m.cache_creation;
            println!(
                "    {GRAY}{}: {} tokens{}{}{RESET}",
                m.model,
                format_num_u64(total),
                cost,
                cache,
            );
        }
    }

    // --- Activity over time ---
    if !stats.buckets.is_empty() {
        println!();
        println!("  {BOLD}Activity over time{RESET}");
        let max_ai = stats
            .buckets
            .iter()
            .map(|b| b.ai_lines)
            .max()
            .unwrap_or(1)
            .max(1);
        for bucket in &stats.buckets {
            let bar_str = ratio_bar(bucket.ai_lines, max_ai, BAR_WIDTH);
            if bucket.ai_lines > 0 {
                // Coverage for this bucket: attributed / total diff additions.
                let coverage = (bucket.attributed_lines * 100)
                    .checked_div(bucket.diff_added_lines)
                    .map(|pct| format!(" · {}% attributed", pct))
                    .unwrap_or_default();
                println!(
                    "  {GRAY}{}{RESET}  {}  {GRAY}{} lines · {} commits{}{RESET}",
                    bucket.label,
                    bar_str,
                    format_num(bucket.ai_lines),
                    bucket.commit_count,
                    coverage,
                );
            } else {
                println!("  {GRAY}{}  {}{RESET}", bucket.label, bar_str);
            }
        }
    }

    // --- Time of day heatmap ---
    if stats.hourly.iter().any(|&v| v > 0) {
        println!();
        println!("  {BOLD}Time of day{RESET} {GRAY}(AI lines committed){RESET}");
        let max_hour = stats.hourly.iter().copied().max().unwrap_or(1).max(1);

        // Each slot is 3 chars: spark char + 2 spaces. Labels are left-padded to 3.
        let spark: String = stats
            .hourly
            .iter()
            .map(|&v| spark_char(v, max_hour))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {}", spark);

        let labels: Vec<String> = (0..24)
            .map(|h| match h {
                0 => "am".to_string(),
                12 => "pm".to_string(),
                h if h < 12 => format!("{h}"),
                h => format!("{}", h - 12),
            })
            .collect();
        let label_row: String = labels
            .iter()
            .map(|l| format!("{:<3}", l))
            .collect::<Vec<_>>()
            .join("");
        println!("  {GRAY}{}{RESET}", label_row.trim_end());
    }

    // --- Day of week heatmap ---
    if stats.daily.iter().any(|&v| v > 0) {
        println!();
        println!("  {BOLD}Day of week{RESET} {GRAY}(AI lines committed){RESET}");
        let max_day = stats.daily.iter().copied().max().unwrap_or(1).max(1);
        let spark: String = stats
            .daily
            .iter()
            .map(|&v| spark_char(v, max_day))
            .collect::<Vec<_>>()
            .join("    ");
        println!("  {}", spark);
        let label_row = "Mon  Tue  Wed  Thu  Fri  Sat  Sun";
        println!("  {GRAY}{}{RESET}", label_row);
    }

    println!();
    if repo_filter.is_none() {
        println!("  {GRAY}Tip: use --repo <name> to filter by repository{RESET}");
    }
    println!(
        "  {GRAY}Local data only · See full history and team insights at https://usegitai.com/dashboard{RESET}"
    );
    println!();
}

fn spark_char(value: u32, max: u32) -> &'static str {
    if value == 0 {
        return "·";
    }
    let pct = value * 8 / max;
    match pct {
        0 => "▁",
        1 => "▂",
        2 => "▃",
        3 => "▄",
        4 => "▅",
        5 => "▆",
        6 => "▇",
        _ => "█",
    }
}

/// Strip `https://` or `http://` from a URL for display purposes.
fn strip_protocol(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
}

/// Render a block bar where `value` out of `max` determines the fill ratio.
fn ratio_bar(value: u32, max: u32, width: u32) -> String {
    let filled = if max > 0 { (value * width / max).min(width) } else { 0 };
    let empty = width - filled;
    format!("{}{}", "█".repeat(filled as usize), "░".repeat(empty as usize))
}

fn bar(pct: u32, width: u32) -> String {
    ratio_bar(pct, 100, width)
}

/// Format a USD cost estimate. Rounds to whole dollars for amounts >= $10
/// (estimates don't warrant cent-level precision at that scale); shows cents otherwise.
fn format_cost(usd: f64) -> String {
    if usd >= 10.0 {
        format!("~${:.0}", usd)
    } else {
        format!("~${:.2}", usd)
    }
}

fn format_num(n: u32) -> String {
    format_num_u64(n as u64)
}

fn format_num_u64(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}
