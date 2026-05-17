use std::path::{Path, PathBuf};
use std::process;

use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::metrics::{CommitStats, FileStats, StatsCache};

use crate::commands::helpers::git_cmd;

pub fn handle_stats(args: &[String]) {
    let mut is_json = false;
    let mut file_filter: Option<String> = None;
    let mut author_filter: Option<String> = None;
    let mut since_filter: Option<String> = None;
    let mut show_help = false;
    let mut positional: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => show_help = true,
            "--json" => is_json = true,
            "--file" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --file requires a value");
                    process::exit(1);
                }
                file_filter = Some(args[i].clone());
            }
            s if s.starts_with("--file=") => {
                file_filter = Some(s.strip_prefix("--file=").unwrap().to_string());
            }
            "--author" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --author requires a value");
                    process::exit(1);
                }
                author_filter = Some(args[i].clone());
            }
            s if s.starts_with("--author=") => {
                author_filter = Some(s.strip_prefix("--author=").unwrap().to_string());
            }
            "--since" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --since requires a value");
                    process::exit(1);
                }
                since_filter = Some(args[i].clone());
            }
            s if s.starts_with("--since=") => {
                since_filter = Some(s.strip_prefix("--since=").unwrap().to_string());
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option '{}'", s);
                process::exit(1);
            }
            _ => {
                if positional.is_none() {
                    positional = Some(arg.to_string());
                }
            }
        }
        i += 1;
    }

    if show_help {
        println!("usage: git-ai stats [options] [<commit-ref>|<range>]");
        println!();
        println!("Show attribution statistics.");
        println!();
        println!("Options:");
        println!("  --file <path>     Show stats for a specific file");
        println!("  --author <name>   Filter by git author");
        println!("  --since <date>    Only include commits after this date");
        println!("  --json            Output as JSON");
        return;
    }

    let commit_ref = positional.as_deref().unwrap_or("HEAD");
    let is_range = commit_ref.contains("..");

    if is_range {
        handle_range_stats(
            commit_ref,
            is_json,
            &file_filter,
            &author_filter,
            &since_filter,
        );
    } else {
        handle_single_commit_stats(commit_ref, is_json);
    }
}

