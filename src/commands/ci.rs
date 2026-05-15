use std::process::{self, Command, Stdio};

use crate::commands::helpers::git_cmd;

pub fn handle_ci(args: &[String]) {
    if args.len() < 2 || args[0] != "local" || args[1] != "merge" {
        eprintln!("usage: git-ai ci local merge [options]");
        process::exit(1);
    }

    let ci_args = &args[2..];
    let mut merge_commit_sha = String::new();
    let mut base_ref = String::new();
    let mut _head_ref = String::new();
    let mut head_sha = String::new();
    let mut base_sha = String::new();
    let mut skip_fetch_base = false;
    let mut skip_fetch_notes = false;
    let mut skip_fetch = false;
    let mut skip_push = false;

    let mut i = 0;
    while i < ci_args.len() {
        match ci_args[i].as_str() {
            "--merge-commit-sha" => {
                i += 1;
                merge_commit_sha = ci_args.get(i).cloned().unwrap_or_default();
            }
            "--base-ref" => {
                i += 1;
                base_ref = ci_args.get(i).cloned().unwrap_or_default();
            }
            "--head-ref" => {
                i += 1;
                _head_ref = ci_args.get(i).cloned().unwrap_or_default();
            }
            "--head-sha" => {
                i += 1;
                head_sha = ci_args.get(i).cloned().unwrap_or_default();
            }
            "--base-sha" => {
                i += 1;
                base_sha = ci_args.get(i).cloned().unwrap_or_default();
            }
            "--skip-fetch-base" => {
                skip_fetch_base = true;
            }
            "--skip-fetch-notes" => {
                skip_fetch_notes = true;
            }
            "--skip-fetch" => {
                skip_fetch = true;
                skip_fetch_notes = true;
                skip_fetch_base = true;
            }
            "--skip-push" => {
                skip_push = true;
            }
            _ => {}
        }
        i += 1;
    }

    // Step 1: Fetch authorship notes (unless skipped)
    if skip_fetch || skip_fetch_notes {
        println!("Skipping authorship history fetch (--skip-fetch)");
    } else {
        let fetch_result = Command::new("/usr/bin/git")
            .args(["fetch", "origin", "+refs/notes/ai:refs/notes/ai"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match fetch_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if !stderr.contains("couldn't find remote ref") {
                    eprintln!(
                        "Error running local CI: failed to fetch authorship notes: {}",
                        stderr.trim()
                    );
                    process::exit(1);
                }
            }
            Err(e) => {
                eprintln!(
                    "Error running local CI: failed to fetch authorship notes: {}",
                    e
                );
                process::exit(1);
            }
            _ => {}
        }
    }

    // Step 2: Resolve base ref
    if skip_fetch_base {
        println!("Skipping base branch fetch for {}", base_ref);
        // Verify it exists locally
        let resolve_result = git_cmd(&["rev-parse", "--verify", &base_ref]);
        if resolve_result.is_err() {
            let with_origin = format!("origin/{}", base_ref);
            if git_cmd(&["rev-parse", "--verify", &with_origin]).is_err() {
                eprintln!("Failed to resolve base ref '{}' locally", base_ref);
                process::exit(1);
            }
        }
    } else {
        // Try to fetch the base branch from origin
        let fetch_base_result = Command::new("/usr/bin/git")
            .args(["fetch", "origin", &base_ref])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match fetch_base_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                eprintln!(
                    "Failed to fetch base branch '{}' from origin: {}",
                    base_ref,
                    stderr.trim()
                );
                process::exit(1);
            }
            Err(e) => {
                eprintln!(
                    "Failed to fetch base branch '{}' from origin: {}",
                    base_ref, e
                );
                process::exit(1);
            }
            _ => {}
        }
    }

    // Step 3: Determine if merge commit has AI authorship from head branch commits
    let range = format!("{}..{}", base_sha, head_sha);
    let commits_output = git_cmd(&["log", "--format=%H", &range]).unwrap_or_default();
    let head_commits: Vec<&str> = commits_output.lines().filter(|l| !l.is_empty()).collect();

    let mut has_ai_authorship = false;
    for commit in &head_commits {
        if let Ok(note) = git_cmd(&["notes", "--ref=ai", "show", commit])
            && !note.trim().is_empty()
        {
            has_ai_authorship = true;
            break;
        }
    }

    if !has_ai_authorship {
        if skip_fetch {
            println!("Local CI (merge): skipped fast-forward merge — no AI authorship to track");
        } else {
            println!("Local CI (merge): no AI authorship to track");
        }
    } else {
        for commit in &head_commits {
            if let Ok(note) = git_cmd(&["notes", "--ref=ai", "show", commit])
                && !note.trim().is_empty()
            {
                let _ = Command::new("/usr/bin/git")
                    .args([
                        "notes",
                        "--ref=ai",
                        "add",
                        "-f",
                        "-m",
                        &note,
                        &merge_commit_sha,
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                break;
            }
        }
        println!("Local CI (merge): transferred AI authorship to merge commit");
    }

    // Step 4: Push authorship notes (unless skipped)
    if skip_push {
        println!("Skipping authorship push (--skip-push)");
    } else {
        println!("Pushing authorship...");
        let _ = Command::new("/usr/bin/git")
            .args(["push", "origin", "refs/notes/ai:refs/notes/ai"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
