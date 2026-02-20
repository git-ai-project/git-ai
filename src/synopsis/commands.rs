use crate::git::find_repository;
use crate::synopsis::collector::collect_input;
use crate::synopsis::config::{ConversationSourceKind, SynopsisConfig, TargetLength};
use crate::synopsis::generator::{build_synopsis_prompt, estimate_input_tokens, generate_synopsis};
use crate::synopsis::storage::{list_synopses, retrieve_synopsis, store_synopsis};
use crate::synopsis::types::{Synopsis, SynopsisMetadata};
use chrono::Utc;

/// Main entry point for the `git-ai synopsis` subcommand.
pub fn handle_synopsis(args: &[String]) {
    if args.is_empty() {
        print_synopsis_help();
        return;
    }

    match args[0].as_str() {
        "generate" => handle_generate(&args[1..]),
        "show" => handle_show(&args[1..]),
        "list" => handle_list(&args[1..]),
        "help" | "--help" | "-h" => print_synopsis_help(),
        unknown => {
            eprintln!("Unknown synopsis subcommand: {}", unknown);
            print_synopsis_help();
            std::process::exit(1);
        }
    }
}

/// Parse common flags shared between subcommands.
struct CommonFlags {
    commit: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    notes_ref: Option<String>,
    conversation: Option<String>,
    no_conversation: bool,
    length: Option<String>,
    dry_run: bool,
}

impl CommonFlags {
    fn parse(args: &[String]) -> (Self, Vec<String>) {
        let mut commit = None;
        let mut model = None;
        let mut api_key = None;
        let mut notes_ref = None;
        let mut conversation = None;
        let mut no_conversation = false;
        let mut length = None;
        let mut dry_run = false;
        let mut remaining = Vec::new();

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--commit" | "-c" if i + 1 < args.len() => {
                    commit = Some(args[i + 1].clone());
                    i += 2;
                }
                "--model" | "-m" if i + 1 < args.len() => {
                    model = Some(args[i + 1].clone());
                    i += 2;
                }
                "--api-key" if i + 1 < args.len() => {
                    api_key = Some(args[i + 1].clone());
                    i += 2;
                }
                "--notes-ref" if i + 1 < args.len() => {
                    notes_ref = Some(args[i + 1].clone());
                    i += 2;
                }
                "--conversation" if i + 1 < args.len() => {
                    conversation = Some(args[i + 1].clone());
                    i += 2;
                }
                "--no-conversation" => {
                    no_conversation = true;
                    i += 1;
                }
                "--length" if i + 1 < args.len() => {
                    length = Some(args[i + 1].clone());
                    i += 2;
                }
                "--dry-run" => {
                    dry_run = true;
                    i += 1;
                }
                _ => {
                    remaining.push(args[i].clone());
                    i += 1;
                }
            }
        }

        (
            CommonFlags {
                commit,
                model,
                api_key,
                notes_ref,
                conversation,
                no_conversation,
                length,
                dry_run,
            },
            remaining,
        )
    }
}

fn handle_generate(args: &[String]) {
    let (flags, _remaining) = CommonFlags::parse(args);

    // Build configuration
    let mut config = SynopsisConfig::default();

    if let Some(model) = flags.model {
        config.model = model;
    }
    if let Some(key) = flags.api_key {
        config.api_key = Some(key);
    }
    if let Some(ref_name) = flags.notes_ref {
        config.notes_ref = ref_name;
    }
    if flags.no_conversation {
        config.conversation_source = ConversationSourceKind::None;
    }
    if let Some(ref conv_path) = flags.conversation {
        config.conversation_path = Some(conv_path.clone());
    }
    if let Some(ref length_str) = flags.length {
        config.target_length = match length_str.as_str() {
            "brief" => TargetLength::Brief,
            "detailed" => TargetLength::Detailed,
            _ => TargetLength::Standard,
        };
    }

    // Check API key before doing any expensive work (but allow dry-run without a key)
    if config.api_key.is_none() && !flags.dry_run {
        eprintln!("Error: No API key found. Set ANTHROPIC_API_KEY or use --api-key <key>.");
        std::process::exit(1);
    }

    // Find the repository
    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to find git repository: {}", e);
            std::process::exit(1);
        }
    };

    // Resolve commit SHA
    let commit_sha = resolve_commit_sha(flags.commit.as_deref(), &repo);

    eprintln!(
        "[synopsis] Collecting inputs for commit {}...",
        &commit_sha[..8.min(commit_sha.len())]
    );

    let input = match collect_input(&repo, &commit_sha, &config, flags.conversation.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to collect synopsis inputs: {}", e);
            std::process::exit(1);
        }
    };

    if flags.dry_run {
        let prompt = build_synopsis_prompt(&input, &config);
        let tokens = estimate_input_tokens(&prompt);
        println!(
            "--- Dry run: synopsis prompt ({} estimated tokens) ---",
            tokens
        );
        println!("{}", prompt);
        return;
    }

    eprintln!(
        "[synopsis] Generating synopsis using model {}...",
        config.model
    );

    let content = match generate_synopsis(&input, &config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to generate synopsis: {}", e);
            std::process::exit(1);
        }
    };

    let word_count = content.split_whitespace().count();
    let prompt = build_synopsis_prompt(&input, &config);
    let input_tokens_estimate = estimate_input_tokens(&prompt);

    let metadata = SynopsisMetadata {
        commit_sha: commit_sha.clone(),
        date: Utc::now(),
        author: input.author.clone(),
        model: config.model.clone(),
        version: 1,
        word_count,
        input_tokens_estimate,
        conversation_source: input.conversation.as_ref().map(|c| c.source_kind.clone()),
        conversation_window_secs: input
            .conversation
            .as_ref()
            .map(|_| config.conversation_window_minutes * 60),
        files_changed: input.diff.files_changed,
    };

    let synopsis = Synopsis {
        metadata,
        content: content.clone(),
    };

    // Store as a git note
    match store_synopsis(&repo, &synopsis, &config.notes_ref) {
        Ok(()) => {
            eprintln!(
                "[synopsis] Stored under refs/notes/{} for commit {}.",
                config.notes_ref,
                &commit_sha[..8.min(commit_sha.len())]
            );
        }
        Err(e) => {
            eprintln!(
                "[synopsis] Warning: Failed to store synopsis as git note: {}",
                e
            );
        }
    }

    // Print the synopsis to stdout
    println!("{}", content);
    eprintln!(
        "\n[synopsis] {} words, ~{} input tokens.",
        word_count, input_tokens_estimate
    );
}

