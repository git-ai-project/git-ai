//! `git-ai activity` — local statistics from persisted metric events.

use crate::metrics::local_stats::{BucketGranularity, LocalActivityStats, compute_activity};
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

    let (since_ts, period_label, granularity) = match period.as_str() {
        "1d" => (days_ago(1), "last 1 day".to_string(), BucketGranularity::Daily),
        "3d" => (days_ago(3), "last 3 days".to_string(), BucketGranularity::Daily),
        "7d" => (days_ago(7), "last 7 days".to_string(), BucketGranularity::Daily),
        "30d" => (days_ago(30), "last 30 days".to_string(), BucketGranularity::Weekly),
        "all" => (0u32, "all time".to_string(), BucketGranularity::Monthly),
        other => {
            eprintln!("Unknown period '{}'. Use 1d, 3d, 7d, 30d, or all.", other);
            std::process::exit(1);
        }
    };

    let stats = match compute_activity(since_ts, period_label, granularity) {
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
    eprintln!("  --period <1d|3d|7d|30d|all>   Time window (default: 30d)");
    eprintln!("  --json                          Output as JSON");
    eprintln!("  --help                          Show this help");
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

    // --- Commits section ---
    println!();
    println!(
        "  {BOLD}Commits with AI{RESET}       {:>6}",
        stats.commits.total
    );

    let total_lines = stats.commits.ai_lines + stats.commits.human_lines;
    if let Some(ai_pct) = (stats.commits.ai_lines * 100).checked_div(total_lines) {
        let human_pct = 100 - ai_pct;
        println!(
            "  AI lines added        {:>6}   {}  {:>3}%",
            format_num(stats.commits.ai_lines),
            bar(ai_pct, BAR_WIDTH),
            ai_pct,
        );
        println!(
            "  Human lines added     {:>6}   {}  {:>3}%",
            format_num(stats.commits.human_lines),
            bar(human_pct, BAR_WIDTH),
            human_pct,
        );
    } else {
        println!(
            "  AI lines added        {:>6}",
            format_num(stats.commits.ai_lines)
        );
        println!(
            "  Human lines added     {:>6}",
            format_num(stats.commits.human_lines)
        );
    }

    if !stats.commits.by_tool.is_empty() {
        let parts: Vec<String> = stats
            .commits
            .by_tool
            .iter()
            .map(|(tool, count)| format!("{}: {}", tool, format_num(*count)))
            .collect();
        println!("  {GRAY}By tool: {}{RESET}", parts.join("  ·  "));
    }

    if let Some(acceptance_pct) =
        (stats.commits.ai_lines * 100).checked_div(stats.checkpoints.ai_lines_added)
        && acceptance_pct <= 100
    {
        println!(
            "  AI acceptance rate    {:>6}   {}  {:>3}%",
            "",
            bar(acceptance_pct, BAR_WIDTH),
            acceptance_pct,
        );
    }

    // --- Checkpoints section ---
    println!();
    println!(
        "  {BOLD}Checkpoints{RESET}           {:>6}",
        format_num(stats.checkpoints.total)
    );

    let total_cp_lines = stats.checkpoints.ai_lines_added + stats.checkpoints.human_lines_added;
    if let Some(ai_pct) = (stats.checkpoints.ai_lines_added * 100).checked_div(total_cp_lines) {
        let human_pct = 100 - ai_pct;
        println!(
            "  AI edits              {:>6}   {}  {:>3}%",
            format_num(stats.checkpoints.ai_lines_added),
            bar(ai_pct, BAR_WIDTH),
            ai_pct,
        );
        println!(
            "  Human edits           {:>6}   {}  {:>3}%",
            format_num(stats.checkpoints.human_lines_added),
            bar(human_pct, BAR_WIDTH),
            human_pct,
        );
    } else {
        println!(
            "  AI edits              {:>6}",
            format_num(stats.checkpoints.ai_lines_added)
        );
        println!(
            "  Human edits           {:>6}",
            format_num(stats.checkpoints.human_lines_added)
        );
    }
    println!(
        "  Files touched         {:>6}",
        format_num(stats.checkpoints.files_edited)
    );

    // --- Sessions section ---
    println!();
    println!(
        "  {BOLD}Sessions{RESET}              {:>6}",
        format_num(stats.sessions.total)
    );

    if !stats.sessions.by_tool.is_empty() {
        let parts: Vec<String> = stats
            .sessions
            .by_tool
            .iter()
            .map(|(tool, count)| format!("{}: {}", tool, count))
            .collect();
        println!("  {GRAY}By tool: {}{RESET}", parts.join("  ·  "));
    }

    // --- Activity over time ---
    if !stats.buckets.is_empty() {
        println!();
        println!("  {BOLD}Activity over time{RESET}");
        let max_ai = stats.buckets.iter().map(|b| b.ai_lines).max().unwrap_or(1).max(1);
        for bucket in &stats.buckets {
            let filled = (bucket.ai_lines * BAR_WIDTH / max_ai).min(BAR_WIDTH);
            let empty = BAR_WIDTH - filled;
            let bar_str = format!("{}{}", "█".repeat(filled as usize), "░".repeat(empty as usize));
            if bucket.ai_lines > 0 {
                println!(
                    "  {GRAY}{}{RESET}  {}  {GRAY}{} lines · {} commits{RESET}",
                    bucket.label,
                    bar_str,
                    format_num(bucket.ai_lines),
                    bucket.commit_count,
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

        // Render two rows: sparkline + hour labels
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
