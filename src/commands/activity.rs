//! `git-ai activity` — local statistics from persisted metric events.

use crate::commands::activity_tui;
use crate::metrics::local_stats::{
    BucketGranularity, LocalActivityStats, compute_activity, compute_repo_summaries,
};
use crate::repo_url::resolve_repo_url_from_path;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn handle_activity(args: &[String]) {
    let mut json = false;
    let mut period = "30d".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--period" if i + 1 < args.len() => {
                period = args[i + 1].clone();
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => {
                eprintln!("Unknown argument: {}", other);
                eprintln!("Run 'git-ai activity --help' for usage.");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let (since_ts, period_label, granularity, tui_period_idx) = match period.as_str() {
        "1d" => (
            days_ago(1),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
            0usize,
        ),
        "3d" => (
            days_ago(3),
            "last 3 days".to_string(),
            BucketGranularity::Daily,
            1,
        ),
        "7d" => (
            days_ago(7),
            "last 7 days".to_string(),
            BucketGranularity::Daily,
            2,
        ),
        "30d" => (
            days_ago(30),
            "last 30 days".to_string(),
            BucketGranularity::Weekly,
            3,
        ),
        "60d" => (
            days_ago(60),
            "last 60 days".to_string(),
            BucketGranularity::Weekly,
            3,
        ),
        "all" => (0u32, "all time".to_string(), BucketGranularity::Monthly, 4),
        other => {
            eprintln!(
                "Unknown period '{}'. Use 1d, 3d, 7d, 30d, 60d, or all.",
                other
            );
            std::process::exit(1);
        }
    };

    // Auto-detect which repo we're in (if any).  Used for the header label and
    // to decide whether to show the per-repo breakdown table.  Stats themselves
    // are always global — events written before the repo_url column existed have
    // NULL there, so filtering would silently drop all historical data.
    let current_repo = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolve_repo_url_from_path(&cwd));

    let stats = match compute_activity(since_ts, period_label, granularity, None)
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };

    if json {
        match serde_json::to_string_pretty(&stats) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("error serializing JSON: {}", e);
                std::process::exit(1);
            }
        }
    } else if std::io::stdout().is_terminal() {
        // Pre-compute repo summaries for the global view (used when not inside a repo).
        let repo_summaries = if current_repo.is_none() {
            compute_repo_summaries(since_ts, granularity).unwrap_or_default()
        } else {
            vec![]
        };
        if let Err(e) = activity_tui::run_tui(stats, tui_period_idx, current_repo, repo_summaries) {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    } else {
        print_terminal(&stats);
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
    eprintln!("git-ai activity - Show local activity statistics");
    eprintln!();
    eprintln!("Usage: git-ai activity [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --period <1d|3d|7d|30d|60d|all>   Time window (default: 30d)");
    eprintln!("  --json                            Output as JSON");
    eprintln!("  --help                            Show this help");
    eprintln!();
    eprintln!("Statistics are sourced from locally recorded metric events.");
    eprintln!("Events accumulate over time and are never deleted from local storage.");
}

fn print_terminal(stats: &LocalActivityStats) {
    const GRAY: &str = "\x1b[90m";
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";
    const BAR_WIDTH: u32 = 20;

    println!(
        "{BOLD}git-ai activity{RESET} {GRAY}— {}{RESET}",
        stats.period_label
    );

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
        "    Commits           {:>6}",
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
        && acceptance_pct <= 100
    {
        println!("    Acceptance rate   {:>5}%", acceptance_pct);
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
                .map(|(_, pct)| format!("  {GRAY}({pct}% accept){RESET}"))
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
