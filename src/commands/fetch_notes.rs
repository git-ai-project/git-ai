use std::process::{self, Stdio};

use git_ai::core::git_binary::git_cmd as git_command;

pub fn handle_fetch_notes(args: &[String]) {
    let mut remote: Option<String> = None;
    let mut is_json = false;
    let mut show_help = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                show_help = true;
            }
            "--json" => {
                is_json = true;
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
        println!("usage: git-ai fetch-notes [--remote <name>] [--json]");
        println!();
        println!("Synchronously fetch AI authorship notes from a remote repository.");
        println!();
        println!("Options:");
        println!("  --remote <name>  Remote to fetch from (default: origin)");
        println!("  --json           Output in JSON format");
        return;
    }

    let remote_name = remote.unwrap_or_else(|| "origin".to_string());

    let result = git_command()
        .args(["fetch", &remote_name, "refs/notes/ai:refs/notes/ai"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => {
            if is_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "found",
                        "remote": remote_name,
                        "notes_ref": "refs/notes/ai"
                    })
                );
            } else {
                println!("Fetched authorship notes from '{}' — done", remote_name);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.contains("couldn't find remote ref") {
                if is_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "not_found",
                            "remote": remote_name,
                            "notes_ref": "refs/notes/ai",
                            "message": "no notes found on remote"
                        })
                    );
                } else {
                    println!("no notes found on remote '{}'", remote_name);
                }
            } else {
                if is_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "fetch_failed",
                            "error": stderr.trim(),
                            "remote": remote_name
                        })
                    );
                    process::exit(1);
                } else {
                    eprintln!(
                        "error: failed to fetch notes from '{}': {}",
                        remote_name,
                        stderr.trim()
                    );
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            if is_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "fetch_failed",
                        "error": format!("{}", e),
                        "remote": remote_name
                    })
                );
            } else {
                eprintln!("error: {}", e);
            }
            process::exit(1);
        }
    }
}
