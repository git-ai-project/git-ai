use std::process::Stdio;

use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::core::git_binary::git_cmd as git_command;

/// Handle the `show-prompt` command
///
/// Usage: `git-ai show-prompt <prompt_id> [--commit <rev>] [--offset <n>]`
///
/// Returns the prompt object from the authorship note where the given prompt ID is found.
/// By default searches recent commits for the prompt.
pub fn handle_show_prompt(args: &[String]) {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    match find_prompt(&parsed.prompt_id, parsed.commit.as_deref(), parsed.offset) {
        Ok((commit_sha, prompt_record)) => {
            let output = serde_json::json!({
                "commit": commit_sha,
                "prompt_id": parsed.prompt_id,
                "prompt": prompt_record,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
            );
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Search through commits' authorship notes for a prompt with the given ID.
fn find_prompt(
    prompt_id: &str,
    commit: Option<&str>,
    offset: usize,
) -> Result<(String, serde_json::Value), String> {
    let commits = if let Some(rev) = commit {
        // Resolve a single commit
        let output = git_command()
            .args(["rev-parse", "--verify", rev])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run git: {}", e))?;
        if !output.status.success() {
            return Err(format!("could not resolve revision: {}", rev));
        }
        vec![String::from_utf8_lossy(&output.stdout).trim().to_string()]
    } else {
        // Search recent commits (skip `offset` matches)
        let output = git_command()
            .args(["log", "--format=%H", "-100"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run git log: {}", e))?;
        if !output.status.success() {
            return Err("failed to list recent commits".to_string());
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect()
    };

    let mut matches_found = 0usize;

    for sha in &commits {
        let note = match get_note(sha) {
            Some(n) => n,
            None => continue,
        };

        let log = match AuthorshipLog::deserialize_from_string(&note) {
            Ok(l) => l,
            Err(_) => continue,
        };

        if let Some(record) = log.metadata.prompts.get(prompt_id) {
            if matches_found == offset {
                let value = serde_json::to_value(record)
                    .map_err(|e| format!("failed to serialize prompt: {}", e))?;
                return Ok((sha.clone(), value));
            }
            matches_found += 1;
        }
    }

    if matches_found > 0 {
        Err(format!(
            "prompt '{}' found {} time(s) but offset {} is out of range",
            prompt_id, matches_found, offset
        ))
    } else {
        Err(format!(
            "prompt '{}' not found in recent commits",
            prompt_id
        ))
    }
}

/// Get the authorship note content for a commit.
fn get_note(sha: &str) -> Option<String> {
    let output = git_command()
        .args(["notes", "--ref=ai", "show", sha])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;

    if output.status.success() {
        let note = String::from_utf8_lossy(&output.stdout).to_string();
        if note.trim().is_empty() {
            None
        } else {
            Some(note)
        }
    } else {
        None
    }
}

#[derive(Debug)]
struct ParsedArgs {
    prompt_id: String,
    commit: Option<String>,
    offset: usize,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut prompt_id: Option<String> = None;
    let mut commit: Option<String> = None;
    let mut offset: Option<usize> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--commit" {
            if i + 1 >= args.len() {
                return Err("--commit requires a value".to_string());
            }
            i += 1;
            commit = Some(args[i].clone());
        } else if arg == "--offset" {
            if i + 1 >= args.len() {
                return Err("--offset requires a value".to_string());
            }
            i += 1;
            offset = Some(
                args[i]
                    .parse::<usize>()
                    .map_err(|_| "--offset must be a non-negative integer".to_string())?,
            );
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if prompt_id.is_some() {
                return Err("Only one prompt ID can be specified".to_string());
            }
            prompt_id = Some(arg.clone());
        }

        i += 1;
    }

    let prompt_id = prompt_id.ok_or("show-prompt requires a prompt ID")?;

    if commit.is_some() && offset.is_some() {
        return Err("--commit and --offset are mutually exclusive".to_string());
    }

    Ok(ParsedArgs {
        prompt_id,
        commit,
        offset: offset.unwrap_or(0),
    })
}