fn handle_show(args: &[String]) {
    let (flags, remaining) = CommonFlags::parse(args);

    // Commit can be given positionally or via --commit
    let commit_spec = flags
        .commit
        .or_else(|| remaining.first().cloned())
        .unwrap_or_else(|| "HEAD".to_string());

    let notes_ref = flags.notes_ref.unwrap_or_else(|| "ai-synopsis".to_string());

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to find git repository: {}", e);
            std::process::exit(1);
        }
    };

    let commit_sha = resolve_commit_sha(Some(&commit_spec), &repo);

    match retrieve_synopsis(&repo, &commit_sha, &notes_ref) {
        Ok(Some(synopsis)) => {
            println!("{}", synopsis.content);
            eprintln!(
                "\n[synopsis] Generated {} | model: {} | {} words",
                synopsis.metadata.date.format("%Y-%m-%d %H:%M UTC"),
                synopsis.metadata.model,
                synopsis.metadata.word_count
            );
        }
        Ok(None) => {
            eprintln!(
                "No synopsis found for commit {}. Run `git-ai synopsis generate` to create one.",
                &commit_sha[..8.min(commit_sha.len())]
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to retrieve synopsis: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_list(args: &[String]) {
    let (flags, _remaining) = CommonFlags::parse(args);

    let notes_ref = flags.notes_ref.unwrap_or_else(|| "ai-synopsis".to_string());

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to find git repository: {}", e);
            std::process::exit(1);
        }
    };

    match list_synopses(&repo, &notes_ref) {
        Ok(shas) if shas.is_empty() => {
            eprintln!("No synopses found. Run `git-ai synopsis generate` to create one.");
        }
        Ok(shas) => {
            println!(
                "Synopses stored under refs/notes/{} ({} total):",
                notes_ref,
                shas.len()
            );
            for sha in &shas {
                println!("  {}", sha);
            }
        }
        Err(e) => {
            eprintln!("Failed to list synopses: {}", e);
            std::process::exit(1);
        }
    }
}

/// Resolve a commit specifier (e.g. "HEAD", a branch name, a partial SHA) to a
/// full SHA. Exits the process on failure.
fn resolve_commit_sha(
    commit_spec: Option<&str>,
    repo: &crate::git::repository::Repository,
) -> String {
    let spec = commit_spec.unwrap_or("HEAD");
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push("--verify".to_string());
    args.push(spec.to_string());

    match crate::git::repository::exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout)
            .unwrap_or_default()
            .trim()
            .to_string(),
        Err(e) => {
            eprintln!("Failed to resolve commit '{}': {}", spec, e);
            std::process::exit(1);
        }
    }
}

fn print_synopsis_help() {
    eprintln!("git-ai synopsis - Generate AI-powered narrative synopses for commits");
    eprintln!();
    eprintln!("Usage: git-ai synopsis <subcommand> [options]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  generate            Generate a synopsis for a commit");
    eprintln!("    --commit <sha>      Commit to generate synopsis for (default: HEAD)");
    eprintln!("    --model <name>      Claude model to use (default: claude-opus-4-6)");
    eprintln!("    --api-key <key>     Anthropic API key (default: ANTHROPIC_API_KEY env)");
    eprintln!(
        "    --length <level>    Target length: brief, standard, detailed (default: standard)"
    );
    eprintln!("    --conversation <path>  Path to a Claude Code JSONL conversation file");
    eprintln!("    --no-conversation   Do not include conversation context");
    eprintln!("    --notes-ref <ref>   Git notes ref (default: ai-synopsis)");
    eprintln!("    --dry-run           Print the prompt without calling the API");
    eprintln!();
    eprintln!("  show [<commit>]     Show the synopsis for a commit (default: HEAD)");
    eprintln!("    --commit <sha>      Commit to show (alternative to positional argument)");
    eprintln!("    --notes-ref <ref>   Git notes ref (default: ai-synopsis)");
    eprintln!();
    eprintln!("  list                List all commits that have synopses");
    eprintln!("    --notes-ref <ref>   Git notes ref (default: ai-synopsis)");
    eprintln!();
    eprintln!("Environment variables:");
    eprintln!("  ANTHROPIC_API_KEY           Anthropic API key");
    eprintln!("  GIT_AI_SYNOPSIS_API_KEY     Alternative API key variable");
    eprintln!("  GIT_AI_SYNOPSIS_MODEL       Default model override");
    eprintln!("  GIT_AI_SYNOPSIS             Set to '1' or 'true' to enable auto-generation");
}
