use crate::authorship::ignore::{
    IgnoreMatcher, build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::stats::{CommitStats, stats_from_authorship_log, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint;
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::git::repo_storage::InitialAttributions;
use crate::git::repository::{InternalGitProfile, Repository, exec_git_with_profile};
use crate::git::status::MAX_PATHSPEC_ARGS;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
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
    let ignore_patterns = effective_ignore_patterns(&repo, &[], &[]);
    let ignore_matcher = build_ignore_matcher(&ignore_patterns);

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
        .filter(|file| !should_ignore_file_with_matcher(file, &ignore_matcher))
        .collect();

    let (authorship_log, initial) = working_va.to_authorship_log_and_initial_working_log(
        &repo,
        &head_sha,
        &head_sha,
        Some(&pathspecs),
    )?;

    // Get actual git diff stats between HEAD and working directory (like post_commit does)
    let (total_additions, total_deletions) =
        get_working_dir_diff_stats(&repo, Some(&pathspecs), &ignore_matcher)?;

    // For status (uncommitted changes), the AI attributions are in `initial` (uncommitted),
    // not in authorship_log.attestations (which is for committed changes).
    // Count AI lines from the uncommitted attributions.
    let ai_accepted = count_ai_lines_from_initial(&initial, &ignore_matcher);

    let stats = stats_from_authorship_log(
        Some(&authorship_log),
        total_additions,
        total_deletions,
        ai_accepted,
        &BTreeMap::new(),
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
    ignore_matcher: &IgnoreMatcher,
) -> Result<(u32, u32), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push("HEAD".to_string());

    // Add pathspecs if provided to scope the diff to specific files
    // Only pass as CLI args when under threshold to avoid E2BIG
    let needs_post_filter = if let Some(paths) = pathspecs {
        if paths.is_empty() {
            return Ok((0, 0));
        }
        if paths.len() > MAX_PATHSPEC_ARGS {
            // Disable rename detection so git reports renames as separate
            // delete + add entries with clean filenames. Without this,
            // numstat outputs "old => new" arrow notation in the filename
            // field, which won't match pathspec entries.
            args.push("--no-renames".to_string());
            true
        } else {
            args.push("--".to_string());
            for path in paths {
                args.push(path.clone());
            }
            false
        }
    } else {
        false
    };

    let output = exec_git_with_profile(&args, InternalGitProfile::NumstatParse)?;
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
            // Post-filter by pathspec when we couldn't pass them as CLI args
            if needs_post_filter
                && let Some(paths) = pathspecs
                && !paths.contains(parts[2])
            {
                continue;
            }

            let file_path = parts[2];
            if should_ignore_file_with_matcher(file_path, ignore_matcher) {
                continue;
            }

            // Parse added lines
            if let Ok(added) = parts[0].parse::<u32>() {
                added_lines += added;
            }

            // Parse deleted lines (handle "-" for binary files)
            if parts[1] != "-"
                && let Ok(deleted) = parts[1].parse::<u32>()
            {
                deleted_lines += deleted;
            }
        }
    }

    Ok((added_lines, deleted_lines))
}

