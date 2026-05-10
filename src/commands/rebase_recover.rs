use crate::authorship::rebase_recovery;
use crate::git::find_repository_in_path;

pub fn handle_rebase_recover(args: &[String]) {
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("--latest");

    match subcommand {
        "--list" => handle_list(),
        "--latest" => handle_recover_latest(),
        "--apply" => {
            let timestamp = args.get(1).and_then(|s| s.parse::<u64>().ok());
            match timestamp {
                Some(ts) => handle_recover_timestamp(ts),
                None => {
                    eprintln!("Usage: git-ai rebase recover --apply <timestamp>");
                    std::process::exit(1);
                }
            }
        }
        "--help" | "-h" => {
            eprintln!("Usage: git-ai rebase recover [--list | --latest | --apply <timestamp>]");
            eprintln!();
            eprintln!("Restore authorship notes from a pre-rebase snapshot.");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --list              List available recovery snapshots");
            eprintln!("  --latest            Restore from the most recent snapshot (default)");
            eprintln!("  --apply <timestamp> Restore from a specific snapshot");
        }
        _ => {
            eprintln!("Unknown option '{}'. Use --help for usage.", subcommand);
            std::process::exit(1);
        }
    }
}

fn handle_list() {
    let repo = match find_repository_in_path(".") {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    let snapshots = rebase_recovery::list_snapshots(&repo.storage);
    if snapshots.is_empty() {
        eprintln!("No recovery snapshots available.");
        return;
    }

    eprintln!("Available rebase recovery snapshots:");
    eprintln!();
    for snapshot in &snapshots {
        let datetime = format_timestamp(snapshot.timestamp);
        eprintln!(
            "  {} | original_head: {} | {} notes | {} commits",
            datetime,
            &snapshot.original_head[..7.min(snapshot.original_head.len())],
            snapshot.note_entries.len(),
            snapshot.original_commits.len(),
        );
    }
    eprintln!();
    eprintln!("Use 'git-ai rebase recover --apply <timestamp>' to restore.");
}

fn handle_recover_latest() {
    let repo = match find_repository_in_path(".") {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    let snapshot = match rebase_recovery::load_latest_snapshot(&repo.storage) {
        Some(s) => s,
        None => {
            eprintln!("No recovery snapshots available.");
            std::process::exit(1);
        }
    };

    match rebase_recovery::recover_from_snapshot(&repo, &snapshot) {
        Ok(count) => {
            eprintln!(
                "Restored {} authorship notes to original commit SHAs.",
                count
            );
            eprintln!();
            eprintln!("If you ran 'git rebase --abort', notes are fully restored.");
            eprintln!("Otherwise, re-run the rebase to regenerate notes for new SHAs.");
        }
        Err(e) => {
            eprintln!("Recovery failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_recover_timestamp(timestamp: u64) {
    let repo = match find_repository_in_path(".") {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    let snapshot = match rebase_recovery::load_snapshot_by_timestamp(&repo.storage, timestamp) {
        Some(s) => s,
        None => {
            eprintln!("No snapshot found for timestamp {}.", timestamp);
            eprintln!("Use 'git-ai rebase recover --list' to see available snapshots.");
            std::process::exit(1);
        }
    };

    match rebase_recovery::recover_from_snapshot(&repo, &snapshot) {
        Ok(count) => {
            eprintln!(
                "Restored {} authorship notes to original commit SHAs.",
                count
            );
            eprintln!();
            eprintln!("If you ran 'git rebase --abort', notes are fully restored.");
            eprintln!("Otherwise, re-run the rebase to regenerate notes for new SHAs.");
        }
        Err(e) => {
            eprintln!("Recovery failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn format_timestamp(timestamp: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let dt = UNIX_EPOCH + Duration::from_secs(timestamp);
    let elapsed = std::time::SystemTime::now()
        .duration_since(dt)
        .unwrap_or_default();

    if elapsed.as_secs() < 60 {
        format!("{} (just now)", timestamp)
    } else if elapsed.as_secs() < 3600 {
        format!("{} ({}m ago)", timestamp, elapsed.as_secs() / 60)
    } else if elapsed.as_secs() < 86400 {
        format!("{} ({}h ago)", timestamp, elapsed.as_secs() / 3600)
    } else {
        format!("{} ({}d ago)", timestamp, elapsed.as_secs() / 86400)
    }
}
