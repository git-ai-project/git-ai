use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::range_authorship;
use crate::authorship::stats::stats_command;
use crate::authorship::working_log::CheckpointKind;
use crate::commands;
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::commands::checkpoint_agent::transcript_readers;
use crate::config;
use crate::daemon::{
    CapturedCheckpointRunRequest, CheckpointRunRequest, ControlRequest, LiveCheckpointRunRequest,
    send_control_request,
};
use crate::git::find_repository;
use crate::git::find_repository_in_path;
use crate::git::repository::{CommitRange, Repository, group_files_by_repository};
use crate::git::sync_authorship::{NotesExistence, fetch_authorship_notes, push_authorship_notes};
use crate::observability::performance_targets::log_performance_for_checkpoint;
use crate::observability::{self, log_message};
use crate::utils::is_interactive_terminal;
use serde::{Deserialize, Serialize};
use std::env;
use std::io::IsTerminal;
use std::io::Read;
use std::time::Duration;

pub fn handle_git_ai(args: &[String]) {
    if args.is_empty() {
        print_help();
        return;
    }

    // Initialize the global telemetry handle so that observability and CAS
    // events are routed over the control socket instead of being written to
    // per-PID log files.
    //
    // Skip for commands that must work without a running background service
    // (help, version, config, d management, debug, upgrade) so users can
    // always diagnose and recover from a broken state.
    let needs_daemon = !matches!(
        args[0].as_str(),
        "help"
            | "--help"
            | "-h"
            | "version"
            | "--version"
            | "-v"
            | "config"
            | "bg"
            | "d"
            | "daemon"
            | "debug"
            | "upgrade"
            | "install-hooks"
            | "install"
            | "uninstall-hooks"
    );
    if needs_daemon {
        use crate::daemon::telemetry_handle::{
            DaemonTelemetryInitResult, init_daemon_telemetry_handle,
        };
        match init_daemon_telemetry_handle() {
            DaemonTelemetryInitResult::Connected | DaemonTelemetryInitResult::Skipped => {}
            DaemonTelemetryInitResult::Failed(err) => {
                // Hard error for git-ai commands: the background service must be reachable.
                eprintln!(
                    "error: failed to connect to git-ai background service: {}",
                    err
                );
                std::process::exit(1);
            }
        }
    }

    // Start DB warmup early for commands that need database access
    match args[0].as_str() {
        "show-prompt" | "flush-cas" => {
            InternalDatabase::warmup();
        }
        _ => {}
    }

    match args[0].as_str() {
        "help" | "--help" | "-h" => {
            print_help();
        }
        "version" | "--version" | "-v" => {
            if cfg!(debug_assertions) {
                println!("{} (debug)", env!("CARGO_PKG_VERSION"));
            } else {
                println!(env!("CARGO_PKG_VERSION"));
            }
            std::process::exit(0);
        }
        "config" => {
            commands::config::handle_config(&args[1..]);
            if is_interactive_terminal() {
                log_message("config", "info", None)
            }
        }
        "debug" => {
            commands::debug::handle_debug(&args[1..]);
        }
        "bg" | "d" | "daemon" => {
            commands::daemon::handle_daemon(&args[1..]);
        }
        "stats" => {
            if is_interactive_terminal() {
                log_message("stats", "info", None)
            }
            handle_stats(&args[1..]);
        }
        "status" => {
            commands::status::handle_status(&args[1..]);
        }
        "show" => {
            commands::show::handle_show(&args[1..]);
        }
        "checkpoint" => {
            handle_checkpoint(&args[1..]);
        }
        "log" => {
            let status = commands::log::handle_log(&args[1..]);
            if is_interactive_terminal() {
                log_message("log", "info", None)
            }
            exit_with_log_status(status);
        }
        "blame" => {
            handle_ai_blame(&args[1..]);
            if is_interactive_terminal() {
                log_message("blame", "info", None)
            }
        }
        "diff" => {
            handle_ai_diff(&args[1..]);
            if is_interactive_terminal() {
                log_message("diff", "info", None)
            }
        }
        "git-path" => {
            let config = config::Config::get();
            println!("{}", config.git_cmd());
            std::process::exit(0);
        }
        "install-hooks" | "install" => match commands::install_hooks::run(&args[1..]) {
            Ok(statuses) => {
                if let Ok(statuses_value) = serde_json::to_value(&statuses) {
                    log_message("install-hooks", "info", Some(statuses_value));
                }
            }
            Err(e) => {
                eprintln!("Install hooks failed: {}", e);
                std::process::exit(1);
            }
        },
        "uninstall-hooks" => match commands::install_hooks::run_uninstall(&args[1..]) {
            Ok(statuses) => {
                if let Ok(statuses_value) = serde_json::to_value(&statuses) {
                    log_message("uninstall-hooks", "info", Some(statuses_value));
                }
            }
            Err(e) => {
                eprintln!("Uninstall hooks failed: {}", e);
                std::process::exit(1);
            }
        },
        "git-hooks" => {
            handle_git_hooks(&args[1..]);
        }
        "squash-authorship" => {
            commands::squash_authorship::handle_squash_authorship(&args[1..]);
        }
        "ci" => {
            commands::ci_handlers::handle_ci(&args[1..]);
        }
        "upgrade" => {
            commands::upgrade::run_with_args(&args[1..]);
        }
        "flush-cas" => {
            commands::flush_cas::handle_flush_cas(&args[1..]);
        }
        "flush-metrics-db" => {
            commands::flush_metrics_db::handle_flush_metrics_db(&args[1..]);
        }
        "login" => {
            commands::login::handle_login(&args[1..]);
        }
        "logout" => {
            commands::logout::handle_logout(&args[1..]);
        }
        "whoami" => {
            commands::whoami::handle_whoami(&args[1..]);
        }
        "exchange-nonce" => {
            commands::exchange_nonce::handle_exchange_nonce(&args[1..]);
        }
        "dash" | "dashboard" => {
            commands::personal_dashboard::handle_personal_dashboard(&args[1..]);
        }
        "show-prompt" => {
            commands::show_prompt::handle_show_prompt(&args[1..]);
        }
        "fetch-notes" => {
            commands::fetch_notes::handle_fetch_notes(&args[1..]);
        }
        "effective-ignore-patterns" => {
            handle_effective_ignore_patterns_internal(&args[1..]);
        }
        "blame-analysis" => {
            handle_blame_analysis_internal(&args[1..]);
        }
        "fetch-authorship-notes" | "fetch_authorship_notes" => {
            handle_fetch_authorship_notes_internal(&args[1..]);
        }
        "push-authorship-notes" | "push_authorship_notes" => {
            handle_push_authorship_notes_internal(&args[1..]);
        }
        #[cfg(debug_assertions)]
        "show-transcript" => {
            handle_show_transcript(&args[1..]);
        }
        _ => {
            println!("Unknown git-ai command: {}", args[0]);
            std::process::exit(1);
        }
    }
}

