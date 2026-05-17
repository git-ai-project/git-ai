use std::process::Stdio;

use git_ai::core::git_binary::git_cmd as git_command;

const NO_AUTHORSHIP_DATA_MESSAGE: &str = "No authorship data found for this revision";

pub fn handle_show(args: &[String]) {
    if args.is_empty() {
        eprintln!("Error: show requires a revision or range");
        std::process::exit(1);
    }

    if args.len() > 1 {
        eprintln!("Error: show accepts exactly one revision or range");
        std::process::exit(1);
    }

    let spec = &args[0];

    let commits = match resolve_commits(spec) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    if commits.is_empty() {
        println!("{}", NO_AUTHORSHIP_DATA_MESSAGE);
        return;
    }

    let multiple = commits.len() > 1;
    for (i, sha) in commits.iter().enumerate() {
        if multiple && i > 0 {
            println!();
        }
        if multiple {
            println!("{}", sha);
        }
        match show_note_for_commit(sha) {
            Some(note) => println!("{}", note),
            None => println!("{}", NO_AUTHORSHIP_DATA_MESSAGE),
        }
    }
}

/// Show the authorship note for a single commit, or None if no note exists.
fn show_note_for_commit(sha: &str) -> Option<String> {
    let output = git_command()
        .args(["notes", "--ref=ai", "show", sha])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;

    if output.status.success() {
        let note = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        if note.is_empty() { None } else { Some(note) }
    } else {
        None
    }
}

/// Resolve a revision spec into a list of commit SHAs.
/// Handles both single revisions and ranges (start..end).
fn resolve_commits(spec: &str) -> Result<Vec<String>, String> {
    if let Some((start, end)) = spec.split_once("..") {
        if start.is_empty() || end.is_empty() {
            return Err("Invalid commit range format. Expected <start>..<end>".to_string());
        }
        // Use git log to enumerate commits in the range
        let output = git_command()
            .args(["log", "--format=%H", &format!("{}..{}", start, end)])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run git log: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("git log failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut commits: Vec<String> = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();

        // If range produced nothing, try resolving the end commit alone
        if commits.is_empty()
            && let Some(sha) = rev_parse(end)
        {
            commits.push(sha);
        }

        Ok(commits)
    } else {
        match rev_parse(spec) {
            Some(sha) => Ok(vec![sha]),
            None => Err(format!("could not resolve revision: {}", spec)),
        }
    }
}

/// Resolve a single revision to its full SHA.
fn rev_parse(rev: &str) -> Option<String> {
    let output = git_command()
        .args(["rev-parse", "--verify", rev])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() { None } else { Some(sha) }
    } else {
        None
    }
}