fn handle_single_commit_stats(commit_ref: &str, is_json: bool) {
    let commit_sha = match git_cmd(&["rev-parse", commit_ref]) {
        Ok(s) => s.trim().to_string(),
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let note = match git_cmd(&["notes", "--ref=ai", "show", &commit_sha]) {
        Ok(n) => n,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let log = match AuthorshipLog::deserialize_from_string(&note) {
        Ok(l) => l,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let mut ai_additions: u64 = 0;
    let mut human_additions: u64 = 0;
    let mut untracked_additions: u64 = 0;

    let mut files = Vec::new();
    for file_att in &log.attestations {
        let mut file_ai: u64 = 0;
        let mut file_human: u64 = 0;
        let mut file_untracked: u64 = 0;
        for entry in &file_att.entries {
            let count: u64 = entry
                .line_ranges
                .iter()
                .map(|r| r.line_count() as u64)
                .sum();
            if entry.hash.starts_with("h_") {
                file_human += count;
            } else if entry.hash == "untracked" || entry.hash.is_empty() {
                file_untracked += count;
            } else {
                file_ai += count;
            }
        }
        ai_additions += file_ai;
        human_additions += file_human;
        untracked_additions += file_untracked;
        files.push(FileStats {
            path: file_att.file_path.clone(),
            ai_lines: file_ai,
            human_lines: file_human,
            untracked_lines: file_untracked,
        });
    }

    let (diff_added, diff_deleted) = get_diff_stats_for_commit(&commit_sha);

    if is_json {
        println!(
            "{}",
            serde_json::json!({
                "ai_additions": ai_additions,
                "human_additions": human_additions,
                "untracked_additions": untracked_additions,
                "git_diff_added_lines": diff_added,
                "git_diff_deleted_lines": diff_deleted,
                "files": { "total": {} }
            })
        );
    } else {
        render_stats_bar(human_additions, untracked_additions, ai_additions);
    }

    // Cache the result
    let git_dir = resolve_git_dir();
    let _ = StatsCache::put(
        &git_dir,
        &CommitStats {
            commit_sha,
            ai_lines: ai_additions,
            human_lines: human_additions,
            untracked_lines: untracked_additions,
            files,
            cached_at: current_timestamp(),
        },
    );
}

fn handle_range_stats(
    range: &str,
    is_json: bool,
    file_filter: &Option<String>,
    author_filter: &Option<String>,
    since_filter: &Option<String>,
) {
    let mut log_args: Vec<String> = vec![
        "log".to_string(),
        "--format=%H".to_string(),
        range.to_string(),
    ];

    if let Some(author) = author_filter {
        log_args.push(format!("--author={}", author));
    }
    if let Some(since) = since_filter {
        log_args.push(format!("--since={}", since));
    }
    if let Some(file) = file_filter {
        log_args.push("--".to_string());
        log_args.push(file.clone());
    }

    let log_args_refs: Vec<&str> = log_args.iter().map(|s| s.as_str()).collect();
    let log_output = match git_cmd(&log_args_refs) {
        Ok(output) => output,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let commits: Vec<String> = log_output
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if commits.is_empty() {
        if is_json {
            println!("{{}}");
        } else {
            println!("No commits in range.");
        }
        return;
    }

    let git_dir = resolve_git_dir();
    let mut total_ai: u64 = 0;
    let mut total_human: u64 = 0;
    let mut total_untracked: u64 = 0;
    let mut total_diff_added: u64 = 0;
    let mut total_diff_deleted: u64 = 0;

    for sha in &commits {
        let (ai, human, untracked) = get_attribution_for_commit(&git_dir, sha);
        total_ai += ai;
        total_human += human;
        total_untracked += untracked;
        let (added, deleted) = get_diff_stats_for_commit(sha);
        total_diff_added += added;
        total_diff_deleted += deleted;
    }

    if is_json {
        println!(
            "{}",
            serde_json::json!({
                "ai_additions": total_ai,
                "human_additions": total_human,
                "untracked_additions": total_untracked,
                "git_diff_added_lines": total_diff_added,
                "git_diff_deleted_lines": total_diff_deleted,
                "commits_analyzed": commits.len(),
            })
        );
    } else {
        render_stats_bar(total_human, total_untracked, total_ai);
    }
}

fn get_attribution_for_commit(git_dir: &Path, sha: &str) -> (u64, u64, u64) {
    if let Some(cached) = StatsCache::get(git_dir, sha) {
        return (cached.ai_lines, cached.human_lines, cached.untracked_lines);
    }

    let note = match git_cmd(&["notes", "--ref=ai", "show", sha]) {
        Ok(n) => n,
        Err(_) => return (0, 0, 0),
    };

    let log = match AuthorshipLog::deserialize_from_string(&note) {
        Ok(l) => l,
        Err(_) => return (0, 0, 0),
    };

    let mut ai: u64 = 0;
    let mut human: u64 = 0;
    let mut untracked: u64 = 0;

    for file_att in &log.attestations {
        for entry in &file_att.entries {
            let count: u64 = entry
                .line_ranges
                .iter()
                .map(|r| r.line_count() as u64)
                .sum();
            if entry.hash.starts_with("h_") {
                human += count;
            } else if entry.hash == "untracked" || entry.hash.is_empty() {
                untracked += count;
            } else {
                ai += count;
            }
        }
    }

    (ai, human, untracked)
}

fn render_stats_bar(human: u64, untracked: u64, ai: u64) {
    const BAR_WIDTH: usize = 40;

    let total = human + untracked + ai;
    if total == 0 {
        let bar = format!("you  {:>bar_w$} ai", "", bar_w = BAR_WIDTH);
        println!("{}", bar);
        println!("     {:^bar_w$}", "(no data)", bar_w = BAR_WIDTH);
        return;
    }

    let untracked_pct_raw = untracked as f64 / total as f64 * 100.0;
    let show_untracked = untracked_pct_raw > 1.0;

    let human_bars = ((human as f64 / total as f64) * BAR_WIDTH as f64) as usize;
    let min_human_bars = if human > 1 { 2 } else { 0 };
    let final_human_bars = human_bars.max(min_human_bars);

    let remaining = BAR_WIDTH.saturating_sub(final_human_bars);
    let (final_untracked_bars, final_ai_bars) = if show_untracked {
        let other_total = untracked + ai;
        let ub = if other_total > 0 {
            ((untracked as f64 / other_total as f64) * remaining as f64) as usize
        } else {
            0
        };
        (ub, remaining.saturating_sub(ub))
    } else {
        (0, remaining)
    };

    let bar = format!(
        "you  {}{}{} ai",
        "█".repeat(final_human_bars),
        "▒".repeat(final_untracked_bars),
        "░".repeat(final_ai_bars),
    );
    println!("{}", bar);

    let human_pct = (human as f64 / total as f64 * 100.0).round() as u32;
    let ai_pct = (ai as f64 / total as f64 * 100.0).round() as u32;

    if show_untracked {
        let untracked_pct = untracked_pct_raw.round() as u32;
        println!(
            "     {:<3}%{:>10}untracked{:>4}%{:>10}{:>3}%",
            human_pct, "", untracked_pct, "", ai_pct
        );
    } else {
        println!(
            "     {}%{:>width$}{}%",
            human_pct,
            "",
            ai_pct,
            width = BAR_WIDTH - 2
        );
    }
}

/// Get diff stats for a commit, filtering out lockfiles and generated files.
fn get_diff_stats_for_commit(sha: &str) -> (u64, u64) {
    let output = match git_cmd(&["diff", "--numstat", &format!("{}^..{}", sha, sha)]) {
        Ok(o) => o,
        Err(_) => {
            // First commit has no parent; try against empty tree
            let empty_tree = "4b825dc642cb6eb9a060e54bf899d69f82cf7174";
            match git_cmd(&["diff", "--numstat", empty_tree, sha]) {
                Ok(o) => o,
                Err(_) => return (0, 0),
            }
        }
    };

    let mut added: u64 = 0;
    let mut deleted: u64 = 0;

    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let file_path = parts[2];

        if should_exclude_file(file_path) {
            continue;
        }

        // Binary files show "-" for additions/deletions
        if parts[0] == "-" || parts[1] == "-" {
            continue;
        }

        added += parts[0].parse::<u64>().unwrap_or(0);
        deleted += parts[1].parse::<u64>().unwrap_or(0);
    }

    (added, deleted)
}

fn should_exclude_file(path: &str) -> bool {
    let lockfiles = [
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Cargo.lock",
        "Gemfile.lock",
        "poetry.lock",
        "composer.lock",
        "go.sum",
        "flake.lock",
    ];

    let basename = path.rsplit('/').next().unwrap_or(path);

    for lock in &lockfiles {
        if basename == *lock {
            return true;
        }
    }

    if basename.contains(".generated.")
        || basename.contains(".gen.")
        || basename.ends_with(".generated")
        || basename.ends_with(".min.js")
        || basename.ends_with(".min.css")
    {
        return true;
    }

    false
}

fn resolve_git_dir() -> PathBuf {
    match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(dir) => {
            let p = PathBuf::from(dir.trim());
            if p.is_relative() {
                std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
            } else {
                p
            }
        }
        Err(_) => PathBuf::from(".git"),
    }
}

fn current_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_flag() {
        handle_stats(&["--help".to_string()]);
    }

    #[test]
    fn test_current_timestamp_format() {
        let ts = current_timestamp();
        assert!(ts.ends_with('Z'));
        let num_part = &ts[..ts.len() - 1];
        assert!(num_part.parse::<u64>().is_ok());
    }

    #[test]
    fn test_should_exclude_lockfiles() {
        assert!(should_exclude_file("Cargo.lock"));
        assert!(should_exclude_file("package-lock.json"));
        assert!(should_exclude_file("some/path/yarn.lock"));
        assert!(!should_exclude_file("src/main.rs"));
        assert!(!should_exclude_file("README.md"));
    }

    #[test]
    fn test_should_exclude_generated() {
        assert!(should_exclude_file("api.generated.ts"));
        assert!(should_exclude_file("schema.gen.go"));
        assert!(should_exclude_file("bundle.min.js"));
        assert!(!should_exclude_file("src/generated_code.rs"));
    }

    #[test]
    fn test_output_file_stats_missing_file() {
        // Doesn't panic
        let _stats: std::collections::HashMap<String, (u64, u64, u64)> =
            std::collections::HashMap::new();
    }
}
