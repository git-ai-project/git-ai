use crate::ci::ci_context::{CiContext, CiEvent, CiRunResult};
use crate::ci::github::{get_github_ci_context, install_github_ci_workflow};
use crate::ci::gitlab::{get_gitlab_ci_context, print_gitlab_ci_yaml};
use crate::git::repository::find_repository_in_path;
use crate::utils::debug_log;

/// Print a human-readable message for a CiRunResult
fn print_ci_result(result: &CiRunResult, prefix: &str) {
    match result {
        CiRunResult::AuthorshipRewritten { .. } => {
            println!("{}: authorship rewritten successfully", prefix);
        }
        CiRunResult::AlreadyExists { .. } => {
            println!("{}: authorship already exists", prefix);
        }
        CiRunResult::SkippedSimpleMerge => {
            println!("{}: skipped simple merge (authorship preserved)", prefix);
        }
        CiRunResult::SkippedFastForward => {
            println!("{}: skipped fast-forward merge", prefix);
        }
        CiRunResult::NoAuthorshipAvailable => {
            println!(
                "{}: no AI authorship to track (pre-git-ai commits or human-only code)",
                prefix
            );
        }
    }
}

pub fn handle_ci(args: &[String]) {
    if args.is_empty() {
        print_ci_help_and_exit();
    }

    match args[0].as_str() {
        "github" => {
            handle_ci_github(&args[1..]);
        }
        "gitlab" => {
            handle_ci_gitlab(&args[1..]);
        }
        "local" => {
            handle_ci_local(&args[1..]);
        }
        _ => {
            eprintln!("Unknown ci subcommand: {}", args[0]);
            print_ci_help_and_exit();
        }
    }
}

fn handle_ci_github(args: &[String]) {
    if args.is_empty() {
        print_ci_github_help_and_exit();
    }
    // Subcommands: install | (default: run in CI context)
    match args[0].as_str() {
        "run" => {
            let no_cleanup = args[1..].iter().any(|a| a == "--no-cleanup");
            let ci_context = get_github_ci_context();
            match ci_context {
                Ok(Some(ci_context)) => {
                    debug_log(&format!("GitHub CI context: {:?}", ci_context));
                    match ci_context.run() {
                        Ok(result) => {
                            debug_log(&format!("GitHub CI result: {:?}", result));
                            print_ci_result(&result, "GitHub CI");
                        }
                        Err(e) => {
                            eprintln!("Error running GitHub CI context: {}", e);
                            std::process::exit(1);
                        }
                    }
                    if !no_cleanup {
                        if let Err(e) = ci_context.teardown() {
                            eprintln!("Error tearing down GitHub CI context: {}", e);
                            std::process::exit(1);
                        }
                        debug_log("GitHub CI context teared down");
                    } else {
                        debug_log("Skipping teardown (--no-cleanup)");
                    }
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Failed to get GitHub CI context: {}", e);
                    std::process::exit(1);
                }
                Ok(None) => {
                    eprintln!("No GitHub CI context found");
                    std::process::exit(1);
                }
            }
        }
        "install" => match install_github_ci_workflow() {
            Ok(path) => {
                println!("Installed GitHub Actions workflow to {}", path.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Failed to install GitHub CI workflow: {}", e);
                std::process::exit(1);
            }
        },
        other => {
            eprintln!("Unknown ci github subcommand: {}", other);
            print_ci_help_and_exit();
        }
    }
}

fn handle_ci_gitlab(args: &[String]) {
    if args.is_empty() {
        print_ci_gitlab_help_and_exit();
    }
    // Subcommands: install | run
    match args[0].as_str() {
        "run" => {
            let no_cleanup = args[1..].iter().any(|a| a == "--no-cleanup");
            let ci_context = get_gitlab_ci_context();
            match ci_context {
                Ok(Some(ci_context)) => {
                    debug_log(&format!("GitLab CI context: {:?}", ci_context));
                    match ci_context.run() {
                        Ok(result) => {
                            debug_log(&format!("GitLab CI result: {:?}", result));
                            print_ci_result(&result, "GitLab CI");
                        }
                        Err(e) => {
                            eprintln!("Error running GitLab CI context: {}", e);
                            std::process::exit(1);
                        }
                    }
                    if !no_cleanup {
                        if let Err(e) = ci_context.teardown() {
                            eprintln!("Error tearing down GitLab CI context: {}", e);
                            std::process::exit(1);
                        }
                        debug_log("GitLab CI context teared down");
                    } else {
                        debug_log("Skipping teardown (--no-cleanup)");
                    }
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Failed to get GitLab CI context: {}", e);
                    std::process::exit(1);
                }
                Ok(None) => {
                    // No matching MR found - this is not an error, just nothing to do
                    std::process::exit(0);
                }
            }
        }
        "install" => {
            print_gitlab_ci_yaml();
            std::process::exit(0);
        }
        other => {
            eprintln!("Unknown ci gitlab subcommand: {}", other);
            print_ci_help_and_exit();
        }
    }
}

fn handle_ci_local(args: &[String]) {
    if args.is_empty() {
        print_ci_local_help_and_exit();
    }

    let event = args[0].as_str();
    let event_args: &[String] = &args[1..];

    // Simple flag parser over remaining args: --key value
    let flag = |name: &str| -> Option<String> {
        let mut i = 0usize;
        while i < event_args.len() {
            if event_args[i] == name {
                if i + 1 < event_args.len() {
                    return Some(event_args[i + 1].clone());
                } else {
                    eprintln!("Missing value for flag {}", name);
                    std::process::exit(1);
                }
            }
            i += 1;
        }
        None
    };

    // Open current repo
    let repo = match find_repository_in_path(".") {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to open repository in current directory: {}", e);
            std::process::exit(1);
        }
    };

    match event {
        "merge" => {
            // Required inputs for merge
            let merge_commit_sha = match flag("--merge-commit-sha") {
                Some(v) => v,
                None => {
                    eprintln!("--merge-commit-sha is required");
                    std::process::exit(1);
                }
            };

            let base_ref = match flag("--base-ref") {
                Some(v) => v,
                None => {
                    eprintln!("--base-ref is required (e.g., main)");
                    std::process::exit(1);
                }
            };

            // All flags required for merge
            let head_ref = match flag("--head-ref") {
                Some(v) => v,
                None => {
                    eprintln!("--head-ref is required");
                    std::process::exit(1);
                }
            };

            let head_sha = match flag("--head-sha") {
                Some(v) => v,
                None => {
                    eprintln!("--head-sha is required");
                    std::process::exit(1);
                }
            };

            let base_sha = match flag("--base-sha") {
                Some(v) => v,
                None => {
                    eprintln!("--base-sha is required");
                    std::process::exit(1);
                }
            };

            let ctx = CiContext {
                repo,
                event: CiEvent::Merge {
                    merge_commit_sha,
                    head_ref,
                    head_sha,
                    base_ref,
                    base_sha,
                },
                // Not used for local runs; teardown not invoked
                temp_dir: std::path::PathBuf::from("."),
            };

            debug_log(&format!("Local CI context: {:?}", ctx));
            match ctx.run() {
                Ok(result) => {
                    debug_log(&format!("Local CI result: {:?}", result));
                    print_ci_result(&result, "Local CI (merge)");
                }
                Err(e) => {
                    eprintln!("Error running local CI: {}", e);
                    std::process::exit(1);
                }
            }
            std::process::exit(0);
        }
        other => {
            eprintln!("Unknown local CI event: {}", other);
            print_ci_local_help_and_exit();
        }
    }
}

