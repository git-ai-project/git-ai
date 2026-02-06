//! Continue command for git-ai
//!
//! Provides `git-ai continue` functionality to restore AI session context
//! and launch agents with pre-loaded conversation history.

use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::secrets::redact_secrets_from_prompts;
use crate::authorship::transcript::Message;
use crate::commands::prompt_picker;
use crate::commands::search::{
    search_by_commit, search_by_commit_range, search_by_file, search_by_pattern,
    search_by_prompt_id, SearchResult,
};
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repository::{exec_git, Repository};
use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Continue mode determined by CLI arguments
#[derive(Debug, Clone, PartialEq)]
pub enum ContinueMode {
    /// Continue from a specific commit
    ByCommit { commit_rev: String },
    /// Continue from a range of commits
    ByCommitRange { start: String, end: String },
    /// Continue from a specific file, optionally with line ranges
    ByFile {
        file_path: String,
        line_ranges: Vec<(u32, u32)>,
    },
    /// Continue from prompts matching a pattern
    ByPattern { query: String },
    /// Continue from a specific prompt ID
    ByPromptId { prompt_id: String },
    /// Interactive TUI mode (no args)
    Interactive,
}

/// Options for the continue command
#[derive(Debug, Clone, Default)]
pub struct ContinueOptions {
    /// Which agent CLI to target (e.g., "claude", "cursor")
    pub agent: Option<String>,
    /// Whether to spawn the agent CLI directly
    pub launch: bool,
    /// Whether to copy context to clipboard
    pub clipboard: bool,
    /// Whether to output structured JSON
    pub json: bool,
    /// Limit on messages to include in context per prompt
    pub max_messages: Option<usize>,
}

impl ContinueOptions {
    /// Create new default options
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the agent name, defaulting to "claude"
    pub fn agent_name(&self) -> &str {
        self.agent.as_deref().unwrap_or("claude")
    }
}

/// Commit metadata for the context block
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

impl CommitInfo {
    /// Create CommitInfo from a commit SHA by querying git
    pub fn from_commit_sha(repo: &Repository, sha: &str) -> Result<Self, GitAiError> {
        let mut args = repo.global_args_for_exec();
        args.push("log".to_string());
        args.push("-1".to_string());
        args.push("--format=%H|||%an|||%ai|||%s".to_string());
        args.push(sha.to_string());

        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| GitAiError::Generic(format!("Invalid UTF-8 in git output: {}", e)))?;

        let parts: Vec<&str> = stdout.trim().split("|||").collect();
        if parts.len() < 4 {
            return Err(GitAiError::Generic(format!(
                "Failed to parse commit info for {}",
                sha
            )));
        }

        Ok(CommitInfo {
            sha: parts[0].to_string(),
            author: parts[1].to_string(),
            date: parts[2].to_string(),
            message: parts[3].to_string(),
        })
    }
}

/// Agent output choice for TUI mode
#[derive(Debug, Clone, PartialEq)]
enum AgentChoice {
    /// Launch the specified agent CLI
    Launch(String),
    /// Output to stdout
    Stdout,
    /// Copy to clipboard
    Clipboard,
}

/// Parse agent choice input string
fn parse_agent_choice_input(input: &str) -> Result<AgentChoice, GitAiError> {
    match input.trim() {
        "" | "1" => Ok(AgentChoice::Launch("claude".to_string())),
        "2" => Ok(AgentChoice::Stdout),
        "3" => Ok(AgentChoice::Clipboard),
        other => Err(GitAiError::Generic(format!("Invalid choice: {}", other))),
    }
}

/// Prompt user to select an output mode
fn prompt_agent_choice(prompt_snippet: &str) -> Result<AgentChoice, GitAiError> {
    eprintln!("\nSelected prompt: {}", prompt_snippet);
    eprintln!("\nLaunch with which agent?");
    eprintln!("  [1] Claude Code (default)");
    eprintln!("  [2] Output to stdout");
    eprintln!("  [3] Copy to clipboard");
    eprint!("\nChoice [1]: ");

    // Flush stderr to ensure prompt is visible
    std::io::stderr().flush().ok();

    let mut input = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|e| GitAiError::Generic(format!("Failed to read input: {}", e)))?;

    parse_agent_choice_input(&input)
}