/// Count AI-attributed lines from InitialAttributions (uncommitted changes)
fn count_ai_lines_from_initial(
    initial: &InitialAttributions,
    ignore_matcher: &IgnoreMatcher,
) -> u32 {
    let mut ai_lines = 0u32;

    for (file_path, line_attrs) in &initial.files {
        if should_ignore_file_with_matcher(file_path, ignore_matcher) {
            continue;
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::MAX_PATHSPEC_ARGS;
    use crate::git::test_utils::TmpRepo;

    /// Pad a set of real paths with non-existent paths to exceed MAX_PATHSPEC_ARGS.
    fn padded_pathspecs(real_paths: &[&str]) -> HashSet<String> {
        let mut set: HashSet<String> = real_paths.iter().map(|s| s.to_string()).collect();
        let needed = MAX_PATHSPEC_ARGS + 1 - set.len();
        for i in 0..needed {
            set.insert(format!("nonexistent/padding_{:04}.txt", i));
        }
        set
    }

    #[test]
    fn test_get_working_dir_diff_stats_post_filter_equivalence() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("a.txt", "L1\nL2\nL3\n", true).unwrap();
        repo.write_file("b.txt", "hello\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        // Modify both in working dir
        std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "hello\nworld\n").unwrap();

        let gitai_repo = repo.gitai_repo();
        let ignore_matcher = build_ignore_matcher(&[]);

        // Small pathspec (CLI-arg path) - only a.txt
        let small: HashSet<String> = ["a.txt".to_string()].into_iter().collect();
        let (added_small, _deleted_small) =
            get_working_dir_diff_stats(gitai_repo, Some(&small), &ignore_matcher).unwrap();

        // Padded pathspec (post-filter path) - only a.txt + padding
        let large = padded_pathspecs(&["a.txt"]);
        let (added_large, _deleted_large) =
            get_working_dir_diff_stats(gitai_repo, Some(&large), &ignore_matcher).unwrap();

        assert_eq!(added_small, 2, "small pathspec: a.txt adds 2 lines");
        assert_eq!(
            added_small, added_large,
            "post-filter should produce same result as CLI-arg path"
        );
    }

    #[test]
    fn test_get_working_dir_diff_stats_post_filter_exclusion() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("a.txt", "L1\nL2\nL3\n", true).unwrap();
        repo.write_file("b.txt", "hello\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        // Modify both in working dir
        std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "hello\nworld\n").unwrap();

        let gitai_repo = repo.gitai_repo();
        let ignore_matcher = build_ignore_matcher(&[]);

        // Padded pathspec containing only "a.txt"
        let large = padded_pathspecs(&["a.txt"]);
        let (added, _deleted) =
            get_working_dir_diff_stats(gitai_repo, Some(&large), &ignore_matcher).unwrap();

        // a.txt adds 2 lines; b.txt adds 1 line but should be excluded
        assert_eq!(added, 2, "should only count a.txt additions, not b.txt");
    }

    #[test]
    fn test_get_working_dir_diff_stats_none_pathspecs() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("a.txt", "L1\nL2\nL3\n", true).unwrap();
        repo.write_file("b.txt", "hello\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        // Modify both in working dir
        std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "hello\nworld\n").unwrap();

        let gitai_repo = repo.gitai_repo();
        let ignore_matcher = build_ignore_matcher(&[]);

        // None pathspecs = all lines counted
        let (added, _deleted) =
            get_working_dir_diff_stats(gitai_repo, None, &ignore_matcher).unwrap();

        // a.txt adds 2 lines + b.txt adds 1 line = 3 total
        assert_eq!(added, 3, "None pathspecs should count all additions");
    }

    #[test]
    fn test_get_working_dir_diff_stats_empty_pathspecs_returns_zero() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("a.txt", "L1\nL2\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();
        std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\n").unwrap();

        let gitai_repo = repo.gitai_repo();
        let ignore_matcher = build_ignore_matcher(&[]);
        let empty: HashSet<String> = HashSet::new();
        let (added, deleted) =
            get_working_dir_diff_stats(gitai_repo, Some(&empty), &ignore_matcher).unwrap();
        assert_eq!(added, 0);
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_get_working_dir_diff_stats_post_filter_with_rename() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("old_name.txt", "L1\nL2\nL3\n", true)
            .unwrap();
        repo.write_file("other.txt", "hello\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        // Rename old_name.txt -> new_name.txt and add a line.
        // Stage the rename so git diff HEAD sees it.
        std::fs::remove_file(repo.path().join("old_name.txt")).unwrap();
        std::fs::write(repo.path().join("new_name.txt"), "L1\nL2\nL3\nL4\n").unwrap();
        // Also modify other.txt
        std::fs::write(repo.path().join("other.txt"), "hello\nworld\n").unwrap();
        // Stage everything so git diff HEAD picks up the rename + other changes
        repo.git_command(&["add", "-A"]).unwrap();

        let gitai_repo = repo.gitai_repo();
        let ignore_matcher = build_ignore_matcher(&[]);

        // Padded pathspec referencing the NEW name — with --no-renames,
        // git reports this as a delete of old_name.txt + add of new_name.txt,
        // so "new_name.txt" matches cleanly against parts[2].
        let large = padded_pathspecs(&["new_name.txt"]);
        let (added, _deleted) =
            get_working_dir_diff_stats(gitai_repo, Some(&large), &ignore_matcher).unwrap();

        // new_name.txt has 4 lines (all added since it's a new file after --no-renames)
        // other.txt should be excluded
        assert_eq!(
            added, 4,
            "should count new_name.txt additions only, not other.txt"
        );
    }

    #[test]
    fn test_get_working_dir_diff_stats_respects_ignore_patterns() {
        let repo = TmpRepo::new().unwrap();
        repo.write_file("src/lib.rs", "pub fn a() {}\n", true)
            .unwrap();
        repo.write_file("Cargo.lock", "# lock\n", true).unwrap();
        repo.commit_with_message("initial").unwrap();

        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn a() {}\npub fn b() {}\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join("Cargo.lock"),
            "# lock\n# lock-2\n# lock-3\n",
        )
        .unwrap();

        let ignore_matcher = build_ignore_matcher(&["Cargo.lock".to_string()]);
        let (added, _deleted) =
            get_working_dir_diff_stats(repo.gitai_repo(), None, &ignore_matcher).unwrap();
        assert_eq!(added, 1, "Cargo.lock additions should be ignored");
    }

    #[test]
    fn test_count_ai_lines_from_initial_respects_ignore_patterns() {
        let mut initial = InitialAttributions::default();
        initial.prompts.insert(
            "prompt-1".to_string(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: crate::authorship::working_log::AgentId {
                    tool: "cursor".to_string(),
                    id: "session".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                messages: vec![],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                messages_url: None,
            },
        );

        initial.files.insert(
            "src/lib.rs".to_string(),
            vec![crate::authorship::attribution_tracker::LineAttribution {
                start_line: 1,
                end_line: 2,
                author_id: "prompt-1".to_string(),
                overrode: None,
            }],
        );
        initial.files.insert(
            "Cargo.lock".to_string(),
            vec![crate::authorship::attribution_tracker::LineAttribution {
                start_line: 1,
                end_line: 100,
                author_id: "prompt-1".to_string(),
                overrode: None,
            }],
        );

        let ignore_matcher = build_ignore_matcher(&["Cargo.lock".to_string()]);
        let ai_lines = count_ai_lines_from_initial(&initial, &ignore_matcher);
        assert_eq!(ai_lines, 2);
    }
}
