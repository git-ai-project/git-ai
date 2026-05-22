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
        "60d" => (days_ago(60), "last 60 days".to_string(), BucketGranularity::Weekly),
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
            "  {}  {BOLD}AI{RESET} {:>3}%  ·  {BOLD}Human{RESET} {:>3}%",
            bar(ai_pct, 40),
            ai_pct,
            human_pct,
        );
    }

    // --- Attribution coverage: how much committed code we confidently attributed ---
    let attributed = stats.commits.ai_lines + stats.commits.human_lines;
    if let Some(coverage_pct) = (attributed * 100).checked_div(stats.commits.diff_added_lines) {
        let untracked = stats.commits.diff_added_lines.saturating_sub(attributed);
        let untracked_pct = 100 - coverage_pct;
        println!(
            "  {}  {BOLD}Attributed{RESET} {:>3}%  {GRAY}· {} untracked ({}%){RESET}",
            bar(coverage_pct, 40),
            coverage_pct,
            format_num(untracked),
            untracked_pct,
        );
    }

    // --- AI section ---
    println!();
    println!("  {BOLD}AI{RESET}");
    println!(
        "    Sessions          {:>6}",
        format_num(stats.sessions.total)
    );
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
    for (tool, count) in &stats.commits.by_tool {
        println!("    {GRAY}{}: {}{RESET}", tool, format_num(*count));
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