fn print_help() {
    eprintln!("git-ai - git proxy with AI authorship tracking");
    eprintln!();
    eprintln!("Usage: git-ai <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  checkpoint         Checkpoint working changes and attribute author");
    eprintln!(
        "    Presets: claude, codex, continue-cli, cursor, gemini, github-copilot, amp, windsurf, opencode, pi, ai_tab, firebender, human, mock_ai, mock_known_human, known_human"
    );
    eprintln!(
        "    --hook-input <json|stdin>   JSON payload required by presets, or 'stdin' to read from stdin"
    );
    eprintln!("    human [pathspecs...]             Untracked/legacy human checkpoint");
    eprintln!("    mock_ai [pathspecs...]           Test preset accepting optional file pathspecs");
    eprintln!("    mock_known_human [pathspecs...]  Test preset for KnownHuman checkpoints");
    eprintln!("  log [args...]      Show commit log with AI authorship notes");
    eprintln!(
        "                        Proxies git log --notes=ai with all standard git log options"
    );
    eprintln!("  blame <file>       Git blame with AI authorship overlay");
    eprintln!("  diff <commit|range>  Show diff with AI authorship annotations");
    eprintln!("    <commit>              Diff from commit's parent to commit");
    eprintln!("    <commit1>..<commit2>  Diff between two commits");
    eprintln!("    --json                 Output in JSON format");
    eprintln!(
        "    --include-stats        Include commit_stats in JSON output (single commit only)"
    );
    eprintln!(
        "    --all-prompts          Include all prompts from commit note in JSON output (single commit only)"
    );
    eprintln!("  stats [commit]     Show AI authorship statistics for a commit");
    eprintln!("    --json                 Output in JSON format");
    eprintln!("  status             Show uncommitted AI authorship status (debug)");
    eprintln!("    --json                 Output in JSON format");
    eprintln!("  show <rev|range>   Display authorship logs for a revision or range");
    eprintln!("  show-prompt <id>   Display a prompt record by its ID");
    eprintln!("    --commit <rev>        Look in a specific commit only");
    eprintln!(
        "    --offset <n>          Skip n occurrences (0 = most recent, mutually exclusive with --commit)"
    );
    eprintln!("  config             View and manage git-ai configuration");
    eprintln!("                        Show all config as formatted JSON");
    eprintln!("    <key>                 Show specific config value (supports dot notation)");
    eprintln!("    set <key> <value>     Set a config value (arrays: single value = [value])");
    eprintln!("    --add <key> <value>   Add to array or upsert into object");
    eprintln!("    unset <key>           Remove config value (reverts to default)");
    eprintln!("  debug              Print support/debug diagnostics");
    eprintln!("  bg                 Run and control git-ai background service");
    eprintln!("  install-hooks      Install git hooks for AI authorship tracking");
    eprintln!("  uninstall-hooks    Remove git-ai hooks from all detected tools");
    eprintln!("  ci                 Continuous integration utilities");
    eprintln!("    github                 GitHub CI helpers");
    eprintln!("  squash-authorship  Generate authorship log for squashed commits");
    eprintln!(
        "    <base_branch> <new_sha> <old_sha>  Required: base branch, new commit SHA, old commit SHA"
    );
    eprintln!("    --dry-run             Show what would be done without making changes");
    eprintln!("  git-path           Print the path to the underlying git executable");
    eprintln!("  upgrade            Check for updates and install if available");
    eprintln!("    --force               Reinstall latest version even if already up to date");
    eprintln!("  fetch-notes [remote] Synchronously fetch AI authorship notes");
    eprintln!("    --remote <name>       Explicit remote name (default: upstream or origin)");
    eprintln!("    --json                Output result as JSON");
    eprintln!("  login              Authenticate with Git AI");
    eprintln!("  logout             Clear stored credentials");
    eprintln!("  whoami             Show auth state and login identity");
    eprintln!("  version, -v, --version     Print the git-ai version");
    eprintln!("  help, -h, --help           Show this help message");
    eprintln!();
    std::process::exit(0);
}