fn print_ci_help_and_exit() -> ! {
    eprintln!("git-ai ci - Continuous integration utilities");
    eprintln!();
    eprintln!("Usage: git-ai ci <subcommand> [args...]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  github           GitHub CI");
    eprintln!("    run [--no-cleanup]  Run GitHub CI in current repo");
    eprintln!("    install        Install/update workflow in current repo");
    eprintln!("  gitlab           GitLab CI");
    eprintln!("    run [--no-cleanup]  Run GitLab CI in current repo");
    eprintln!("    install        Print YAML snippet to add to .gitlab-ci.yml");
    eprintln!("  local            Run CI locally by event name and flags");
    eprintln!("                   Usage: git-ai ci local <event> [flags]");
    eprintln!("                   Events:");
    eprintln!(
        "                     merge  --merge-commit-sha <sha> --base-ref <ref> --head-ref <ref> --head-sha <sha> --base-sha <sha>"
    );
    std::process::exit(1);
}

fn print_ci_local_help_and_exit() -> ! {
    eprintln!("git-ai ci local - Run CI locally by event name and flags");
    eprintln!();
    eprintln!("Usage: git-ai ci local <event> [flags]");
    eprintln!();
    eprintln!("Events:");
    eprintln!(
        "  merge  --merge-commit-sha <sha> --base-ref <ref> --head-ref <ref> --head-sha <sha> --base-sha <sha>"
    );
    std::process::exit(1);
}

fn print_ci_github_help_and_exit() -> ! {
    eprintln!("git-ai ci github - GitHub CI utilities");
    eprintln!();
    eprintln!("Usage: git-ai ci github <subcommand> [args...]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  run [--no-cleanup]   Run GitHub CI in current repo");
    eprintln!("                       --no-cleanup  Skip teardown after run");
    eprintln!("  install              Install/update workflow in current repo");
    std::process::exit(1);
}

fn print_ci_gitlab_help_and_exit() -> ! {
    eprintln!("git-ai ci gitlab - GitLab CI utilities");
    eprintln!();
    eprintln!("Usage: git-ai ci gitlab <subcommand> [args...]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  run [--no-cleanup]   Run GitLab CI in current repo");
    eprintln!("                       --no-cleanup  Skip teardown after run");
    eprintln!("  install              Print YAML snippet to add to .gitlab-ci.yml");
    std::process::exit(1);
}
