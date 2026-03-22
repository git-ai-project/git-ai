use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::range_authorship;
use crate::authorship::stats::stats_command;
use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands;
use crate::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, AiTabPreset, ClaudePreset,
    CodexPreset, ContinueCliPreset, CursorPreset, DroidPreset, GeminiPreset, GithubCopilotPreset,
    WindsurfPreset,
};
use crate::commands::checkpoint_agent::agent_v1_preset::AgentV1Preset;
use crate::commands::checkpoint_agent::amp_preset::AmpPreset;
use crate::commands::checkpoint_agent::opencode_preset::OpenCodePreset;
use crate::config;
use crate::daemon::{
    CapturedCheckpointRunRequest, CheckpointRunRequest, ControlRequest, LiveCheckpointRunRequest,
    send_control_request,
};
use crate::git::find_repository;
use crate::git::find_repository_in_path;
use crate::git::repository::{CommitRange, Repository, group_files_by_repository};
use crate::git::sync_authorship::{NotesExistence, fetch_authorship_notes, push_authorship_notes};
use crate::observability::wrapper_performance_targets::log_performance_for_checkpoint;
use crate::observability::{self, log_message};
use crate::utils::is_interactive_terminal;
use serde::{Deserialize, Serialize};
use std::env;
use std::io::IsTerminal;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn handle_git_ai(args: &[String]) {
    if args.is_empty() {
        print_help();
        return;
    }

    // Start DB warmup early for commands that need database access
    match args[0].as_str() {
        "checkpoint" | "show-prompt" | "share" | "sync-prompts" | "flush-cas" | "search"
        | "continue" => {
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
        "daemon" => {
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
        "share" => {
            commands::share::handle_share(&args[1..]);
        }
        "sync-prompts" => {
            commands::sync_prompts::handle_sync_prompts(&args[1..]);
        }
        "prompts" => {
            commands::prompts_db::handle_prompts(&args[1..]);
        }
        "search" => {
            commands::search::handle_search(&args[1..]);
        }
        "continue" => {
            commands::continue_session::handle_continue(&args[1..]);
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
        "    Presets: claude, codex, continue-cli, cursor, gemini, github-copilot, amp, windsurf, opencode, ai_tab, mock_ai"
    );
    eprintln!(
        "    --hook-input <json|stdin>   JSON payload required by presets, or 'stdin' to read from stdin"
    );
    eprintln!("    --reset                     Reset working log");
    eprintln!("    mock_ai [pathspecs...]      Test preset accepting optional file pathspecs");
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
    eprintln!("  debug              Print support/debug diagnostics");
    eprintln!("  daemon             Run and control git-ai daemon mode");
    eprintln!("  install-hooks      Install git hooks for AI authorship tracking");
    eprintln!("  uninstall-hooks    Remove git-ai hooks from all detected tools");
    eprintln!("  git-hooks ensure   Ensure repo-local git-ai hooks are installed/healed");
    eprintln!("  git-hooks remove   Remove repo-local git-ai hooks and restore local hooksPath");
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
    eprintln!("  search             Search AI prompt history");
    eprintln!("    --commit <rev>        Search by commit (SHA, branch, tag, symbolic ref)");
    eprintln!("    --file <path>         Search by file path");
    eprintln!("    --lines <start-end>   Limit to line range (requires --file; repeatable)");
    eprintln!("    --pattern <text>      Full-text search in prompt messages");
    eprintln!("    --prompt-id <id>      Look up specific prompt");
    eprintln!("    --tool <name>         Filter by AI tool (claude, cursor, etc.)");
    eprintln!("    --author <name>       Filter by human author");
    eprintln!("    --since <time>        Only prompts after this time");
    eprintln!("    --until <time>        Only prompts before this time");
    eprintln!("    --json                Output as JSON");
    eprintln!("    --verbose             Include full transcripts");
    eprintln!("    --porcelain           Stable machine-parseable format");
    eprintln!("    --count               Just show result count");
    eprintln!("  continue           Restore AI session context and launch agent");
    eprintln!("    --commit <rev>        Continue from a specific commit");
    eprintln!("    --file <path>         Continue from a specific file");
    eprintln!("    --lines <start-end>   Limit to line range (requires --file)");
    eprintln!("    --prompt-id <id>      Continue from a specific prompt");
    eprintln!("    --agent <name>        Select agent (claude, cursor; default: claude)");
    eprintln!("    --launch              Launch agent CLI with restored context");
    eprintln!("    --clipboard           Copy context to system clipboard");
    eprintln!("    --json                Output context as structured JSON");
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
    let mut reset = false;
    let mut hook_input = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reset" => {
                reset = true;
                i += 1;
            }
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
            "codex" => {
                match CodexPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Codex preset error: {}", e);
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
            "windsurf" => {
                match WindsurfPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Windsurf preset error: {}", e);
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
            "amp" => {
                match AmpPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Amp preset error: {}", e);
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
            "droid" => {
                match DroidPreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("Droid preset error: {}", e);
                        std::process::exit(0);
                    }
                }
            }
            "opencode" => {
                match OpenCodePreset.run(AgentCheckpointFlags {
                    hook_input: hook_input.clone(),
                }) {
                    Ok(agent_run) => {
                        if agent_run.repo_working_dir.is_some() {
                            repository_working_dir = agent_run.repo_working_dir.clone().unwrap();
                        }
                        agent_run_result = Some(agent_run);
                    }
                    Err(e) => {
                        eprintln!("OpenCode preset error: {}", e);
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

        if let Some(files) = files_to_check
            && !files.is_empty()
        {
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
                emit_no_repo_agent_metrics(agent_run_result.as_ref());
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
            let allow_captured_async =
                checkpoint_request_has_explicit_capture_scope(args, agent_run_result.as_ref());

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

                commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(&repo);
                let checkpoint_result = run_checkpoint_via_daemon_or_local(
                    &repo,
                    &default_user_name,
                    checkpoint_kind,
                    reset,
                    false,
                    repo_agent_result,
                    allow_captured_async,
                    false,
                );

                match checkpoint_result {
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
            } else {
                if queued_repos > 0 {
                    eprintln!("Checkpoint queued in {:?}", elapsed);
                } else {
                    eprintln!("Checkpoint completed in {:?}", elapsed);
                }
            }
            return;
        }

        // No files to check, fall through to error
        eprintln!(
            "Failed to find repository: workspace root is not a git repository and no edited files provided"
        );
        emit_no_repo_agent_metrics(agent_run_result.as_ref());
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
    let allow_captured_async =
        checkpoint_request_has_explicit_capture_scope(args, agent_run_result.as_ref());

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

    // Get the current user name
    let default_user_name = repo.git_author_identity().name_or_unknown();

    let checkpoint_start = std::time::Instant::now();
    let agent_tool = agent_run_result.as_ref().map(|r| r.agent_id.tool.clone());

    let external_files: Vec<String> = agent_run_result
        .as_ref()
        .and_then(|r| {
            let paths = if r.checkpoint_kind == CheckpointKind::Human {
                r.will_edit_filepaths.as_ref()
            } else {
                r.edited_filepaths.as_ref()
            };
            paths.map(|p| {
                let repo_workdir = repo.workdir().ok();
                p.iter()
                    .filter_map(|path| {
                        let workdir = repo_workdir.as_ref()?;
                        let path_buf = if std::path::Path::new(path).is_absolute() {
                            std::path::PathBuf::from(path)
                        } else {
                            workdir.join(path)
                        };
                        if repo.path_is_in_workdir(&path_buf) {
                            None
                        } else {
                            let abs = if std::path::Path::new(path).is_absolute() {
                                path.clone()
                            } else {
                                workdir.join(path).to_string_lossy().to_string()
                            };
                            Some(abs)
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();

    let external_agent_base = if !external_files.is_empty() {
        agent_run_result.as_ref().cloned()
    } else {
        None
    };

    commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(&repo);
    let checkpoint_result = run_checkpoint_via_daemon_or_local(
        &repo,
        &default_user_name,
        checkpoint_kind,
        reset,
        false,
        agent_run_result,
        allow_captured_async,
        false,
    );
    let local_checkpoint_failed = checkpoint_result.is_err();
    match checkpoint_result {
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
            modified.repo_working_dir = Some(repo_workdir.to_string_lossy().to_string());
            if base_result.checkpoint_kind == CheckpointKind::Human {
                modified.will_edit_filepaths = Some(repo_file_paths);
                modified.edited_filepaths = None;
            } else {
                modified.edited_filepaths = Some(repo_file_paths);
                modified.will_edit_filepaths = None;
            }

            commands::git_hook_handlers::ensure_repo_level_hooks_for_checkpoint(&ext_repo);
            match run_checkpoint_via_daemon_or_local(
                &ext_repo,
                &ext_user_name,
                checkpoint_kind,
                false,
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

    if checkpoint_kind != CheckpointKind::Human {
        observability::spawn_background_flush();
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
    reset: bool,
    quiet: bool,
    agent_run_result: Option<AgentRunResult>,
    allow_captured_async: bool,
    is_pre_commit: bool,
) -> Result<CheckpointDispatchOutcome, crate::error::GitAiError> {
    if daemon_checkpoint_delegate_enabled() {
        let repo_working_dir = repo.workdir().map(|p| p.to_string_lossy().to_string()).ok();
        if let Some(repo_working_dir) = repo_working_dir {
            match crate::commands::daemon::ensure_daemon_running(Duration::from_secs(10)) {
                Ok(config) => {
                    if allow_captured_async
                        && crate::commands::checkpoint::explicit_capture_target_paths(
                            kind,
                            agent_run_result.as_ref(),
                        )
                        .is_some()
                    {
                        match crate::commands::checkpoint::prepare_captured_checkpoint(
                            repo,
                            author,
                            kind,
                            reset,
                            agent_run_result.as_ref(),
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
                        request: Box::new(CheckpointRunRequest::Live(LiveCheckpointRunRequest {
                            repo_working_dir: repo_working_dir.clone(),
                            kind: Some(checkpoint_kind_to_str(kind).to_string()),
                            author: Some(author.to_string()),
                            reset: Some(reset),
                            quiet: Some(quiet),
                            is_pre_commit: Some(is_pre_commit),
                            agent_run_result: agent_run_result.clone(),
                        })),
                        wait: Some(true),
                    };
                    match send_control_request(&config.control_socket_path, &request) {
                        Ok(response) if response.ok => {
                            let estimated_files =
                                estimate_checkpoint_file_count(kind, &agent_run_result);
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
    let stats = commands::checkpoint::run(
        repo,
        author,
        kind,
        reset,
        quiet,
        agent_run_result,
        is_pre_commit,
    )?;
    Ok(CheckpointDispatchOutcome {
        stats,
        queued: false,
    })
}

fn checkpoint_request_has_explicit_capture_scope(
    args: &[String],
    agent_run_result: Option<&AgentRunResult>,
) -> bool {
    if args.first().map(String::as_str) == Some("mock_ai") {
        return args.iter().skip(1).any(|arg| !arg.starts_with("--"));
    }

    if let Some(separator_pos) = args.iter().position(|arg| arg == "--") {
        return args[separator_pos + 1..]
            .iter()
            .any(|arg| !arg.starts_with("--"));
    }

    agent_run_result
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
        "[git-ai] daemon checkpoint delegate {}: {}; falling back to local checkpoint",
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
    if config::Config::get().feature_flags().async_mode {
        return true;
    }

    matches!(
        std::env::var("GIT_AI_DAEMON_CHECKPOINT_DELEGATE")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn checkpoint_kind_to_str(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Human => "human",
        CheckpointKind::AiAgent => "ai_agent",
        CheckpointKind::AiTab => "ai_tab",
    }
}

fn estimate_checkpoint_file_count(
    kind: CheckpointKind,
    agent_run_result: &Option<AgentRunResult>,
) -> usize {
    match (kind, agent_run_result) {
        (CheckpointKind::Human, Some(result)) => result
            .will_edit_filepaths
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0),
        (_, Some(result)) => result
            .edited_filepaths
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0),
        _ => 0,
    }
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

fn handle_git_hooks(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("ensure") => {
            let repo = match find_repository(&Vec::<String>::new()) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to find repository: {}", e);
                    std::process::exit(1);
                }
            };

            match commands::git_hook_handlers::ensure_repo_hooks_installed(&repo, false) {
                Ok(report) => {
                    if let Err(e) = commands::git_hook_handlers::mark_repo_hooks_enabled(&repo) {
                        eprintln!("Failed to persist repo hook opt-in: {}", e);
                        std::process::exit(1);
                    }
                    let status = if report.changed { "updated" } else { "ok" };
                    println!(
                        "repo hooks {}: {}",
                        status,
                        report.managed_hooks_path.to_string_lossy()
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Failed to ensure repo hooks: {}", e);
                    std::process::exit(1);
                }
            }
        }
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
            eprintln!("Usage: git-ai git-hooks <ensure|remove>");
            std::process::exit(1);
        }
    }
}

fn emit_no_repo_agent_metrics(agent_run_result: Option<&AgentRunResult>) {
    let Some(result) = agent_run_result else {
        return;
    };
    if result.checkpoint_kind == CheckpointKind::Human {
        return;
    }

    let agent_id = &result.agent_id;
    if !commands::checkpoint::should_emit_agent_usage(agent_id) {
        return;
    }

    let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
    let attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .tool(&agent_id.tool)
        .model(&agent_id.model)
        .prompt_id(prompt_id)
        .external_prompt_id(&agent_id.id)
        .custom_attributes_map(crate::config::Config::get().custom_attributes());

    let values = crate::metrics::AgentUsageValues::new();
    crate::metrics::record(values, attrs);

    observability::spawn_background_flush();
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

#[cfg(debug_assertions)]
fn handle_show_transcript(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Error: show-transcript requires agent name and path/id");
        eprintln!("Usage: git-ai show-transcript <agent> <path|id>");
        eprintln!(
            "  Agents: claude, codex, gemini, continue-cli, github-copilot, cursor, amp, windsurf"
        );
        eprintln!("  For cursor and amp, provide conversation/thread id instead of path");
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
        "codex" => match CodexPreset::transcript_and_model_from_codex_rollout_jsonl(path_or_id) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Codex transcript: {}", e);
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
        "windsurf" => match WindsurfPreset::transcript_and_model_from_windsurf_jsonl(path_or_id) {
            Ok((transcript, model)) => Ok((transcript, model)),
            Err(e) => {
                eprintln!("Error loading Windsurf transcript: {}", e);
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
        "amp" => {
            let path = std::path::Path::new(path_or_id);
            let amp_result = if path.exists() {
                AmpPreset::transcript_and_model_from_thread_path(path)
                    .map(|(transcript, model, _thread_id)| (transcript, model))
            } else {
                AmpPreset::transcript_and_model_from_thread_id(path_or_id)
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
