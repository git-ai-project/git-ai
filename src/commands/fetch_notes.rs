use crate::config::{NotesBackendKind, RepositoryFilterRejection};
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::git::sync_authorship::{NotesExistence, fetch_authorship_notes};
use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Serialize)]
struct FetchNotesJsonOutput {
    remote: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn handle_fetch_notes(args: &[String]) {
    let mut remote: Option<String> = None;
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_output = true,
            "--remote" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --remote requires a value");
                    std::process::exit(1);
                }
                if remote.is_some() {
                    eprintln!("Error: remote specified more than once");
                    std::process::exit(1);
                }
                remote = Some(args[i].clone());
            }
            "--help" | "-h" => {
                print_fetch_notes_help();
                return;
            }
            other if other.starts_with('-') => {
                eprintln!("Error: unknown option '{}'", other);
                eprintln!("Run 'git ai fetch-notes --help' for usage");
                std::process::exit(1);
            }
            // Positional argument treated as remote name
            _ => {
                if remote.is_none() {
                    remote = Some(args[i].clone());
                } else {
                    eprintln!("Error: unexpected argument '{}'", args[i]);
                    std::process::exit(1);
                }
            }
        }
        i += 1;
    }

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(repo) => repo,
        Err(e) => {
            if json_output {
                print_json_error("not_a_repository", &e.to_string(), remote.as_deref());
            } else {
                eprintln!("Error: not a git repository ({})", e);
            }
            std::process::exit(1);
        }
    };

    // Resolve remote name: explicit arg > upstream tracking > default (origin)
    let remote_name = match remote {
        Some(r) => r,
        None => match resolve_default_remote(&repo) {
            Ok(r) => r,
            Err(e) => {
                if json_output {
                    print_json_error("no_remote", &e.to_string(), None);
                } else {
                    eprintln!("Error: {}", e);
                    eprintln!(
                        "Hint: specify a remote with 'git ai fetch-notes <remote>' or 'git ai fetch-notes --remote <name>'"
                    );
                }
                std::process::exit(1);
            }
        },
    };

    let config = crate::config::Config::get();
    if let Some(rejection) = config.notes_sync_rejection(&repo, &remote_name) {
        if json_output {
            print_skipped_json(remote_name);
        } else {
            print_filter_skip_hint(&remote_name, &rejection, config.notes_backend_kind());
        }
        return;
    }

    if !json_output {
        eprint!("Fetching authorship notes from '{}'...", remote_name);
    }

    let start = Instant::now();

    // When the HTTP notes backend is enabled, warm the local notes-db cache
    // from the HTTP backend instead of fetching refs/notes/ai.
    if crate::config::Config::get().notes_backend_kind() == NotesBackendKind::Http {
        match crate::git::notes_api::warm_cache_for_remote(&repo, &remote_name) {
            Ok(()) => {
                let elapsed = start.elapsed();
                if json_output {
                    let output = FetchNotesJsonOutput {
                        remote: remote_name,
                        status: "warmed".to_string(),
                        error: None,
                    };
                    println!(
                        "{}",
                        serde_json::to_string(&output).expect("failed to serialize JSON")
                    );
                } else {
                    eprintln!(" cache warmed ({:.2}s).", elapsed.as_secs_f64());
                }
                return;
            }
            Err(e) => {
                if json_output {
                    print_json_error("warm_failed", &e.to_string(), Some(&remote_name));
                } else {
                    eprintln!(" failed.");
                    eprintln!("Error: {}", e);
                }
                std::process::exit(1);
            }
        }
    }

    match fetch_authorship_notes(&repo, &remote_name) {
        Ok(notes_existence) => {
            let elapsed = start.elapsed();
            if json_output {
                let status = match notes_existence {
                    NotesExistence::Found => "found".to_string(),
                    NotesExistence::NotFound => "not_found".to_string(),
                    NotesExistence::Skipped => "skipped".to_string(),
                };
                let output = FetchNotesJsonOutput {
                    remote: remote_name,
                    status,
                    error: None,
                };
                println!(
                    "{}",
                    serde_json::to_string(&output).expect("failed to serialize JSON")
                );
            } else {
                match notes_existence {
                    NotesExistence::Found => {
                        eprintln!(" done ({:.2}s).", elapsed.as_secs_f64());
                    }
                    NotesExistence::NotFound => {
                        eprintln!(" no notes found on remote ({:.2}s).", elapsed.as_secs_f64());
                    }
                    NotesExistence::Skipped => {
                        eprintln!(" skipped, rejected by repository filters.");
                    }
                }
            }
        }
        Err(e) => {
            if json_output {
                print_json_error("fetch_failed", &e.to_string(), Some(&remote_name));
            } else {
                eprintln!(" failed.");
                eprintln!("Error: {}", e);
            }
            std::process::exit(1);
        }
    }
}

fn print_skipped_json(remote: String) {
    let output = FetchNotesJsonOutput {
        remote,
        status: "skipped".to_string(),
        error: None,
    };
    println!(
        "{}",
        serde_json::to_string(&output).expect("failed to serialize JSON")
    );
}

/// Explain a fetch the repository filters rejected, and the two ways around
/// it: change the filters, or fetch with plain git outside git-ai.
fn print_filter_skip_hint(
    remote: &str,
    rejection: &RepositoryFilterRejection,
    backend: NotesBackendKind,
) {
    let reason = rejection.reason();
    let change_filters = rejection.how_to_change_filters();
    eprintln!(
        "Skipped fetching authorship notes from '{}': the repository filters reject it ({}).",
        remote, reason
    );
    if backend == NotesBackendKind::Http {
        eprintln!("To fetch them anyway, {}.", change_filters);
        return;
    }
    eprintln!("To fetch them anyway, either:");
    eprintln!("  - {}, or", change_filters);
    eprintln!(
        "  - fetch them with plain git, outside git-ai: git fetch {} refs/notes/ai:refs/notes/ai",
        remote
    );
}

fn resolve_default_remote(repo: &crate::git::repository::Repository) -> Result<String, GitAiError> {
    // Try upstream tracking remote first, then default remote
    if let Ok(Some(upstream)) = repo.upstream_remote() {
        return Ok(upstream);
    }
    if let Ok(Some(default)) = repo.get_default_remote() {
        return Ok(default);
    }
    Err(GitAiError::Generic(
        "could not determine a remote. No upstream tracking branch configured and no default remote found".to_string(),
    ))
}

fn print_json_error(status: &str, message: &str, remote: Option<&str>) {
    let output = FetchNotesJsonOutput {
        remote: remote.unwrap_or_default().to_string(),
        status: status.to_string(),
        error: Some(message.to_string()),
    };
    println!(
        "{}",
        serde_json::to_string(&output).expect("failed to serialize JSON")
    );
}

fn print_fetch_notes_help() {
    eprintln!("git ai fetch-notes - Synchronously fetch AI authorship notes from a remote");
    eprintln!();
    eprintln!("Usage: git ai fetch-notes [options] [<remote>]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  <remote>           Remote to fetch from (default: upstream or origin)");
    eprintln!("  --remote <name>    Explicit remote name");
    eprintln!("  --json             Output result as JSON");
    eprintln!("  -h, --help         Show this help message");
    eprintln!();
    eprintln!("Notes are not fetched from a remote that allow_repositories /");
    eprintln!("exclude_repositories reject; the command says which rule matched.");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  git ai fetch-notes              Fetch from default remote");
    eprintln!("  git ai fetch-notes upstream      Fetch from 'upstream' remote");
    eprintln!("  git ai fetch-notes --json        Fetch and output JSON result");
}
