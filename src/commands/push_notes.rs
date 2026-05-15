use std::process::{self, Stdio};

use git_ai::core::git_binary::git_cmd as git_command;

pub fn handle_push_notes(args: &[String]) {
    let mut remote: Option<String> = None;
    let mut force = false;
    let mut show_help = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                show_help = true;
            }
            "--force" | "-f" => {
                force = true;
            }
            "--remote" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --remote requires a value");
                    process::exit(1);
                }
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(args[i].clone());
            }
            s if s.starts_with("--remote=") => {
                let val = s.strip_prefix("--remote=").unwrap();
                if val.is_empty() {
                    eprintln!("error: --remote requires a value");
                    process::exit(1);
                }
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(val.to_string());
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option '{}'", s);
                process::exit(1);
            }
            _ => {
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(arg.to_string());
            }
        }
        i += 1;
    }

    if show_help {
        println!("usage: git-ai push [--remote <name>] [--force]");
        println!();
        println!("Push AI authorship notes to a remote repository.");
        println!();
        println!("Options:");
        println!("  --remote <name>  Remote to push to (default: origin)");
        println!("  --force, -f      Force push (use when notes have diverged)");
        return;
    }

    let remote_name = remote.unwrap_or_else(|| "origin".to_string());

    let mut git_args: Vec<&str> = vec!["push"];
    if force {
        git_args.push("--force");
    }
    git_args.push(&remote_name);
    git_args.push("refs/notes/ai:refs/notes/ai");

    let result = git_command()
        .args(&git_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => {
            println!("Pushed authorship notes to '{}' — done", remote_name);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.contains("non-fast-forward") || stderr.contains("fetch first") {
                eprintln!(
                    "error: notes have diverged on '{}'. Use --force to overwrite, \
                     or run 'git-ai fetch' first to incorporate remote notes.",
                    remote_name
                );
                process::exit(1);
            } else if stderr.contains("does not appear to be a git repository")
                || stderr.contains("Could not read from remote")
            {
                eprintln!(
                    "error: could not connect to remote '{}'. Check that the remote exists \
                     and you have push access.",
                    remote_name
                );
                process::exit(1);
            } else {
                eprintln!(
                    "error: failed to push notes to '{}': {}",
                    remote_name,
                    stderr.trim()
                );
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error: failed to run git push: {}", e);
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_flag() {
        // --help should return without panicking or exiting
        handle_push_notes(&["--help".to_string()]);
    }
}
