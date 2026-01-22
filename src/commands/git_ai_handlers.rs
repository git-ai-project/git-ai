use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::range_authorship;
use crate::authorship::stats::stats_command;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands;
use crate::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, AiTabPreset, ClaudePreset,
    ContinueCliPreset, CursorPreset, GeminiPreset, GithubCopilotPreset,
};
use crate::commands::checkpoint_agent::agent_v1_preset::AgentV1Preset;
use crate::config;
use crate::git::find_repository;
use crate::git::find_repository_in_path;
use crate::git::repository::{group_files_by_repository, CommitRange};
use crate::observability::wrapper_performance_targets::log_performance_for_checkpoint;
use crate::observability::{self, log_message};
use crate::utils::is_interactive_terminal;
use std::env;
use std::io::IsTerminal;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn handle_git_ai(args: &[String]) {
    if args.is_empty() {
        print_help();
        return;
    }

    let current_dir = env::current_dir().unwrap().to_string_lossy().to_string();
    let repository_option = find_repository_in_path(&current_dir).ok();

    // Set repo context to flush buffered events
    if let Some(repo) = repository_option.as_ref() {
        observability::set_repo_context(repo);
    }

    let config = config::Config::get();

    let allowed_repository = config.is_allowed_repository(&repository_option);

    // Start DB warmup early for commands that need database access
    match args[0].as_str() {
        "checkpoint" | "show-prompt" | "share" | "sync-prompts" | "flush-cas" => {
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
            if !allowed_repository {
                eprintln!(
                    "Skipping checkpoint because repository is excluded or not in allow_repositories list"
                );
                std::process::exit(0);
            }
            handle_checkpoint(&args[1..]);
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
        "install-hooks" => match commands::install_hooks::run(&args[1..]) {
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
        "squash-authorship" => {
            commands::squash_authorship::handle_squash_authorship(&args[1..]);
        }
        "ci" => {
            commands::ci_handlers::handle_ci(&args[1..]);
        }
        "upgrade" => {
            commands::upgrade::run_with_args(&args[1..]);
        }
        "flush-logs" => {
            commands::flush_logs::handle_flush_logs(&args[1..]);
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
        "show-prompt" => {
            commands::show_prompt::handle_show_prompt(&args[1..]);
        }
        "share" => {
            commands::share::handle_share(&args[1..]);
        }
        "sync-prompts" => {
            commands::sync_prompts::handle_sync_prompts(&args[1..]);
        }
        "prompts" => {
            commands::prompts_db::handle_prompts(&args[1..]);
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
    eprintln!("");
    eprintln!("Usage: git-ai <command> [args...]");
    eprintln!("");
    eprintln!("Commands:");
    eprintln!("  checkpoint         Checkpoint working changes and attribute author");
    eprintln!("    Presets: claude, continue-cli, cursor, gemini, github-copilot, ai_tab, mock_ai");
    eprintln!(
        "    --hook-input <json|stdin>   JSON payload required by presets, or 'stdin' to read from stdin"
    );
    eprintln!("    --show-working-log          Display current working log");
    eprintln!("    --reset                     Reset working log");
    eprintln!("    mock_ai [pathspecs...]      Test preset accepting optional file pathspecs");
    eprintln!("  blame <file>       Git blame with AI authorship overlay");
    eprintln!("  diff <commit|range>  Show diff with AI authorship annotations");
    eprintln!("    <commit>              Diff from commit's parent to commit");
    eprintln!("    <commit1>..<commit2>  Diff between two commits");
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
    eprintln!("  share <id>         Share a prompt by creating a bundle");
    eprintln!("    --title <title>       Custom title for the bundle (default: auto-generated)");
    eprintln!("  sync-prompts       Update prompts in database to latest versions");
    eprintln!("    --since <time>        Only sync prompts updated after this time");
    eprintln!(
        "                          Formats: '1d', '2h', '1w', Unix timestamp, ISO8601, YYYY-MM-DD"
    );
    eprintln!("    --workdir <path>      Only sync prompts from specific repository");
    eprintln!("  config             View and manage git-ai configuration");
    eprintln!("                        Show all config as formatted JSON");
    eprintln!("    <key>                 Show specific config value (supports dot notation)");
    eprintln!("    set <key> <value>     Set a config value (arrays: single value = [value])");
    eprintln!("    --add <key> <value>   Add to array or upsert into object");
    eprintln!("    unset <key>           Remove config value (reverts to default)");
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
    eprintln!("  prompts            Create local SQLite database for prompt analysis");
    eprintln!("    --since <time>        Only include prompts after this time (default: 30d)");
    eprintln!("    --author <name>       Filter by human author (default: current git user)");
    eprintln!("    --all-authors         Include prompts from all authors");
    eprintln!("    --all-repositories    Include prompts from all repositories");
    eprintln!("    exec \"<SQL>\"          Execute arbitrary SQL on prompts.db");
    eprintln!("    list                  List prompts as TSV");
    eprintln!("    next                  Get next prompt as JSON (iterator pattern)");
    eprintln!("    reset                 Reset iteration pointer to start");
    eprintln!("  login              Authenticate with Git AI");
    eprintln!("  logout             Clear stored credentials");
    eprintln!("  version, -v, --version     Print the git-ai version");
    eprintln!("  help, -h, --help           Show this help message");
    eprintln!("");
    std::process::exit(0);
}

fn handle_checkpoint(args: &[String]) {
    let mut repository_working_dir = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Parse checkpoint-specific arguments
    let mut show_working_log = false;
    let mut reset = false;
    let mut hook_input = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--show-working-log" => {
                show_working_log = true;
                i += 1;
            }
            "--reset" => {
                reset = true;
                i += 1;
            }
            "--hook-input" => {
                if i + 1 < args.len() {
                    hook_input = Some(args[i + 1].clone());
                    if hook_input.as_ref().unwrap() == "stdin" {
                        let mut stdin = std::io::stdin();
                        let mut buffer = String::new();
                        if let Err(e) = stdin.read_to_string(&mut buffer) {
                            eprintln!("Failed to read stdin for hook input: {}", e);
                            std::process::exit(0);
                        }
                        if !buffer.trim().is_empty() {
                            hook_input = Some(buffer);
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

    let mut agent_run_result = None;
    // Handle preset arguments after parsing all flags
    if !args.is_empty() {
        match args[0].as_str() {
            "claude" => {
                match ClaudePreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Claude preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "gemini" => {
                match GeminiPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Gemini preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "continue-cli" => {
                match ContinueCliPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Continue CLI preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "cursor" => {
                match CursorPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Error running Cursor preset: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "github-copilot" => {
                match GithubCopilotPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Github Copilot preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "ai_tab" => {
                match AiTabPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("ai_tab preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "agent-v1" => {
                match AgentV1Preset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Agent V1 preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "mock_ai" => {
                let mock_agent_id = format!(
                    "ai-thread-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or_else(|_| 0)
                );

                // Collect all remaining args (after mock_ai and flags) as pathspecs
                let edited_filepaths = if args.len() > 1 {
                    let mut paths = Vec::new();
                    for arg in &args[1..] {
                        // Skip flags
                        if !arg.starts_with("--") {
                            paths.push(arg.clone());
                        }
                    }
                    if paths.is_empty() { None } else { Some(paths) }
                } else {
                    let working_dir = agent_run_result
                        .as_ref()
                        .and_then(|r| r.repo_working_dir.clone())
                        .unwrap_or(repository_working_dir.clone());
                    // Find the git repository
                    Some(get_all_files_for_mock_ai(&working_dir))
                };

                agent_run_result = Some(AgentRunResult {
                    agent_id: AgentId {
                        tool: "mock_ai".to_string(),
                        id: mock_agent_id,
                        model: "unknown".to_string(),
                    },
                    agent_metadata: None,
                    checkpoint_kind: CheckpointKind::AiAgent,
                    transcript: None,
                    repo_working_dir: None,
                    edited_filepaths,
                    will_edit_filepaths: None,
                    dirty_files: None,
                });
            }
            _ => {}
        }
    }

    let final_working_dir = agent_run_result
        .as_ref()
        .and_then(|r| r.repo_working_dir.clone())
        .unwrap_or_else(|| repository_working_dir.clone());

    // Try to find the git repository
    // First, try the standard approach using the working directory
    let repo_result = find_repository_in_path(&final_working_dir);

    // If the working directory is not a git repository, we need to detect repos from file paths
    // This happens in multi-repo workspaces where the workspace root contains multiple git repos
    let needs_file_based_repo_detection = repo_result.is_err();

    if needs_file_based_repo_detection {
        // Workspace root is not a git repo - try to detect repositories from edited files
        let files_to_check = agent_run_result.as_ref().and_then(|r| {
            if r.checkpoint_kind == CheckpointKind::Human {
                r.will_edit_filepaths.as_ref()
            } else {
                r.edited_filepaths.as_ref()
            }
        });

        if let Some(files) = files_to_check {
            if !files.is_empty() {
                // Convert relative paths to absolute paths based on workspace root
                let absolute_files: Vec<String> = files
                    .iter()
                    .map(|f| {
                        let path = std::path::Path::new(f);
                        if path.is_absolute() {
                            f.clone()
                        } else {
                            std::path::Path::new(&repository_working_dir)
                                .join(f)
                                .to_string_lossy()
                                .to_string()
                        }
                    })
                    .collect();

                // Group files by their containing repository
                let (repo_files, orphan_files) =
                    group_files_by_repository(&absolute_files, Some(&repository_working_dir));

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

                let checkpoint_kind = agent_run_result
                    .as_ref()
                    .map(|r| r.checkpoint_kind)
                    .unwrap_or(CheckpointKind::Human);

                let checkpoint_start = std::time::Instant::now();
                let mut total_files_edited = 0;
                let mut repos_processed = 0;
                let total_repos = repo_files.len();

                // Process each repository separately
                for (repo_workdir, (repo, repo_file_paths)) in repo_files {
                    repos_processed += 1;
                    eprintln!(
                        "Processing repository {}/{}: {}",
                        repos_processed,
                        total_repos,
                        repo_workdir.display()
                    );

                    // Get user name from this repo's config
                    let default_user_name = match repo.config_get_str("user.name") {
                        Ok(Some(name)) if !name.trim().is_empty() => name,
                        _ => {
                            eprintln!(
                                "Warning: git user.name not configured for {}. Using 'unknown'.",
                                repo_workdir.display()
                            );
                            "unknown".to_string()
                        }
                    };

                    // Create a modified agent_run_result with only this repo's files
                    let repo_agent_result = agent_run_result.as_ref().map(|r| {
                        let mut modified = r.clone();
                        modified.repo_working_dir = Some(repo_workdir.to_string_lossy().to_string());
                        if r.checkpoint_kind == CheckpointKind::Human {
                            modified.will_edit_filepaths = Some(repo_file_paths.clone());
                            modified.edited_filepaths = None;
                        } else {
                            modified.edited_filepaths = Some(repo_file_paths.clone());
                            modified.will_edit_filepaths = None;
                        }
                        modified
                    });

                    let checkpoint_result = commands::checkpoint::run(
                        &repo,
                        &default_user_name,
                        checkpoint_kind,
                        show_working_log,
                        reset,
                        false,
                        repo_agent_result,
                        false,
                    );

                    match checkpoint_result {
                        Ok((_, files_edited, _)) => {
                            total_files_edited += files_edited;
                            eprintln!(
                                "  Checkpoint for {} completed ({} files)",
                                repo_workdir.display(),
                                files_edited
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "  Checkpoint for {} failed: {}",
                                repo_workdir.display(),
                                e
                            );
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
                    eprintln!(
                        "Checkpoint completed in {:?} ({} repositories, {} total files)",
                        elapsed, repos_processed, total_files_edited
                    );
                } else {
                    eprintln!("Checkpoint completed in {:?}", elapsed);
                }
                return;
            }
        }

        // No files to check, fall through to error
        eprintln!("Failed to find repository: workspace root is not a git repository and no edited files provided");
        std::process::exit(0);
    }

    // Standard single-repo mode
    let repo = repo_result.unwrap();

    // Get the effective working directory from the detected repository
    let effective_working_dir = repo
        .workdir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| final_working_dir.clone());

    let checkpoint_kind = agent_run_result
        .as_ref()
        .map(|r| r.checkpoint_kind)
        .unwrap_or(CheckpointKind::Human);

    if CheckpointKind::Human == checkpoint_kind && agent_run_result.is_none() {
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

        agent_run_result = Some(AgentRunResult {
            agent_id: AgentId {
                tool: "mock_ai".to_string(),
                id: format!(
                    "ai-thread-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or_else(|_| 0)
                ),
                model: "unknown".to_string(),
            },
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::Human,
            transcript: None,
            will_edit_filepaths: Some(will_edit_filepaths.unwrap_or_default()),
            edited_filepaths: None,
            repo_working_dir: Some(effective_working_dir),
            dirty_files: None,
        });
    }

    // Get the current user name from git config
    let default_user_name = match repo.config_get_str("user.name") {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        _ => {
            eprintln!("Warning: git user.name not configured. Using 'unknown' as author.");
            "unknown".to_string()
        }
    };

    let checkpoint_start = std::time::Instant::now();
    let agent_tool = agent_run_result.as_ref().map(|r| r.agent_id.tool.clone());
    let checkpoint_result = commands::checkpoint::run(
        &repo,
        &default_user_name,
        checkpoint_kind,
        show_working_log,
        reset,
        false,
        agent_run_result,
        false,
    );
    match checkpoint_result {
        Ok((_, files_edited, _)) => {
            let elapsed = checkpoint_start.elapsed();
            log_performance_for_checkpoint(files_edited, elapsed, checkpoint_kind);
            eprintln!("Checkpoint completed in {:?}", elapsed);

            // Flush logs and metrics after checkpoint (skip for human checkpoints)
            if checkpoint_kind != CheckpointKind::Human {
                observability::spawn_background_flush();
            }
        }
        Err(e) => {
            let elapsed = checkpoint_start.elapsed();
            eprintln!("Checkpoint failed after {:?} with error {}", elapsed, e);
            let context = serde_json::json!({
                "function": "checkpoint",
                "agent": agent_tool.unwrap_or_default(),
                "duration": elapsed.as_millis(),
                "checkpoint_kind": format!("{:?}", checkpoint_kind)
            });
            observability::log_error(&e, Some(context));
            std::process::exit(0);
        }
    }
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
        if let Ok(Some(config_path)) = repo.config_get_str("blame.ignoreRevsFile") {
            if !config_path.is_empty() {
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
        }

        // If still not set, check for .git-blame-ignore-revs in the repository root
        if options.ignore_revs_file.is_none() {
            if let Ok(workdir) = repo.workdir() {
                let ignore_revs_path = workdir.join(".git-blame-ignore-revs");
                if ignore_revs_path.exists() {
                    options.ignore_revs_file = Some(ignore_revs_path.to_string_lossy().to_string());
                }
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
                                parts[0].to_string(),
                                parts[1].to_string(),
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
                        commit_sha = Some(arg.clone());
                    }
                    i += 1;
                } else {
                    eprintln!("Unknown stats argument: {}", args[i]);
                    std::process::exit(1);
                }
            }
        }
    }

    // Handle commit range if detected
    if let Some(range) = commit_range {
        match range_authorship::range_authorship(range, false, &ignore_patterns) {
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

    if let Err(e) = stats_command(&repo, commit_sha.as_deref(), json_output, &ignore_patterns) {
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

fn get_all_files_for_mock_ai(working_dir: &str) -> Vec<String> {
    // Find the git repository
    let repo = match find_repository_in_path(&working_dir) {
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

#[cfg(debug_assertions)]
fn handle_show_transcript(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Error: show-transcript requires agent name and path/id");
        eprintln!("Usage: git-ai show-transcript <agent> <path|id>");
        eprintln!("  Agents: claude, gemini, continue-cli, github-copilot, cursor");
        eprintln!("  For cursor, provide conversation_id instead of path");
        std::process::exit(1);
    }

    let agent_name = &args[0];
    let path_or_id = &args[1];

    let result: Result<
        (crate::authorship::transcript::AiTranscript, Option<String>),
        crate::error::GitAiError,
    > = match agent_name.as_str() {
        "claude" => match ClaudePreset::transcript_and_model_from_claude_code_jsonl(path_or_id) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Claude transcript: {}", e);
                std::process::exit(1);
            }
        },
        "gemini" => match GeminiPreset::transcript_and_model_from_gemini_json(path_or_id) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Gemini transcript: {}", e);
                std::process::exit(1);
            }
        },
        "continue-cli" => match ContinueCliPreset::transcript_from_continue_json(path_or_id) {
            Ok(transcript) => Ok((transcript, None)),
            Err(e) => {
                eprintln!("Error loading Continue CLI transcript: {}", e);
                std::process::exit(1);
            }
        },
        "github-copilot" => {
            match GithubCopilotPreset::transcript_and_model_from_copilot_session_json(path_or_id) {
                Ok((transcript, model, _file_paths)) => Ok((transcript, model)),
                Err(e) => {
                    eprintln!("Error loading GitHub Copilot transcript: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "cursor" => match CursorPreset::fetch_latest_cursor_conversation(path_or_id) {
            Ok(Some((transcript, model))) => Ok((transcript, Some(model))),
            Ok(None) => {
                eprintln!("Error: Conversation not found or database not available");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error loading Cursor transcript: {}", e);
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Error: Unknown agent '{}'", agent_name);
            eprintln!("Supported agents: claude, gemini, continue-cli, github-copilot, cursor");
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