/// Handle interactive TUI mode for continue command
fn handle_continue_tui(repo: &Repository) {
    // Check if terminal is interactive
    if !std::io::stdout().is_terminal() {
        eprintln!("TUI mode requires an interactive terminal.");
        eprintln!("Use --commit, --file, or --prompt-id flags instead.");
        std::process::exit(1);
    }

    // Launch the prompt picker
    let selected = match prompt_picker::pick_prompt(Some(repo), "Select a prompt to continue") {
        Ok(Some(db_record)) => db_record,
        Ok(None) => {
            // User cancelled
            return;
        }
        Err(e) => {
            eprintln!("Error launching prompt picker: {}", e);
            std::process::exit(1);
        }
    };

    // Get snippet for display
    let snippet = selected.first_message_snippet(80);

    // Prompt for agent choice
    let choice = match prompt_agent_choice(&snippet) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Convert PromptDbRecord to SearchResult
    let prompt_record = selected.to_prompt_record();
    let mut result = SearchResult::new();
    result.prompts.insert(selected.id.clone(), prompt_record);

    // Convert to BTreeMap for ordered iteration
    let mut prompts: BTreeMap<String, PromptRecord> = result.prompts.into_iter().collect();

    // Apply secret redaction
    let redaction_count = redact_secrets_from_prompts(&mut prompts);
    if redaction_count > 0 {
        eprintln!("Redacted {} potential secret(s) from output", redaction_count);
    }

    // Format context block
    let context = format_context_block(&prompts, None, 50);

    // Execute the chosen action
    match choice {
        AgentChoice::Launch(agent) => {
            match launch_agent(&agent, &context) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Error launching agent: {}", e);
                    eprintln!("Printing context to stdout instead:");
                    println!("{}", context);
                }
            }
        }
        AgentChoice::Stdout => {
            println!("{}", context);
        }
        AgentChoice::Clipboard => {
            match copy_to_clipboard(&context) {
                Ok(()) => {
                    eprintln!("Context copied to clipboard ({} characters)", context.len());
                }
                Err(e) => {
                    eprintln!("Error copying to clipboard: {}", e);
                    eprintln!("Printing context to stdout instead:");
                    println!("{}", context);
                }
            }
        }
    }
}

/// Handle the `git-ai continue` command
pub fn handle_continue(args: &[String]) {
    let parsed = match parse_continue_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("Error: {}", e);
            print_continue_help();
            std::process::exit(1);
        }
    };

    // Check for help flag
    if parsed.help {
        print_continue_help();
        std::process::exit(0);
    }

    // Find repository (needed for all modes)
    let current_dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Error getting current directory: {}", e);
            std::process::exit(1);
        }
    };

    let repo = match find_repository_in_path(&current_dir.to_string_lossy()) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Error finding repository: {}", e);
            std::process::exit(1);
        }
    };

    // Check for interactive mode
    if parsed.mode == ContinueMode::Interactive {
        handle_continue_tui(&repo);
        return;
    }

    // Execute search based on mode
    let (result, commit_info) = match &parsed.mode {
        ContinueMode::ByCommit { commit_rev } => {
            let commit_info = CommitInfo::from_commit_sha(&repo, commit_rev).ok();
            match search_by_commit(&repo, commit_rev) {
                Ok(r) => (r, commit_info),
                Err(e) => {
                    eprintln!("Error searching commit '{}': {}", commit_rev, e);
                    std::process::exit(1);
                }
            }
        }
        ContinueMode::ByCommitRange { start, end } => {
            match search_by_commit_range(&repo, start, end) {
                Ok(r) => (r, None),
                Err(e) => {
                    eprintln!("Error searching commit range '{}..{}': {}", start, end, e);
                    std::process::exit(1);
                }
            }
        }
        ContinueMode::ByFile {
            file_path,
            line_ranges,
        } => match search_by_file(&repo, file_path, line_ranges) {
            Ok(r) => (r, None),
            Err(e) => {
                eprintln!("Error searching file '{}': {}", file_path, e);
                std::process::exit(1);
            }
        },
        ContinueMode::ByPattern { query } => match search_by_pattern(query) {
            Ok(r) => (r, None),
            Err(e) => {
                eprintln!("Error searching pattern '{}': {}", query, e);
                std::process::exit(1);
            }
        },
        ContinueMode::ByPromptId { prompt_id } => match search_by_prompt_id(&repo, prompt_id) {
            Ok(r) => (r, None),
            Err(e) => {
                eprintln!("Error searching prompt ID '{}': {}", prompt_id, e);
                std::process::exit(1);
            }
        },
        ContinueMode::Interactive => unreachable!(), // Handled above
    };

    // Check for empty results
    if result.prompts.is_empty() {
        eprintln!("No AI prompt history found for the specified context.");
        std::process::exit(2);
    }

    // Convert to BTreeMap for ordered iteration
    let mut prompts: BTreeMap<String, PromptRecord> = result.prompts.into_iter().collect();

    // Apply secret redaction
    let redaction_count = redact_secrets_from_prompts(&mut prompts);
    if redaction_count > 0 {
        eprintln!("Redacted {} potential secret(s) from output", redaction_count);
    }

    // Determine max messages (default 50)
    let max_messages = parsed.options.max_messages.unwrap_or(50);

    // Format output
    let output = if parsed.options.json {
        format_context_json(&prompts, commit_info.as_ref())
    } else {
        format_context_block(&prompts, commit_info.as_ref(), max_messages)
    };

    // Handle output mode (precedence: clipboard > launch/stdout)
    // Default behavior: launch agent if stdout is a terminal, otherwise print to stdout.
    // The --launch flag is accepted but is the default for interactive terminals.
    if parsed.options.clipboard {
        match copy_to_clipboard(&output) {
            Ok(()) => {
                eprintln!(
                    "Context copied to clipboard ({} characters)",
                    output.len()
                );
            }
            Err(e) => {
                eprintln!("Error copying to clipboard: {}", e);
                eprintln!("Printing context to stdout instead:");
                println!("{}", output);
            }
        }
    } else if parsed.options.launch || std::io::stdout().is_terminal() {
        // Launch agent by default when output is a terminal
        match launch_agent(parsed.options.agent_name(), &output) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error launching agent: {}", e);
                eprintln!("Printing context to stdout instead:");
                println!("{}", output);
            }
        }
    } else {
        // Non-terminal (piped) output: print to stdout
        println!("{}", output);
    }
}