fn handle_checkpoint(args: &[String]) {
    let mut repository_working_dir = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Parse checkpoint-specific arguments
    let mut hook_input = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hook-input" => {
                if i + 1 < args.len() {
                    hook_input = Some(strip_utf8_bom(args[i + 1].clone()));
                    if hook_input.as_ref().unwrap() == "stdin" {
                        let mut stdin = std::io::stdin();
                        let mut buffer = String::new();
                        if let Err(e) = stdin.read_to_string(&mut buffer) {
                            eprintln!("Failed to read stdin for hook input: {}", e);
                            std::process::exit(0);
                        }
                        if !buffer.trim().is_empty() {
                            hook_input = Some(strip_utf8_bom(buffer));
                        } else {
                            eprintln!("No hook input provided (via --hook-input or stdin).");
                            std::process::exit(0);
                        }
                    } else if hook_input.as_ref().unwrap().trim().is_empty() {
                        eprintln!("Error: --hook-input requires a value");
                        std::process::exit(0);
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --hook-input requires a value or 'stdin' to read from stdin");
                    std::process::exit(0);
                }
            }

            _ => {
                i += 1;
            }
        }
    }

    let mut checkpoint_request: Option<CheckpointRequest> = None;
    // Handle preset arguments after parsing all flags
    if !args.is_empty()
        && crate::commands::checkpoint_agent::presets::resolve_preset(args[0].as_str()).is_ok()
    {
        // For mock/test presets invoked via CLI args (no --hook-input), synthesize
        // JSON so they can go through the standard orchestrator path.
        let effective_hook_input = if hook_input.is_some() {
            hook_input.clone().unwrap_or_default()
        } else {
            synthesize_hook_input_from_cli_args(args[0].as_str(), &args[1..])
        };

        match crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint(
            args[0].as_str(),
            &effective_hook_input,
        ) {
            Ok(mut results) => {
                if results.is_empty() {
                    // SnapshotOnly or no-op: orchestrator handled
                    // the side effect already, nothing to checkpoint.
                    std::process::exit(0);
                }

                // Use the first result as the primary checkpoint; any additional
                // results are extra events from the same preset invocation (e.g. a
                // preset that emits both a pre and post event in one call).
                let mut first = results.remove(0);

                // mock_ai with no file paths: fall back to all staged/unstaged files
                if args[0] == "mock_ai" && first.file_paths.is_empty() {
                    let working_dir = first.repo_working_dir.to_string_lossy().to_string();
                    first.file_paths = get_all_files_for_mock_ai(&working_dir)
                        .into_iter()
                        .map(std::path::PathBuf::from)
                        .collect();
                }
                repository_working_dir = first.repo_working_dir.to_string_lossy().to_string();
                checkpoint_request = Some(first);

                // Process any additional results from the same preset invocation
                for extra in results {
                    let working_dir = extra.repo_working_dir.to_string_lossy().to_string();
                    if let Ok(extra_repo) = find_repository_in_path(&working_dir) {
                        let user_name = extra_repo.git_author_identity().name_or_unknown();
                        let _ = run_checkpoint_via_daemon_or_local(
                            &extra_repo,
                            &user_name,
                            extra.checkpoint_kind,
                            true,
                            Some(extra),
                            false,
                            false,
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("{} preset error: {}", args[0], e);
                std::process::exit(0);
            }
        }
    }

    // Emit agent_usage metric for every AI hook, regardless of whether a
    // file-edit checkpoint is created downstream.  The existing per-prompt
    // throttle (`should_emit_agent_usage`) prevents duplicate events.
    if let Some(ref result) = checkpoint_request
        && result.checkpoint_kind.is_ai()
        && let Some(ref agent_id) = result.agent_id
        && commands::checkpoint::should_emit_agent_usage(agent_id)
    {
        let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
        let attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
            .tool(&agent_id.tool)
            .model(&agent_id.model)
            .prompt_id(prompt_id)
            .external_prompt_id(&agent_id.id)
            .custom_attributes_map(crate::config::Config::fresh().custom_attributes());

        let values = crate::metrics::AgentUsageValues::new();
        crate::metrics::record(values, attrs);
    }

    let final_working_dir = checkpoint_request
        .as_ref()
        .map(|r| r.repo_working_dir.to_string_lossy().to_string())
        .unwrap_or_else(|| repository_working_dir.clone());

    // Try to find the git repository
    // First, try the standard approach using the working directory
    let repo_result = find_repository_in_path(&final_working_dir);

    let config = config::Config::get();
    if let Ok(ref repo) = repo_result
        && !config.is_allowed_repository(&Some(repo.clone()))
    {
        eprintln!(
            "Skipping checkpoint because repository is excluded or not in allow_repositories list"
        );
        std::process::exit(0);
    }

    // If the working directory is not a git repository, we need to detect repos from file paths
    // This happens in multi-repo workspaces where the workspace root contains multiple git repos.
    // We also trigger file-based detection when the CWD *is* a git repo but an edited file lives
    // in a different git repo — most commonly a linked worktree created with `git worktree add`.
    // In that case git-ai would otherwise attempt to checkpoint the file against the CWD repo,
    // which cannot see changes inside the linked worktree's working tree.
    let needs_file_based_repo_detection = repo_result.is_err()
        || if let Ok(ref cwd_repo) = repo_result {
            checkpoint_request
                .as_ref()
                .filter(|r| !r.file_paths.is_empty())
                .map(|r| {
                    r.file_paths.iter().any(|f| {
                        let pb = if f.is_absolute() {
                            f.clone()
                        } else {
                            std::path::Path::new(&repository_working_dir).join(f)
                        };
                        !cwd_repo.path_is_in_workdir(&pb)
                    })
                })
                .unwrap_or(false)
        } else {
            false
        };

    if needs_file_based_repo_detection {
        // Workspace root is not a git repo - try to detect repositories from edited files
        let files_to_check = checkpoint_request.as_ref().map(|r| &r.file_paths);

        if let Some(files) = files_to_check
            && !files.is_empty()
        {
            // Convert relative paths to absolute paths based on workspace root
            let absolute_files: Vec<String> = files
                .iter()
                .map(|f| {
                    if f.is_absolute() {
                        f.to_string_lossy().to_string()
                    } else {
                        std::path::Path::new(&repository_working_dir)
                            .join(f)
                            .to_string_lossy()
                            .to_string()
                    }
                })
                .collect();

            // Group files by their containing repository.
            // Pass None as workspace_root so that find_repository_for_file can search
            // outside the CWD boundary. This fixes issue #954 where launching from a
            // non-git directory (e.g. /tmp) caused the workspace boundary to block
            // discovery of repos in sibling directories.
            let (repo_files, orphan_files) = group_files_by_repository(&absolute_files, None);

            if repo_files.is_empty() {
                eprintln!(
                    "Failed to find any git repositories for the edited files. Orphaned files: {:?}",
                    orphan_files
                );
                std::process::exit(0);
            }

            // Log orphan files if any
            if !orphan_files.is_empty() {
                eprintln!(
                    "Warning: {} file(s) are not in any git repository and will be skipped: {:?}",
                    orphan_files.len(),
                    orphan_files
                );
            }

            // Determine if this is truly a multi-repo workspace or just a single nested repo
            let is_multi_repo = repo_files.len() > 1;

            if is_multi_repo {
                eprintln!(
                    "Multi-repo workspace detected. Found {} repositories with edits.",
                    repo_files.len()
                );
            } else {
                eprintln!(
                    "Workspace root is not a git repository. Detected repository from edited files."
                );
            }

            let checkpoint_kind = checkpoint_request
                .as_ref()
                .map(|r| r.checkpoint_kind)
                .unwrap_or(CheckpointKind::Human);
            let allow_captured_async =
                checkpoint_request_has_explicit_capture_scope(args, checkpoint_request.as_ref());

            let checkpoint_start = std::time::Instant::now();
            let mut total_files_edited = 0;
            let mut repos_processed: usize = 0;
            let mut queued_repos: usize = 0;
            let total_repos = repo_files.len();

            // Process each repository separately
            for (repo_workdir, (repo, repo_file_paths)) in repo_files {
                if !config.is_allowed_repository(&Some(repo.clone())) {
                    eprintln!(
                        "Skipping checkpoint for {} because repository is excluded or not in allow_repositories list",
                        repo_workdir.display()
                    );
                    continue;
                }
                repos_processed += 1;
                eprintln!(
                    "Processing repository {}/{}: {}",
                    repos_processed,
                    total_repos,
                    repo_workdir.display()
                );

                // Get user name from this repo's config
                let default_user_name = repo.git_author_identity().name_or_unknown();

                // Create a modified checkpoint_request with only this repo's files
                let repo_checkpoint_request = checkpoint_request.as_ref().map(|r| {
                    let mut modified = r.clone();
                    modified.repo_working_dir = repo_workdir.clone();
                    modified.file_paths = repo_file_paths
                        .iter()
                        .map(std::path::PathBuf::from)
                        .collect();
                    modified
                });

                let checkpoint_outcome = run_checkpoint_via_daemon_or_local(
                    &repo,
                    &default_user_name,
                    checkpoint_kind,
                    false,
                    repo_checkpoint_request,
                    allow_captured_async,
                    false,
                );

                match checkpoint_outcome {
                    Ok(outcome) => {
                        total_files_edited += outcome.stats.1;
                        if outcome.queued {
                            queued_repos += 1;
                            eprintln!(
                                "  Checkpoint for {} queued ({} files)",
                                repo_workdir.display(),
                                outcome.stats.1
                            );
                        } else {
                            eprintln!(
                                "  Checkpoint for {} completed ({} files)",
                                repo_workdir.display(),
                                outcome.stats.1
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("  Checkpoint for {} failed: {}", repo_workdir.display(), e);
                        let context = serde_json::json!({
                            "function": "checkpoint",
                            "repo": repo_workdir.to_string_lossy(),
                            "checkpoint_kind": format!("{:?}", checkpoint_kind)
                        });
                        observability::log_error(&e, Some(context));
                        // Continue processing other repos instead of exiting
                    }
                }
            }

            let elapsed = checkpoint_start.elapsed();
            log_performance_for_checkpoint(total_files_edited, elapsed, checkpoint_kind);
            if is_multi_repo {
                if queued_repos == repos_processed && queued_repos > 0 {
                    eprintln!(
                        "Checkpoint queued in {:?} ({} repositories, {} total files)",
                        elapsed, repos_processed, total_files_edited
                    );
                } else if queued_repos == 0 {
                    eprintln!(
                        "Checkpoint completed in {:?} ({} repositories, {} total files)",
                        elapsed, repos_processed, total_files_edited
                    );
                } else {
                    eprintln!(
                        "Checkpoint dispatched in {:?} ({} queued, {} completed, {} total files)",
                        elapsed,
                        queued_repos,
                        repos_processed.saturating_sub(queued_repos),
                        total_files_edited
                    );
                }
            } else if queued_repos > 0 {
                eprintln!("Checkpoint queued in {:?}", elapsed);
            } else {
                eprintln!("Checkpoint completed in {:?}", elapsed);
            }
            return;
        }

        // No files to check, fall through to error
        eprintln!(
            "Failed to find repository: workspace root is not a git repository and no edited files provided"
        );
        std::process::exit(0);
    }

    // Standard single-repo mode
    let repo = repo_result.unwrap();

    // Get the effective working directory from the detected repository
    let effective_working_dir = repo
        .workdir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| final_working_dir.clone());

    let mut checkpoint_kind = checkpoint_request
        .as_ref()
        .map(|r| r.checkpoint_kind)
        .unwrap_or(CheckpointKind::Human);

    // If a git commit fires inside an AI bash tool call (e.g. `echo foo > f && git commit -am x`),
    // the pre-commit hook reaches here with no agent context and would default to Human.
    // Override to AI when a non-stale pre-snapshot exists, which is the precise signal
    // that a bash invocation is in flight. This uses existing snapshot lifecycle — no new
    // daemon messages or side-channel files needed.
    if checkpoint_kind == CheckpointKind::Human && checkpoint_request.is_none() {
        let repo_root = std::path::Path::new(&effective_working_dir);

        if let Some((resolved_kind, resolved_checkpoint_request)) =
            crate::commands::checkpoint_agent::bash_tool::checkpoint_context_from_active_bash(
                repo_root,
                &effective_working_dir,
            )
        {
            tracing::debug!("Using active bash context for pre-commit AI checkpoint");
            checkpoint_kind = resolved_kind;
            checkpoint_request = resolved_checkpoint_request;
        }
    }

    let allow_captured_async =
        checkpoint_request_has_explicit_capture_scope(args, checkpoint_request.as_ref());

    if CheckpointKind::Human == checkpoint_kind && checkpoint_request.is_none() {
        // Parse pathspecs after `--` for human checkpoints
        let will_edit_filepaths = if let Some(separator_pos) = args.iter().position(|a| a == "--") {
            let paths: Vec<String> = args[separator_pos + 1..]
                .iter()
                .filter(|arg| !arg.starts_with("--"))
                .cloned()
                .collect();
            if paths.is_empty() { None } else { Some(paths) }
        } else {
            Some(get_all_files_for_mock_ai(&effective_working_dir))
        };

        checkpoint_request = Some(CheckpointRequest {
            trace_id: crate::authorship::authorship_log_serialization::generate_trace_id(),
            checkpoint_kind: CheckpointKind::Human,
            agent_id: None,
            repo_working_dir: std::path::PathBuf::from(&effective_working_dir),
            file_paths: will_edit_filepaths
                .unwrap_or_default()
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect(),
            path_role: PreparedPathRole::WillEdit,
            dirty_files: None,
            transcript_source: None,
            metadata: std::collections::HashMap::new(),
            captured_checkpoint_id: None,
        });
    }

    // Get the current user name
    let default_user_name = repo.git_author_identity().name_or_unknown();

    let checkpoint_start = std::time::Instant::now();
    let agent_tool = checkpoint_request
        .as_ref()
        .and_then(|r| r.agent_id.as_ref())
        .map(|aid| aid.tool.clone());

    let external_files: Vec<String> = checkpoint_request
        .as_ref()
        .filter(|r| !r.file_paths.is_empty())
        .map(|r| {
            let repo_workdir = repo.workdir().ok();
            r.file_paths
                .iter()
                .filter_map(|path| {
                    let workdir = repo_workdir.as_ref()?;
                    let path_buf = if path.is_absolute() {
                        path.clone()
                    } else {
                        workdir.join(path)
                    };
                    if repo.path_is_in_workdir(&path_buf) {
                        None
                    } else {
                        let abs = if path.is_absolute() {
                            path.to_string_lossy().to_string()
                        } else {
                            workdir.join(path).to_string_lossy().to_string()
                        };
                        Some(abs)
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let external_agent_base = if !external_files.is_empty() {
        checkpoint_request.as_ref().cloned()
    } else {
        None
    };

    let checkpoint_request = run_checkpoint_via_daemon_or_local(
        &repo,
        &default_user_name,
        checkpoint_kind,
        false,
        checkpoint_request,
        allow_captured_async,
        false,
    );
    let local_checkpoint_failed = checkpoint_request.is_err();
    match checkpoint_request {
        Ok(outcome) => {
            let elapsed = checkpoint_start.elapsed();
            log_performance_for_checkpoint(outcome.stats.1, elapsed, checkpoint_kind);
            if outcome.queued {
                eprintln!("Checkpoint queued in {:?}", elapsed);
            } else {
                eprintln!("Checkpoint completed in {:?}", elapsed);
            }
        }
        Err(e) => {
            let elapsed = checkpoint_start.elapsed();
            eprintln!("Checkpoint failed after {:?} with error {}", elapsed, e);
            let context = serde_json::json!({
                "function": "checkpoint",
                "agent": agent_tool.clone().unwrap_or_default(),
                "duration": elapsed.as_millis(),
                "checkpoint_kind": format!("{:?}", checkpoint_kind)
            });
            observability::log_error(&e, Some(context));
        }
    }

    if !external_files.is_empty()
        && let Some(base_result) = external_agent_base
    {
        let (repo_files, orphan_files) = group_files_by_repository(&external_files, None);

        if !orphan_files.is_empty() {
            eprintln!(
                "Warning: {} cross-repo file(s) are not in any git repository and will be skipped",
                orphan_files.len()
            );
        }

        for (repo_workdir, (ext_repo, repo_file_paths)) in repo_files {
            if !config.is_allowed_repository(&Some(ext_repo.clone())) {
                continue;
            }

            let ext_user_name = ext_repo.git_author_identity().name_or_unknown();

            let mut modified = base_result.clone();
            modified.repo_working_dir = repo_workdir.clone();
            // Clear stale captured checkpoint ID — the original capture was consumed
            // (or will be consumed) by the primary repo's checkpoint dispatch and
            // the on-disk files may already be deleted by the daemon.
            modified.captured_checkpoint_id = None;
            modified.file_paths = repo_file_paths
                .iter()
                .map(std::path::PathBuf::from)
                .collect();

            match run_checkpoint_via_daemon_or_local(
                &ext_repo,
                &ext_user_name,
                checkpoint_kind,
                false,
                Some(modified),
                allow_captured_async,
                false,
            ) {
                Ok(outcome) => {
                    if outcome.queued {
                        eprintln!(
                            "Cross-repo checkpoint for {} queued ({} files)",
                            repo_workdir.display(),
                            outcome.stats.1
                        );
                    } else {
                        eprintln!(
                            "Cross-repo checkpoint for {} completed ({} files)",
                            repo_workdir.display(),
                            outcome.stats.1
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Cross-repo checkpoint for {} failed: {}",
                        repo_workdir.display(),
                        e
                    );
                    let context = serde_json::json!({
                        "function": "checkpoint",
                        "repo": repo_workdir.to_string_lossy(),
                        "checkpoint_kind": format!("{:?}", checkpoint_kind)
                    });
                    observability::log_error(&e, Some(context));
                }
            }
        }
    }

    if local_checkpoint_failed {
        std::process::exit(0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckpointDispatchOutcome {
    stats: (usize, usize, usize),
    queued: bool,
}

#[allow(clippy::too_many_arguments)]
fn run_checkpoint_via_daemon_or_local(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    allow_captured_async: bool,
    is_pre_commit: bool,
) -> Result<CheckpointDispatchOutcome, crate::error::GitAiError> {
    if daemon_checkpoint_delegate_enabled() {
        let repo_working_dir = repo.workdir().map(|p| p.to_string_lossy().to_string()).ok();
        if let Some(repo_working_dir) = repo_working_dir {
            let is_test = std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
                || std::env::var_os("GITAI_TEST_DB_PATH").is_some();
            let checkpoint_daemon_timeout = if cfg!(windows) || is_test {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(5)
            };
            match crate::commands::daemon::ensure_daemon_running(checkpoint_daemon_timeout) {
                Ok(config) => {
                    // Early path: if the bash tool already captured a checkpoint,
                    // submit it directly to the daemon without re-capturing.
                    if let Some(capture_id) = checkpoint_request
                        .as_ref()
                        .and_then(|r| r.captured_checkpoint_id.as_deref())
                    {
                        // Patch the manifest with the real agent identity/transcript/metadata
                        // so the daemon sees the actual agent context instead of the synthetic
                        // placeholder written at bash-tool capture time.
                        if let Err(e) =
                            crate::commands::checkpoint::update_captured_checkpoint_agent_context(
                                capture_id,
                                author,
                                checkpoint_request.as_ref(),
                            )
                        {
                            tracing::debug!(
                                "Failed to update captured checkpoint agent context: {}",
                                e
                            );
                        }

                        let request = ControlRequest::CheckpointRun {
                            request: Box::new(CheckpointRunRequest::Captured(
                                CapturedCheckpointRunRequest {
                                    repo_working_dir: repo_working_dir.clone(),
                                    capture_id: capture_id.to_string(),
                                },
                            )),
                            wait: Some(false),
                        };
                        match send_control_request(&config.control_socket_path, &request) {
                            Ok(response) if response.ok => {
                                let estimated_files =
                                    estimate_checkpoint_file_count(kind, &checkpoint_request);
                                return Ok(CheckpointDispatchOutcome {
                                    stats: (0, estimated_files, 0),
                                    queued: true,
                                });
                            }
                            Ok(response) => {
                                let message = response
                                    .error
                                    .unwrap_or_else(|| "unknown error".to_string());
                                let _ = cleanup_captured_checkpoint_after_delegate_failure(
                                    capture_id,
                                    &repo_working_dir,
                                    kind,
                                    "bash_captured_request_cleanup_failed",
                                );
                                log_daemon_checkpoint_delegate_failure(
                                    "bash_captured_request_rejected",
                                    &repo_working_dir,
                                    kind,
                                    &message,
                                );
                            }
                            Err(e) => {
                                let _ = cleanup_captured_checkpoint_after_delegate_failure(
                                    capture_id,
                                    &repo_working_dir,
                                    kind,
                                    "bash_captured_connect_cleanup_failed",
                                );
                                log_daemon_checkpoint_delegate_failure(
                                    "bash_captured_connect_failed",
                                    &repo_working_dir,
                                    kind,
                                    &e.to_string(),
                                );
                            }
                        }
                        // Fall through to normal path on failure
                    }

                    if allow_captured_async
                        && crate::commands::checkpoint::explicit_capture_target_paths(
                            kind,
                            checkpoint_request.as_ref(),
                        )
                        .is_some()
                    {
                        match crate::commands::checkpoint::prepare_captured_checkpoint(
                            repo,
                            author,
                            kind,
                            checkpoint_request.as_ref(),
                            is_pre_commit,
                            None,
                        ) {
                            Ok(Some(capture)) => {
                                let request = ControlRequest::CheckpointRun {
                                    request: Box::new(CheckpointRunRequest::Captured(
                                        CapturedCheckpointRunRequest {
                                            repo_working_dir: capture.repo_working_dir.clone(),
                                            capture_id: capture.capture_id.clone(),
                                        },
                                    )),
                                    wait: Some(false),
                                };
                                match send_control_request(&config.control_socket_path, &request) {
                                    Ok(response) if response.ok => {
                                        return Ok(CheckpointDispatchOutcome {
                                            stats: (0, capture.file_count, 0),
                                            queued: true,
                                        });
                                    }
                                    Ok(response) => {
                                        let message = response
                                            .error
                                            .unwrap_or_else(|| "unknown error".to_string());
                                        let _ = cleanup_captured_checkpoint_after_delegate_failure(
                                            &capture.capture_id,
                                            &repo_working_dir,
                                            kind,
                                            "captured_request_cleanup_failed",
                                        );
                                        log_daemon_checkpoint_delegate_failure(
                                            "captured_request_rejected",
                                            &repo_working_dir,
                                            kind,
                                            &message,
                                        );
                                    }
                                    Err(e) => {
                                        let _ = cleanup_captured_checkpoint_after_delegate_failure(
                                            &capture.capture_id,
                                            &repo_working_dir,
                                            kind,
                                            "captured_connect_cleanup_failed",
                                        );
                                        log_daemon_checkpoint_delegate_failure(
                                            "captured_connect_failed",
                                            &repo_working_dir,
                                            kind,
                                            &e.to_string(),
                                        );
                                    }
                                }
                            }
                            Ok(None) => {
                                return Ok(CheckpointDispatchOutcome {
                                    stats: (0, 0, 0),
                                    queued: false,
                                });
                            }
                            Err(e) => {
                                log_daemon_checkpoint_delegate_failure(
                                    "capture_prepare_failed",
                                    &repo_working_dir,
                                    kind,
                                    &e.to_string(),
                                );
                            }
                        }
                    }

                    let request = ControlRequest::CheckpointRun {
                        request: Box::new(CheckpointRunRequest::Live(Box::new(
                            LiveCheckpointRunRequest {
                                repo_working_dir: repo_working_dir.clone(),
                                kind: Some(checkpoint_kind_to_str(kind).to_string()),
                                author: Some(author.to_string()),
                                quiet: Some(quiet),
                                is_pre_commit: Some(is_pre_commit),
                                checkpoint_request: checkpoint_request.clone(),
                            },
                        ))),
                        wait: Some(true),
                    };
                    match send_control_request(&config.control_socket_path, &request) {
                        Ok(response) if response.ok => {
                            let estimated_files =
                                estimate_checkpoint_file_count(kind, &checkpoint_request);
                            return Ok(CheckpointDispatchOutcome {
                                stats: (0, estimated_files, 0),
                                queued: false,
                            });
                        }
                        Ok(response) => {
                            let message = response
                                .error
                                .unwrap_or_else(|| "unknown error".to_string());
                            log_daemon_checkpoint_delegate_failure(
                                "request_rejected",
                                &repo_working_dir,
                                kind,
                                &message,
                            );
                        }
                        Err(e) => {
                            log_daemon_checkpoint_delegate_failure(
                                "connect_failed",
                                &repo_working_dir,
                                kind,
                                &e.to_string(),
                            );
                        }
                    }
                }
                Err(e) => {
                    log_daemon_checkpoint_delegate_failure(
                        "startup_failed",
                        &repo_working_dir,
                        kind,
                        &e,
                    );
                }
            }
        }
    }
    let stats =
        commands::checkpoint::run(repo, author, kind, quiet, checkpoint_request, is_pre_commit)?;
    Ok(CheckpointDispatchOutcome {
        stats,
        queued: false,
    })
}

fn checkpoint_request_has_explicit_capture_scope(
    args: &[String],
    checkpoint_request: Option<&CheckpointRequest>,
) -> bool {
    if args.first().map(String::as_str) == Some("mock_ai") {
        return args.iter().skip(1).any(|arg| !arg.starts_with("--"));
    }

    if let Some(separator_pos) = args.iter().position(|arg| arg == "--") {
        return args[separator_pos + 1..]
            .iter()
            .any(|arg| !arg.starts_with("--"));
    }

    checkpoint_request
        .and_then(|result| {
            crate::commands::checkpoint::explicit_capture_target_paths(
                result.checkpoint_kind,
                Some(result),
            )
        })
        .is_some()
}

fn cleanup_captured_checkpoint_after_delegate_failure(
    capture_id: &str,
    repo_working_dir: &str,
    kind: CheckpointKind,
    cleanup_phase: &str,
) -> Result<(), crate::error::GitAiError> {
    match crate::commands::checkpoint::delete_captured_checkpoint(capture_id) {
        Ok(()) => Ok(()),
        Err(error) => {
            log_daemon_checkpoint_delegate_failure(
                cleanup_phase,
                repo_working_dir,
                kind,
                &format!(
                    "failed cleaning up captured checkpoint {}: {}",
                    capture_id, error
                ),
            );
            Err(error)
        }
    }
}

fn log_daemon_checkpoint_delegate_failure(
    phase: &str,
    repo_working_dir: &str,
    kind: CheckpointKind,
    message: &str,
) {
    eprintln!(
        "[git-ai] checkpoint delegate {}: {}; falling back to local checkpoint",
        phase, message
    );

    let error = crate::error::GitAiError::Generic(format!(
        "daemon checkpoint delegate {}: {}",
        phase, message
    ));
    let context = serde_json::json!({
        "function": "run_checkpoint_via_daemon_or_local",
        "phase": phase,
        "repo_working_dir": repo_working_dir,
        "checkpoint_kind": checkpoint_kind_to_str(kind),
    });
    observability::log_error(&error, Some(context));
}

fn daemon_checkpoint_delegate_enabled() -> bool {
    crate::utils::checkpoint_delegation_enabled()
}

fn checkpoint_kind_to_str(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Human => "human",
        CheckpointKind::AiAgent => "ai_agent",
        CheckpointKind::AiTab => "ai_tab",
        CheckpointKind::KnownHuman => "known_human",
    }
}

fn estimate_checkpoint_file_count(
    _kind: CheckpointKind,
    checkpoint_request: &Option<CheckpointRequest>,
) -> usize {
    checkpoint_request
        .as_ref()
        .map(|r| r.file_paths.len())
        .unwrap_or(0)
}

fn strip_utf8_bom(input: String) -> String {
    if let Some(stripped) = input.strip_prefix('\u{feff}') {
        stripped.to_string()
    } else {
        input
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EffectiveIgnorePatternsRequest {
    user_patterns: Vec<String>,
    extra_patterns: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EffectiveIgnorePatternsResponse {
    patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlameAnalysisRequest {
    file_path: String,
    #[serde(default)]
    options: commands::blame::GitAiBlameOptions,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorshipRemoteRequest {
    remote_name: String,
}

#[derive(Debug, Serialize)]
struct FetchAuthorshipNotesResponse {
    notes_existence: String,
}

#[derive(Debug, Serialize)]
struct PushAuthorshipNotesResponse {
    ok: bool,
}

fn parse_machine_json_arg(args: &[String], command: &str) -> Result<String, String> {
    if args.len() != 2 || args[0] != "--json" {
        return Err(format!("Usage: git-ai {} --json '<json-payload>'", command));
    }

    let payload = strip_utf8_bom(args[1].clone());
    if payload.trim().is_empty() {
        return Err("JSON payload cannot be empty".to_string());
    }

    Ok(payload)
}

fn emit_machine_json_error(message: impl AsRef<str>) -> ! {
    let payload = serde_json::json!({ "error": message.as_ref() });
    if let Ok(json) = serde_json::to_string(&payload) {
        eprintln!("{}", json);
    } else {
        eprintln!(r#"{{"error":"failed to serialize error payload"}}"#);
    }
    std::process::exit(1);
}

fn print_machine_json(value: &serde_json::Value) {
    match serde_json::to_string(value) {
        Ok(json) => println!("{}", json),
        Err(e) => emit_machine_json_error(format!("Failed to serialize JSON output: {}", e)),
    }
}

fn disable_debug_logs_for_machine_command() {
    // SAFETY: git-ai command handlers run on the main thread and mutate process env
    // before spawning any worker threads for these internal machine commands.
    unsafe {
        std::env::set_var("GIT_AI_DEBUG", "0");
        std::env::remove_var("GIT_AI_DEBUG_PERFORMANCE");
    }
}

fn parse_authorship_remote_request(
    args: &[String],
    command: &str,
) -> (Repository, AuthorshipRemoteRequest) {
    let payload =
        parse_machine_json_arg(args, command).unwrap_or_else(|msg| emit_machine_json_error(msg));

    let request: AuthorshipRemoteRequest = serde_json::from_str(&payload)
        .unwrap_or_else(|e| emit_machine_json_error(format!("Invalid JSON payload: {}", e)));

    if request.remote_name.trim().is_empty() {
        emit_machine_json_error("remote_name cannot be empty");
    }

    let repo = find_repository(&Vec::<String>::new())
        .unwrap_or_else(|e| emit_machine_json_error(format!("Failed to find repository: {}", e)));

    (repo, request)
}

fn notes_existence_label(existence: NotesExistence) -> &'static str {
    match existence {
        NotesExistence::Found => "found",
        NotesExistence::NotFound => "not_found",
    }
}

fn handle_effective_ignore_patterns_internal(args: &[String]) {
    let payload = parse_machine_json_arg(args, "effective-ignore-patterns")
        .unwrap_or_else(|msg| emit_machine_json_error(msg));

    let request: EffectiveIgnorePatternsRequest = serde_json::from_str(&payload)
        .unwrap_or_else(|e| emit_machine_json_error(format!("Invalid JSON payload: {}", e)));

    let repo = find_repository(&Vec::<String>::new())
        .unwrap_or_else(|e| emit_machine_json_error(format!("Failed to find repository: {}", e)));

    let response = EffectiveIgnorePatternsResponse {
        patterns: effective_ignore_patterns(&repo, &request.user_patterns, &request.extra_patterns),
    };

    let response_value = serde_json::to_value(response).unwrap_or_else(|e| {
        emit_machine_json_error(format!("Failed to serialize command response: {}", e))
    });
    print_machine_json(&response_value);
}

fn handle_blame_analysis_internal(args: &[String]) {
    let payload = parse_machine_json_arg(args, "blame-analysis")
        .unwrap_or_else(|msg| emit_machine_json_error(msg));

    let request: BlameAnalysisRequest = serde_json::from_str(&payload)
        .unwrap_or_else(|e| emit_machine_json_error(format!("Invalid JSON payload: {}", e)));

    if request.file_path.trim().is_empty() {
        emit_machine_json_error("file_path cannot be empty");
    }

    let repo = find_repository(&Vec::<String>::new())
        .unwrap_or_else(|e| emit_machine_json_error(format!("Failed to find repository: {}", e)));

    let analysis = repo
        .blame_analysis(&request.file_path, &request.options)
        .unwrap_or_else(|e| emit_machine_json_error(format!("blame_analysis failed: {}", e)));

    let response_value = serde_json::to_value(analysis).unwrap_or_else(|e| {
        emit_machine_json_error(format!("Failed to serialize command response: {}", e))
    });
    print_machine_json(&response_value);
}

fn handle_fetch_authorship_notes_internal(args: &[String]) {
    disable_debug_logs_for_machine_command();
    let (repo, request) = parse_authorship_remote_request(args, "fetch-authorship-notes");

    let notes_existence = fetch_authorship_notes(&repo, &request.remote_name).unwrap_or_else(|e| {
        emit_machine_json_error(format!("fetch_authorship_notes failed: {}", e))
    });

    let response = FetchAuthorshipNotesResponse {
        notes_existence: notes_existence_label(notes_existence).to_string(),
    };
    let response_value = serde_json::to_value(response).unwrap_or_else(|e| {
        emit_machine_json_error(format!("Failed to serialize command response: {}", e))
    });
    print_machine_json(&response_value);
}

fn handle_push_authorship_notes_internal(args: &[String]) {
    disable_debug_logs_for_machine_command();
    let (repo, request) = parse_authorship_remote_request(args, "push-authorship-notes");

    push_authorship_notes(&repo, &request.remote_name).unwrap_or_else(|e| {
        emit_machine_json_error(format!("push_authorship_notes failed: {}", e))
    });

    let response = PushAuthorshipNotesResponse { ok: true };
    let response_value = serde_json::to_value(response).unwrap_or_else(|e| {
        emit_machine_json_error(format!("Failed to serialize command response: {}", e))
    });
    print_machine_json(&response_value);
}

fn handle_ai_blame(args: &[String]) {
    if args.is_empty() {
        eprintln!("Error: blame requires a file argument");
        std::process::exit(1);
    }

    // Find the git repository from current directory
    let current_dir = env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let repo = match find_repository_in_path(&current_dir) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    // Parse blame arguments
    let (file_path, mut options) = match commands::blame::parse_blame_args(args) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to parse blame arguments: {}", e);
            std::process::exit(1);
        }
    };

    // Auto-detect ignore-revs-file if not explicitly provided, not disabled via --no-ignore-revs-file,
    // and git version supports --ignore-revs-file (git >= 2.23)
    if options.ignore_revs_file.is_none()
        && !options.no_ignore_revs_file
        && repo.git_supports_ignore_revs_file()
    {
        // First, check git config for blame.ignoreRevsFile
        if let Ok(Some(config_path)) = repo.config_get_str("blame.ignoreRevsFile")
            && !config_path.is_empty()
        {
            // Config path could be relative to repo root or absolute
            if let Ok(workdir) = repo.workdir() {
                let full_path = if std::path::Path::new(&config_path).is_absolute() {
                    std::path::PathBuf::from(&config_path)
                } else {
                    workdir.join(&config_path)
                };
                if full_path.exists() {
                    options.ignore_revs_file = Some(full_path.to_string_lossy().to_string());
                }
            }
        }

        // If still not set, check for .git-blame-ignore-revs in the repository root
        if options.ignore_revs_file.is_none()
            && let Ok(workdir) = repo.workdir()
        {
            let ignore_revs_path = workdir.join(".git-blame-ignore-revs");
            if ignore_revs_path.exists() {
                options.ignore_revs_file = Some(ignore_revs_path.to_string_lossy().to_string());
            }
        }
    }

    // Check if this is an interactive terminal
    let is_interactive = std::io::stdout().is_terminal();

    if is_interactive && options.incremental {
        // For incremental mode in interactive terminal, we need special handling
        // This would typically involve a pager like less
        eprintln!("Error: incremental mode is not supported in interactive terminal");
        std::process::exit(1);
    }

    let file_path = if !std::path::Path::new(&file_path).is_absolute() {
        let current_dir_path = std::path::PathBuf::from(&current_dir);
        current_dir_path
            .join(&file_path)
            .to_string_lossy()
            .to_string()
    } else {
        file_path
    };

    if let Err(e) = repo.blame(&file_path, &options) {
        eprintln!("Blame failed: {}", e);
        std::process::exit(1);
    }
}

fn handle_ai_diff(args: &[String]) {
    let current_dir = env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let repo = match find_repository_in_path(&current_dir) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = commands::diff::handle_diff(&repo, args) {
        eprintln!("Diff failed: {}", e);
        std::process::exit(1);
    }
}

fn handle_stats(args: &[String]) {
    // Find the git repository
    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };
    // Parse stats-specific arguments
    let mut json_output = false;
    let mut commit_sha = None;
    let mut commit_range: Option<CommitRange> = None;
    let mut ignore_patterns: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json_output = true;
                i += 1;
            }
            "--ignore" => {
                // Collect all arguments after --ignore until we hit another flag or commit SHA
                // This supports shell glob expansion: `--ignore *.lock` expands to `--ignore Cargo.lock package.lock`
                i += 1;
                let mut found_pattern = false;
                while i < args.len() {
                    let arg = &args[i];
                    // Stop if we hit another flag
                    if arg.starts_with("--") {
                        break;
                    }
                    // Stop if this looks like a commit SHA or range (contains ..)
                    if arg.contains("..")
                        || (commit_sha.is_none() && !found_pattern && arg.len() >= 7)
                    {
                        // Could be a commit SHA, stop collecting patterns
                        break;
                    }
                    ignore_patterns.push(arg.clone());
                    found_pattern = true;
                    i += 1;
                }
                if !found_pattern {
                    eprintln!("--ignore requires at least one pattern argument");
                    std::process::exit(1);
                }
            }
            _ => {
                // First non-flag argument is treated as commit SHA or range
                if commit_sha.is_none() {
                    let arg = &args[i];
                    // Check if this is a commit range (contains "..")
                    if arg.contains("..") {
                        let parts: Vec<&str> = arg.split("..").collect();
                        if parts.len() == 2 {
                            match CommitRange::new_infer_refname(
                                &repo,
                                normalize_head_rev(parts[0]),
                                normalize_head_rev(parts[1]),
                                // @todo this is probably fine, but we might want to give users an option to override from this command.
                                None,
                            ) {
                                Ok(range) => {
                                    commit_range = Some(range);
                                }
                                Err(e) => {
                                    eprintln!("Failed to create commit range: {}", e);
                                    std::process::exit(1);
                                }
                            }
                        } else {
                            eprintln!("Invalid commit range format. Expected: <commit>..<commit>");
                            std::process::exit(1);
                        }
                    } else {
                        commit_sha = Some(normalize_head_rev(arg));
                    }
                    i += 1;
                } else {
                    eprintln!("Unknown stats argument: {}", args[i]);
                    std::process::exit(1);
                }
            }
        }
    }

    let effective_patterns = effective_ignore_patterns(&repo, &ignore_patterns, &[]);

    // Handle commit range if detected
    if let Some(range) = commit_range {
        match range_authorship::range_authorship(range, false, &effective_patterns, None) {
            Ok(stats) => {
                if json_output {
                    let json_str = serde_json::to_string(&stats).unwrap();
                    println!("{}", json_str);
                } else {
                    range_authorship::print_range_authorship_stats(&stats);
                }
            }
            Err(e) => {
                eprintln!("Range authorship failed: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    if let Err(e) = stats_command(
        &repo,
        commit_sha.as_deref(),
        json_output,
        &effective_patterns,
    ) {
        match e {
            crate::error::GitAiError::Generic(msg) if msg.starts_with("No commit found:") => {
                eprintln!("{}", msg);
            }
            _ => {
                eprintln!("Stats failed: {}", e);
            }
        }
        std::process::exit(1);
    }
}

/// Normalise a revision token that the user may have typed with a lowercase
/// "head" prefix.  On case-insensitive file systems (macOS) git accepts both
/// "head" and "HEAD", but in a linked worktree "head" can resolve to the
/// *main* repository's HEAD file rather than the worktree's own HEAD, so the
/// wrong commit is used.  On case-sensitive file systems (Linux) "head"
/// simply fails with "Not a valid revision".  Normalising to uppercase "HEAD"
/// before passing to git fixes both issues.
///
/// Only the four-character prefix is replaced; suffixes like `~2`, `^1` or
/// `@{0}` are preserved verbatim.
fn normalize_head_rev(rev: &str) -> String {
    if rev.len() >= 4 && rev[..4].eq_ignore_ascii_case("head") {
        let suffix = &rev[4..];
        if suffix.is_empty()
            || suffix.starts_with('~')
            || suffix.starts_with('^')
            || suffix.starts_with('@')
        {
            return format!("HEAD{}", suffix);
        }
    }
    rev.to_string()
}

fn handle_git_hooks(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("remove") | Some("uninstall") => {
            let repo = match find_repository(&Vec::<String>::new()) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to find repository: {}", e);
                    std::process::exit(1);
                }
            };

            match commands::git_hook_handlers::remove_repo_hooks(&repo, false) {
                Ok(report) => {
                    let status = if report.changed { "removed" } else { "ok" };
                    println!(
                        "repo hooks {}: {}",
                        status,
                        report.managed_hooks_path.to_string_lossy()
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Failed to remove repo hooks: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("The git core hooks feature has been sunset.");
            eprintln!("Usage: git-ai git-hooks remove");
            std::process::exit(1);
        }
    }
}

fn get_all_files_for_mock_ai(working_dir: &str) -> Vec<String> {
    // Find the git repository
    let repo = match find_repository_in_path(working_dir) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            return Vec::new();
        }
    };
    match repo.get_staged_and_unstaged_filenames() {
        Ok(filenames) => filenames.into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

/// Synthesize JSON hook_input from CLI args for mock/test presets that can be
/// invoked without --hook-input.
fn synthesize_hook_input_from_cli_args(preset_name: &str, remaining_args: &[String]) -> String {
    match preset_name {
        "human" | "mock_ai" | "mock_known_human" => {
            let paths: Vec<&str> = remaining_args
                .iter()
                .filter(|a| !a.starts_with("--"))
                .map(|s| s.as_str())
                .collect();
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .to_string();
            serde_json::json!({
                "file_paths": paths,
                "cwd": cwd,
            })
            .to_string()
        }
        "known_human" => {
            let mut editor = "unknown".to_string();
            let mut editor_version = "unknown".to_string();
            let mut extension_version = "unknown".to_string();
            let mut files: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < remaining_args.len() {
                match remaining_args[i].as_str() {
                    "--editor" if i + 1 < remaining_args.len() => {
                        editor = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--editor-version" if i + 1 < remaining_args.len() => {
                        editor_version = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--extension-version" if i + 1 < remaining_args.len() => {
                        extension_version = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--" => {
                        files.extend(remaining_args[i + 1..].iter().cloned());
                        break;
                    }
                    arg if !arg.starts_with("--") => {
                        files.push(arg.to_string());
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .to_string();
            serde_json::json!({
                "editor": editor,
                "editor_version": editor_version,
                "extension_version": extension_version,
                "cwd": cwd,
                "edited_filepaths": files,
            })
            .to_string()
        }
        _ => String::new(),
    }
}

#[cfg(debug_assertions)]
fn handle_show_transcript(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Error: show-transcript requires agent name and path/id");
        eprintln!("Usage: git-ai show-transcript <agent> <path|id>");
        eprintln!(
            "  Agents: claude, codex, gemini, continue-cli, github-copilot, cursor, amp, windsurf"
        );
        eprintln!("  For amp, provide conversation/thread id instead of path");
        std::process::exit(1);
    }

    let agent_name = &args[0];
    let path_or_id = &args[1];

    let result: Result<
        (crate::authorship::transcript::AiTranscript, Option<String>),
        crate::error::GitAiError,
    > = match agent_name.as_str() {
        "claude" => match transcript_readers::read_claude_jsonl(std::path::Path::new(path_or_id)) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Claude transcript: {}", e);
                std::process::exit(1);
            }
        },
        "codex" => match transcript_readers::read_codex_jsonl(std::path::Path::new(path_or_id)) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Codex transcript: {}", e);
                std::process::exit(1);
            }
        },
        "gemini" => match transcript_readers::read_gemini_json(std::path::Path::new(path_or_id)) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Gemini transcript: {}", e);
                std::process::exit(1);
            }
        },
        "windsurf" => {
            match transcript_readers::read_windsurf_jsonl(std::path::Path::new(path_or_id)) {
                Ok((transcript, model)) => Ok((transcript, model)),
                Err(e) => {
                    eprintln!("Error loading Windsurf transcript: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "continue-cli" => {
            match transcript_readers::read_continue_json(std::path::Path::new(path_or_id)) {
                Ok(transcript) => Ok((transcript, None)),
                Err(e) => {
                    eprintln!("Error loading Continue CLI transcript: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "github-copilot" => {
            match transcript_readers::read_copilot_session_json(std::path::Path::new(path_or_id)) {
                Ok((transcript, model, _file_paths)) => Ok((transcript, model)),
                Err(e) => {
                    eprintln!("Error loading GitHub Copilot transcript: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "cursor" => match transcript_readers::read_cursor_jsonl(std::path::Path::new(path_or_id)) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Cursor transcript: {}", e);
                std::process::exit(1);
            }
        },
        "amp" => {
            let path = std::path::Path::new(path_or_id);
            let amp_result = if path.exists() {
                transcript_readers::read_amp_thread_json(path)
                    .map(|(transcript, model, _thread_id)| (transcript, model))
            } else {
                transcript_readers::read_amp_thread_by_id(path_or_id)
            };

            match amp_result {
                Ok((transcript, model)) => Ok((transcript, model)),
                Err(e) => {
                    eprintln!("Error loading Amp transcript: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Error: Unknown agent '{}'", agent_name);
            eprintln!(
                "Supported agents: claude, codex, gemini, continue-cli, github-copilot, cursor, amp, windsurf"
            );
            std::process::exit(1);
        }
    };

    match result {
        Ok((transcript, model)) => {
            // Serialize transcript to JSON
            let transcript_json = match serde_json::to_string_pretty(&transcript) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Error serializing transcript: {}", e);
                    std::process::exit(1);
                }
            };

            // Print model and transcript
            if let Some(model_name) = model {
                println!("Model: {}", model_name);
            } else {
                println!("Model: (not available)");
            }
            println!("\nTranscript:");
            println!("{}", transcript_json);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Exit mirroring the child's termination status, re-raising the original
/// signal on Unix so the calling shell sees the correct termination reason
/// (e.g. SIGPIPE from `git ai log | head`).
fn exit_with_log_status(status: std::process::ExitStatus) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            unreachable!();
        }
    }
    std::process::exit(status.code().unwrap_or(1));
}
