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
                let raw = args[i + 1].as_str();
                let normalized = raw
                    .trim_start_matches("https://")
                    .trim_start_matches("http://");
                repo_filter = Some(normalized.to_string());
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
    let no_data = stats.commits.total == 0
        && stats.sessions.total == 0
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
        let display = repo
            .trim_start_matches("https://")
            .trim_start_matches("http://");
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
                .map(|r| r.repo_url.trim_start_matches("https://").trim_start_matches("http://"))
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
        let max_lines = repos.iter().map(|r| r.ai_lines).max().unwrap_or(1).max(1);
        for r in repos {
            let repo_display = r
                .repo_url
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            let repo_display = if repo_display.is_empty() {
                "unknown"
            } else {
                repo_display
            };
            let filled = (r.ai_lines * 16 / max_lines).min(16);
            let empty = 16 - filled;
            let bar_str = format!(
                "{}{}",
                "█".repeat(filled as usize),
                "░".repeat(empty as usize)
            );
            let cost_str = if r.estimated_cost_usd > 0.0 {
                format!("  {GRAY}~${:.2}{RESET}", r.estimated_cost_usd)
            } else {
                String::new()
            };
            println!(
                "    {}  {GRAY}{}{RESET}  {GRAY}{} lines · {} commits · {} sessions{}{RESET}",
                bar_str,
                repo_display,
                format_num(r.ai_lines),
                r.commits,
                r.sessions,
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
    if let Some(acceptance_pct) =
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
            "    {GRAY}{}: {}{RESET}{}",
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
                format!("~${:.2}", t.estimated_cost_usd)
            );
        }
        if let Some(wow) = &t.wow_spend {
            let change_str = match (wow.new_this_week, wow.change_pct) {
                (true, _) => "↑ new this week".to_string(),
                (_, Some(change_pct)) if change_pct > 0.0 => {
                    format!("↑ {:.0}% vs last week", change_pct)
                }
                (_, Some(change_pct)) if change_pct < 0.0 => {
                    format!("↓ {:.0}% vs last week", change_pct.abs())
                }
                _ => "no change vs last week".to_string(),
            };
            println!(
                "    {GRAY}This week ~${:.2} · Last week ~${:.2}  {}{RESET}",
                wow.this_week_usd, wow.last_week_usd, change_str,
            );
        }
        for m in &t.by_model {
            let cost = m
                .estimated_cost_usd
                .map(|c| format!("  ~${:.2}", c))
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
            let filled = (bucket.ai_lines * BAR_WIDTH / max_ai).min(BAR_WIDTH);
            let empty = BAR_WIDTH - filled;
            let bar_str = format!(
                "{}{}",
                "█".repeat(filled as usize),
                "░".repeat(empty as usize)
            );
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
        println!("");
        println!(
            "  {GRAY}Local data only · See full history and team insights at https://usegitai.com/dashboard{RESET}"
        );
    } else {
        println!(
            "  {GRAY}Local data only · See full history and team insights at https://usegitai.com/dashboard{RESET}"
        );
    }
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

fn bar(pct: u32, width: u32) -> String {
    let filled = (pct * width / 100).min(width);
    let empty = width - filled;
    format!(
        "{}{}",
        "█".repeat(filled as usize),
        "░".repeat(empty as usize)
    )
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