/// Check if a CLI tool is available on the system
fn is_cli_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Launch an agent CLI interactively with the context as the initial prompt
fn launch_agent(agent: &str, context: &str) -> Result<(), GitAiError> {
    match agent {
        "claude" => {
            // Check if claude CLI is available
            if !is_cli_available("claude") {
                return Err(GitAiError::Generic(
                    "Claude CLI not found. Install it with: npm install -g @anthropic-ai/claude-code"
                        .to_string(),
                ));
            }

            // Replace this process with claude using exec(). This ensures claude
            // is the direct child of the shell, so terminal/interactive detection
            // works correctly (spawning as a subprocess causes claude to run in
            // non-interactive print mode).
            #[cfg(unix)]
            {
                let err = Command::new("claude")
                    .arg("--append-system-prompt")
                    .arg(context)
                    .exec();
                // exec() only returns if it failed
                return Err(GitAiError::Generic(format!("Failed to exec claude: {}", err)));
            }

            #[cfg(not(unix))]
            {
                let status = Command::new("claude")
                    .arg("--append-system-prompt")
                    .arg(context)
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
                    .map_err(|e| GitAiError::Generic(format!("Failed to spawn claude: {}", e)))?;

                if !status.success() {
                    return Err(GitAiError::Generic(format!(
                        "Claude exited with status: {}",
                        status
                    )));
                }

                Ok(())
            }
        }
        _ => {
            // For other agents, fall back to stdout with a message
            eprintln!(
                "Agent '{}' does not support direct launch. Use --clipboard to copy context.",
                agent
            );
            println!("{}", context);
            Ok(())
        }
    }
}

/// Copy text to the system clipboard
fn copy_to_clipboard(text: &str) -> Result<(), GitAiError> {
    let result = copy_to_clipboard_platform(text);

    if result.is_err() {
        // Fallback: try common clipboard tools
        if let Ok(()) = try_clipboard_fallback(text) {
            return Ok(());
        }
    }

    result
}

#[cfg(target_os = "macos")]
fn copy_to_clipboard_platform(text: &str) -> Result<(), GitAiError> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| GitAiError::Generic(format!("Failed to spawn pbcopy: {}", e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| GitAiError::Generic(format!("Failed to write to pbcopy: {}", e)))?;
    }

    let status = child
        .wait()
        .map_err(|e| GitAiError::Generic(format!("Failed to wait for pbcopy: {}", e)))?;

    if status.success() {
        Ok(())
    } else {
        Err(GitAiError::Generic("pbcopy failed".to_string()))
    }
}

