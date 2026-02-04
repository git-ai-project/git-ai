use crate::authorship::stats::{CommitStats, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint;
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::git::repo_storage::InitialAttributions;
use crate::git::repository::Repository;
use serde::Serialize;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct CheckpointInfo {
    time_ago: String,
    additions: u32,
    deletions: u32,
    tool_model: String,
    is_human: bool,
}

#[derive(Serialize)]
struct StatusOutput {
    stats: CommitStats,
    checkpoints: Vec<CheckpointInfo>,
}

pub fn handle_status(args: &[String]) {
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        if args[i].as_str() == "--json" {
            json_output = true;
        }
        i += 1;
    }

    if let Err(e) = run_status(json_output) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run_status(json: bool) -> Result<(), GitAiError> {
    let repo = find_repository(&[])?;

    let default_user_name = match repo.config_get_str("user.name") {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        _ => "unknown".to_string(),
    };

    let _ = checkpoint::run(
        &repo,
        &default_user_name,
        CheckpointKind::Human,
        false,
        false,
        true,
        None,
        false,
    );

    let head = repo.head()?;
    let head_sha = head.target()?;

    let working_log = repo.storage.working_log_for_base_commit(&head_sha);
    let checkpoints = working_log.read_all_checkpoints()?;

    if checkpoints.is_empty() {
        if json {
            let output = StatusOutput {
                stats: CommitStats::default(),
                checkpoints: vec![],
            };
            let json_str = serde_json::to_string(&output)?;
            println!("{}", json_str);
        } else {
            eprintln!(
                "No checkpoints recorded since last commit ({})",
                &head_sha[..7]
            );
            eprintln!();

            eprintln!(
                "If you've made AI edits recently and don't see them here, you might need to install hooks:"
            );
            eprintln!();
            eprintln!("  git-ai install-hooks");
            eprintln!();
        }
        return Ok(());
    }

    let mut checkpoint_infos = Vec::new();

    for checkpoint in checkpoints.iter().rev() {
        let (additions, deletions) = (
            checkpoint.line_stats.additions,
            checkpoint.line_stats.deletions,
        );

        let tool_model = checkpoint
            .agent_id
            .as_ref()
            .map(|a| format!("{} {}", capitalize(&a.tool), &a.model))
            .unwrap_or_else(|| default_user_name.clone());

        let is_human = checkpoint.kind == CheckpointKind::Human;
        checkpoint_infos.push(CheckpointInfo {
            time_ago: format_time_ago(checkpoint.timestamp),
            additions,
            deletions,
            tool_model,
            is_human,
        });
    }

    let working_va = VirtualAttributions::from_just_working_log(
        repo.clone(),
        head_sha.clone(),
        Some(default_user_name.clone()),
    )?;

    let pathspecs: HashSet<String> = checkpoints
        .iter()
        .flat_map(|cp| cp.entries.iter().map(|e| e.file.clone()))
        .collect();

    let (authorship_log, initial) = working_va.to_authorship_log_and_initial_working_log(
        &repo,
        &head_sha,
        &head_sha,
        Some(&pathspecs),
    )?;

    // Get actual git diff stats between HEAD and working directory (like post_commit does)
    let (total_additions, total_deletions) = get_working_dir_diff_stats(&repo, Some(&pathspecs))?;

    // For status (uncommitted changes), the AI attributions are in `initial` (uncommitted),
    // not in authorship_log.attestations (which is for committed changes).
    // Count AI lines from the uncommitted attributions.
    let ai_accepted = count_ai_lines_from_initial(&initial);

    let stats = stats_from_authorship_log_with_override(
        Some(&authorship_log),
        total_additions,
        total_deletions,
        ai_accepted,
    );

    if json {
        let output = StatusOutput {
            stats,
            checkpoints: checkpoint_infos,
        };
        let json_str = serde_json::to_string(&output)?;
        println!("{}", json_str);
        return Ok(());
    }

    write_stats_to_terminal(&stats, true);

    println!();
    for cp in &checkpoint_infos {
        let add_str = if cp.additions > 0 {
            format!("+{}", cp.additions)
        } else {
            "0".to_string()
        };
        let del_str = if cp.deletions > 0 {
            format!("-{}", cp.deletions)
        } else {
            "0".to_string()
        };

        let line = format!(
            "{:<14} {:>5}  {:>5}  {}",
            cp.time_ago, add_str, del_str, cp.tool_model
        );

        if cp.is_human {
            println!("\x1b[90m{}\x1b[0m", line);
        } else {
            println!("{}", line);
        }
    }

    Ok(())
}

fn format_time_ago(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let diff = now.saturating_sub(timestamp);

    if diff < 60 {
        format!("{} secs ago", diff)
    } else if diff < 3600 {
        format!("{} mins ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Get git diff statistics between HEAD and the working directory
/// This mirrors the logic in stats.rs get_git_diff_stats but for uncommitted changes
fn get_working_dir_diff_stats(
    repo: &Repository,
    pathspecs: Option<&HashSet<String>>,
) -> Result<(u32, u32), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push("HEAD".to_string());

    // Add pathspecs if provided to scope the diff to specific files
    if let Some(paths) = pathspecs
        && !paths.is_empty() {
            args.push("--".to_string());
            for path in paths {
                args.push(path.clone());
            }
        }

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    let mut added_lines = 0u32;
    let mut deleted_lines = 0u32;

    // Parse numstat output
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Parse numstat format: "added\tdeleted\tfilename"
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            // Parse added lines
            if let Ok(added) = parts[0].parse::<u32>() {
                added_lines += added;
            }

            // Parse deleted lines (handle "-" for binary files)
            if parts[1] != "-"
                && let Ok(deleted) = parts[1].parse::<u32>() {
                    deleted_lines += deleted;
                }
        }
    }

    Ok((added_lines, deleted_lines))
}

/// Count AI-attributed lines from InitialAttributions (uncommitted changes)
fn count_ai_lines_from_initial(initial: &InitialAttributions) -> u32 {
    let mut ai_lines = 0u32;

    for line_attrs in initial.files.values() {
        for line_attr in line_attrs {
            // Check if this author_id corresponds to an AI prompt (not human)
            if initial.prompts.contains_key(&line_attr.author_id) {
                // Count lines in this attribution
                let lines_count = line_attr.end_line - line_attr.start_line + 1;
                ai_lines += lines_count;
            }
        }
    }

    ai_lines
}

/// Create CommitStats for uncommitted changes with a known ai_accepted count
/// This is used by status where we calculate ai_accepted from InitialAttributions
fn stats_from_authorship_log_with_override(
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    git_diff_added_lines: u32,
    git_diff_deleted_lines: u32,
    ai_accepted_override: u32,
) -> CommitStats {
    let mut stats = CommitStats {
        git_diff_added_lines,
        git_diff_deleted_lines,
        ai_accepted: ai_accepted_override,
        ai_additions: ai_accepted_override, // For uncommitted, ai_additions = ai_accepted (no mixed tracking)
        human_additions: git_diff_added_lines.saturating_sub(ai_accepted_override),
        ..Default::default()
    };

    // Still extract total_ai_additions/deletions and time_waiting from prompts if available
    if let Some(log) = authorship_log {
        for prompt_record in log.metadata.prompts.values() {
            stats.total_ai_additions += prompt_record.total_additions;
            stats.total_ai_deletions += prompt_record.total_deletions;

            // Calculate time waiting for AI from transcript
            let transcript = crate::authorship::transcript::AiTranscript {
                messages: prompt_record.messages.clone(),
            };
            stats.time_waiting_for_ai += calculate_waiting_time(&transcript);
        }
    }

    stats
}

/// Calculate time waiting for AI from transcript messages
fn calculate_waiting_time(transcript: &crate::authorship::transcript::AiTranscript) -> u64 {
    use crate::authorship::transcript::Message;

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
            }
            | Message::Thinking {
                timestamp: Some(ai_ts),
                ..
            }
            | Message::Plan {
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