#[cfg(target_os = "linux")]
fn copy_to_clipboard_platform(text: &str) -> Result<(), GitAiError> {
    // Try xclip first, then xsel
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
        .or_else(|_| {
            Command::new("xsel")
                .args(["--clipboard", "--input"])
                .stdin(Stdio::piped())
                .spawn()
        })
        .map_err(|e| {
            GitAiError::Generic(format!(
                "No clipboard tool available (xclip or xsel required): {}",
                e
            ))
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| GitAiError::Generic(format!("Failed to write to clipboard: {}", e)))?;
    }

    let status = child
        .wait()
        .map_err(|e| GitAiError::Generic(format!("Failed to wait for clipboard command: {}", e)))?;

    if status.success() {
        Ok(())
    } else {
        Err(GitAiError::Generic("Clipboard command failed".to_string()))
    }
}

#[cfg(target_os = "windows")]
fn copy_to_clipboard_platform(text: &str) -> Result<(), GitAiError> {
    let mut child = Command::new("clip")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| GitAiError::Generic(format!("Failed to spawn clip: {}", e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| GitAiError::Generic(format!("Failed to write to clip: {}", e)))?;
    }

    let status = child
        .wait()
        .map_err(|e| GitAiError::Generic(format!("Failed to wait for clip: {}", e)))?;

    if status.success() {
        Ok(())
    } else {
        Err(GitAiError::Generic("clip failed".to_string()))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn copy_to_clipboard_platform(_text: &str) -> Result<(), GitAiError> {
    Err(GitAiError::Generic(
        "Clipboard not supported on this platform".to_string(),
    ))
}

/// Fallback clipboard method for when platform-specific method fails
fn try_clipboard_fallback(text: &str) -> Result<(), GitAiError> {
    // Try common clipboard tools in order
    let tools = [
        ("pbcopy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
        ("clip", vec![]),
    ];

    for (tool, args) in tools {
        if let Ok(mut child) = Command::new(tool)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                if stdin.write_all(text.as_bytes()).is_ok() {
                    if let Ok(status) = child.wait() {
                        if status.success() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    Err(GitAiError::Generic(
        "No clipboard tool available".to_string(),
    ))
}

/// Parsed continue arguments
#[derive(Debug)]
struct ParsedContinueArgs {
    mode: ContinueMode,
    options: ContinueOptions,
    help: bool,
}

/// Parse command-line arguments for continue
fn parse_continue_args(args: &[String]) -> Result<ParsedContinueArgs, String> {
    let mut mode: Option<ContinueMode> = None;
    let mut options = ContinueOptions::new();
    let mut help = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                help = true;
            }
            "--commit" => {
                i += 1;
                if i >= args.len() {
                    return Err("--commit requires a value".to_string());
                }
                let commit_rev = args[i].clone();
                // Check for range syntax (sha1..sha2)
                if let Some(pos) = commit_rev.find("..") {
                    let start = commit_rev[..pos].to_string();
                    let end = commit_rev[pos + 2..].to_string();
                    mode = Some(ContinueMode::ByCommitRange { start, end });
                } else {
                    mode = Some(ContinueMode::ByCommit { commit_rev });
                }
            }
            "--file" => {
                i += 1;
                if i >= args.len() {
                    return Err("--file requires a value".to_string());
                }
                let file_path = args[i].clone();
                if let Some(ContinueMode::ByFile { line_ranges, .. }) = &mode {
                    mode = Some(ContinueMode::ByFile {
                        file_path,
                        line_ranges: line_ranges.clone(),
                    });
                } else {
                    mode = Some(ContinueMode::ByFile {
                        file_path,
                        line_ranges: vec![],
                    });
                }
            }
            "--lines" => {
                i += 1;
                if i >= args.len() {
                    return Err("--lines requires a value".to_string());
                }
                let range_str = &args[i];
                let range = parse_line_range(range_str)?;

                match &mut mode {
                    Some(ContinueMode::ByFile { line_ranges, .. }) => {
                        line_ranges.push(range);
                    }
                    _ => {
                        return Err("--lines requires --file to be specified first".to_string());
                    }
                }
            }
            "--pattern" => {
                i += 1;
                if i >= args.len() {
                    return Err("--pattern requires a value".to_string());
                }
                mode = Some(ContinueMode::ByPattern {
                    query: args[i].clone(),
                });
            }
            "--prompt-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("--prompt-id requires a value".to_string());
                }
                mode = Some(ContinueMode::ByPromptId {
                    prompt_id: args[i].clone(),
                });
            }
            // Agent selection
            "--agent" | "--tool" => {
                i += 1;
                if i >= args.len() {
                    return Err(format!("{} requires a value", args[i - 1]));
                }
                options.agent = Some(args[i].to_lowercase());
            }
            // Output modes
            "--launch" => {
                options.launch = true;
            }
            "--clipboard" => {
                options.clipboard = true;
            }
            "--json" => {
                options.json = true;
            }
            // Options
            "--max-messages" => {
                i += 1;
                if i >= args.len() {
                    return Err("--max-messages requires a value".to_string());
                }
                let max: usize = args[i]
                    .parse()
                    .map_err(|_| format!("Invalid number: {}", args[i]))?;
                options.max_messages = Some(max);
            }
            arg => {
                return Err(format!("Unknown argument: {}", arg));
            }
        }
        i += 1;
    }

    // Default to interactive mode if no mode specified
    let mode = mode.unwrap_or(ContinueMode::Interactive);

    Ok(ParsedContinueArgs {
        mode,
        options,
        help,
    })
}

/// Parse a line range specification (e.g., "10", "10-50")
/// Format prompts as a structured markdown context block
fn format_context_block(
    prompts: &BTreeMap<String, PromptRecord>,
    commit_info: Option<&CommitInfo>,
    max_messages: usize,
) -> String {
    let mut output = String::with_capacity(8192);

    // Preamble
    output.push_str("# Restored AI Session Context\n\n");
    output.push_str(
        "This context was restored from git-ai prompt history. \
         It contains the AI conversation(s) associated with the specified code changes.\n\n",
    );

    // Source section (if commit info available)
    if let Some(info) = commit_info {
        output.push_str("## Source\n");
        output.push_str(&format!(
            "- **Commit**: {} - \"{}\" ({}, {})\n\n",
            &info.sha[..8.min(info.sha.len())],
            info.message,
            info.author,
            info.date
        ));
    }

    output.push_str("---\n\n");

    // Session sections
    let total_sessions = prompts.len();
    for (idx, (prompt_id, prompt)) in prompts.iter().enumerate() {
        let session_num = idx + 1;

        output.push_str(&format!(
            "## Session {} of {}: Prompt {}\n",
            session_num,
            total_sessions,
            &prompt_id[..8.min(prompt_id.len())]
        ));
        output.push_str(&format!(
            "- **Tool**: {} ({})\n",
            prompt.agent_id.tool, prompt.agent_id.model
        ));
        if let Some(ref author) = prompt.human_author {
            output.push_str(&format!("- **Author**: {}\n", author));
        }
        output.push_str("\n### Conversation\n\n");

        // Filter out ToolUse and apply truncation
        let non_tool_messages: Vec<&Message> = prompt
            .messages
            .iter()
            .filter(|m| !matches!(m, Message::ToolUse { .. }))
            .collect();

        let (messages_to_show, omitted) = if max_messages > 0 && non_tool_messages.len() > max_messages
        {
            let omitted = non_tool_messages.len() - max_messages;
            let slice = &non_tool_messages[omitted..];
            (slice.to_vec(), Some(omitted))
        } else {
            (non_tool_messages, None)
        };

        // Show truncation notice if applicable
        if let Some(n) = omitted {
            output.push_str(&format!("[... {} earlier messages omitted]\n\n", n));
        }

        // Format messages
        for message in messages_to_show {
            match message {
                Message::User { text, .. } => {
                    output.push_str("**User**:\n");
                    output.push_str(text);
                    output.push_str("\n\n");
                }
                Message::Assistant { text, .. } => {
                    output.push_str("**Assistant**:\n");
                    output.push_str(text);
                    output.push_str("\n\n");
                }
                Message::Thinking { text, .. } => {
                    output.push_str("**[Thinking]**:\n");
                    output.push_str(text);
                    output.push_str("\n\n");
                }
                Message::Plan { text, .. } => {
                    output.push_str("**[Plan]**:\n");
                    output.push_str(text);
                    output.push_str("\n\n");
                }
                Message::ToolUse { .. } => {} // Already filtered out
            }
        }

        // Separator between sessions (except after last)
        if session_num < total_sessions {
            output.push_str("---\n\n");
        }
    }

    // Footer
    output.push_str("\n---\n\n");
    output.push_str("You can now ask follow-up questions about this work.\n");

    output
}

/// Format prompts as JSON for machine consumption
fn format_context_json(
    prompts: &BTreeMap<String, PromptRecord>,
    commit_info: Option<&CommitInfo>,
) -> String {
    use serde_json::json;

    let prompts_json: Vec<serde_json::Value> = prompts
        .iter()
        .map(|(id, prompt)| {
            json!({
                "id": id,
                "tool": prompt.agent_id.tool,
                "model": prompt.agent_id.model,
                "author": prompt.human_author,
                "messages": prompt.messages.iter()
                    .filter(|m| !matches!(m, Message::ToolUse { .. }))
                    .map(|m| match m {
                        Message::User { text, timestamp } => json!({
                            "role": "user",
                            "text": text,
                            "timestamp": timestamp
                        }),
                        Message::Assistant { text, timestamp } => json!({
                            "role": "assistant",
                            "text": text,
                            "timestamp": timestamp
                        }),
                        Message::Thinking { text, timestamp } => json!({
                            "role": "thinking",
                            "text": text,
                            "timestamp": timestamp
                        }),
                        Message::Plan { text, timestamp } => json!({
                            "role": "plan",
                            "text": text,
                            "timestamp": timestamp
                        }),
                        Message::ToolUse { .. } => json!(null),
                    })
                    .filter(|v| !v.is_null())
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    let output = json!({
        "source": commit_info.map(|info| json!({
            "sha": info.sha,
            "author": info.author,
            "date": info.date,
            "message": info.message
        })),
        "prompts": prompts_json
    });

    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
}

fn parse_line_range(s: &str) -> Result<(u32, u32), String> {
    if let Some(pos) = s.find('-') {
        let start: u32 = s[..pos]
            .parse()
            .map_err(|_| format!("Invalid line number: {}", &s[..pos]))?;
        let end: u32 = s[pos + 1..]
            .parse()
            .map_err(|_| format!("Invalid line number: {}", &s[pos + 1..]))?;
        if start > end {
            return Err(format!(
                "Invalid line range: start ({}) > end ({})",
                start, end
            ));
        }
        Ok((start, end))
    } else {
        let line: u32 = s
            .parse()
            .map_err(|_| format!("Invalid line number: {}", s))?;
        Ok((line, line))
    }
}

fn print_continue_help() {
    eprintln!("git-ai continue - Restore AI session context and launch agent");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    git-ai continue [OPTIONS]");
    eprintln!();
    eprintln!("CONTEXT SOURCE (at least one, or none for TUI mode):");
    eprintln!("    --commit <rev>          Continue from a specific commit");
    eprintln!("    --file <path>           Continue from a specific file");
    eprintln!("    --lines <start-end>     Limit to line range (requires --file)");
    eprintln!("    --prompt-id <id>        Continue from a specific prompt");
    eprintln!("    (no args)               Launch interactive TUI picker");
    eprintln!();
    eprintln!("AGENT SELECTION:");
    eprintln!("    --agent <name>          Agent to use (claude, cursor; default: claude)");
    eprintln!("    --tool <name>           Alias for --agent");
    eprintln!();
    eprintln!("OUTPUT MODE:");
    eprintln!("    (default)               Launch agent CLI (terminal) or write to stdout (pipe)");
    eprintln!("    --launch                Launch agent CLI with the context (always)");
    eprintln!("    --clipboard             Copy context to system clipboard");
    eprintln!("    --json                  Output context as structured JSON");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("    --max-messages <n>      Max messages per prompt in output (default: 50)");
    eprintln!();
    eprintln!("EXAMPLES:");
    eprintln!("    git-ai continue --commit abc1234");
    eprintln!("    git-ai continue --file src/main.rs --lines 10-50");
    eprintln!("    git-ai continue --commit abc1234 --launch");
    eprintln!("    git-ai continue --commit abc1234 --agent claude --launch");
    eprintln!("    git-ai continue --file src/main.rs --clipboard");
    eprintln!("    git-ai continue --prompt-id abcd1234ef567890");
    eprintln!("    git-ai continue                # TUI mode");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_continue_mode_variants() {
        let by_commit = ContinueMode::ByCommit {
            commit_rev: "abc123".to_string(),
        };
        let by_file = ContinueMode::ByFile {
            file_path: "src/main.rs".to_string(),
            line_ranges: vec![(10, 50)],
        };
        let by_prompt_id = ContinueMode::ByPromptId {
            prompt_id: "xyz789".to_string(),
        };
        let interactive = ContinueMode::Interactive;

        assert_eq!(
            by_commit,
            ContinueMode::ByCommit {
                commit_rev: "abc123".to_string()
            }
        );
        assert_eq!(
            by_file,
            ContinueMode::ByFile {
                file_path: "src/main.rs".to_string(),
                line_ranges: vec![(10, 50)]
            }
        );
        assert_eq!(
            by_prompt_id,
            ContinueMode::ByPromptId {
                prompt_id: "xyz789".to_string()
            }
        );
        assert_eq!(interactive, ContinueMode::Interactive);
    }

    #[test]
    fn test_continue_options_default() {
        let options = ContinueOptions::new();
        assert!(options.agent.is_none());
        assert!(!options.launch);
        assert!(!options.clipboard);
        assert!(!options.json);
        assert!(options.max_messages.is_none());
        assert_eq!(options.agent_name(), "claude");
    }

    #[test]
    fn test_continue_options_agent_name() {
        let options = ContinueOptions {
            agent: Some("cursor".to_string()),
            ..Default::default()
        };
        assert_eq!(options.agent_name(), "cursor");
    }

    #[test]
    fn test_parse_continue_args_empty() {
        let args: Vec<String> = vec![];
        let parsed = parse_continue_args(&args).unwrap();
        assert_eq!(parsed.mode, ContinueMode::Interactive);
    }

    #[test]
    fn test_parse_continue_args_commit() {
        let args = vec!["--commit".to_string(), "abc123".to_string()];
        let parsed = parse_continue_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            ContinueMode::ByCommit {
                commit_rev: "abc123".to_string()
            }
        );
    }

    #[test]
    fn test_parse_continue_args_with_launch() {
        let args = vec![
            "--commit".to_string(),
            "HEAD".to_string(),
            "--agent".to_string(),
            "Claude".to_string(),
            "--launch".to_string(),
        ];
        let parsed = parse_continue_args(&args).unwrap();
        assert!(parsed.options.launch);
        assert_eq!(parsed.options.agent, Some("claude".to_string())); // lowercased
    }

    #[test]
    fn test_parse_continue_args_file_with_lines() {
        let args = vec![
            "--file".to_string(),
            "src/lib.rs".to_string(),
            "--lines".to_string(),
            "20-40".to_string(),
        ];
        let parsed = parse_continue_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            ContinueMode::ByFile {
                file_path: "src/lib.rs".to_string(),
                line_ranges: vec![(20, 40)]
            }
        );
    }

    #[test]
    fn test_parse_line_range() {
        assert_eq!(parse_line_range("42").unwrap(), (42, 42));
        assert_eq!(parse_line_range("10-50").unwrap(), (10, 50));
        assert!(parse_line_range("50-10").is_err());
    }

    #[test]
    fn test_parse_agent_choice_empty_default() {
        let choice = parse_agent_choice_input("").unwrap();
        assert_eq!(choice, AgentChoice::Launch("claude".to_string()));
    }

    #[test]
    fn test_parse_agent_choice_one() {
        let choice = parse_agent_choice_input("1").unwrap();
        assert_eq!(choice, AgentChoice::Launch("claude".to_string()));
    }

    #[test]
    fn test_parse_agent_choice_two() {
        let choice = parse_agent_choice_input("2").unwrap();
        assert_eq!(choice, AgentChoice::Stdout);
    }

    #[test]
    fn test_parse_agent_choice_three() {
        let choice = parse_agent_choice_input("3").unwrap();
        assert_eq!(choice, AgentChoice::Clipboard);
    }

    #[test]
    fn test_parse_agent_choice_invalid() {
        assert!(parse_agent_choice_input("4").is_err());
        assert!(parse_agent_choice_input("abc").is_err());
    }

    #[test]
    fn test_parse_agent_choice_with_whitespace() {
        let choice = parse_agent_choice_input("  2  \n").unwrap();
        assert_eq!(choice, AgentChoice::Stdout);
    }

    #[test]
    fn test_no_args_activates_interactive_mode() {
        let args: Vec<String> = vec![];
        let parsed = parse_continue_args(&args).unwrap();
        assert_eq!(parsed.mode, ContinueMode::Interactive);
    }
}
