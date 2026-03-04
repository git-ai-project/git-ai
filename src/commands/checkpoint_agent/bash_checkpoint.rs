//! Shared bash/shell command checkpoint logic.
//!
//! This module provides a standard code path for determining whether a shell
//! command invocation should trigger a checkpoint and what scope that checkpoint
//! should have. It is used by all agent presets when they encounter a bash/shell
//! tool invocation.
//!
//! Design:
//! - Pre-command hooks produce a **Human** checkpoint (captures user changes).
//! - Post-command hooks produce an **AI** checkpoint (captures agent changes).
//! - A blacklist of read-only / non-file-modifying commands is evaluated to skip
//!   checkpoints for commands that cannot change the working tree.
//! - An 800ms timeout kill-switch aborts the evaluation and logs to Sentry.

use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::observability::log_error;
use std::time::Instant;

/// Maximum wall-clock time allowed for bash checkpoint evaluation.
/// If exceeded, we abort and log to Sentry.
const BASH_CHECKPOINT_TIMEOUT_MS: u128 = 800;

/// Result of evaluating a bash command for checkpointing.
#[derive(Debug, Clone, PartialEq)]
pub struct BashCheckpointResult {
    /// Whether a checkpoint should be created.
    pub should_checkpoint: bool,
    /// The kind of checkpoint (Human for pre-command, AiAgent for post-command).
    pub checkpoint_kind: CheckpointKind,
    /// Optional list of file paths that the command is likely to modify.
    /// `None` means "unscoped" — the checkpoint should consider all dirty files.
    pub scoped_paths: Option<Vec<String>>,
}

/// Evaluate whether a bash command should trigger a checkpoint.
///
/// `command` is the raw shell command string from the hook payload.
/// `is_pre_command` indicates whether this is a pre-tool (true) or post-tool (false) hook.
///
/// Returns `Ok(BashCheckpointResult)` with `should_checkpoint = false` if the
/// command is on the blacklist (read-only). Returns an error if the evaluation
/// times out (>800ms).
pub fn evaluate_bash_command(
    command: &str,
    is_pre_command: bool,
) -> Result<BashCheckpointResult, GitAiError> {
    let start = Instant::now();

    let checkpoint_kind = if is_pre_command {
        CheckpointKind::Human
    } else {
        CheckpointKind::AiAgent
    };

    // Fast path: empty command
    if command.trim().is_empty() {
        return Ok(BashCheckpointResult {
            should_checkpoint: false,
            checkpoint_kind,
            scoped_paths: None,
        });
    }

    // Check timeout before doing work
    if start.elapsed().as_millis() > BASH_CHECKPOINT_TIMEOUT_MS {
        return Err(timeout_error("bash_checkpoint_evaluate", command));
    }

    // Extract the base command (first word or pipeline components)
    let trimmed = command.trim();

    // Check if the command is on the blacklist
    if is_blacklisted_command(trimmed) {
        return Ok(BashCheckpointResult {
            should_checkpoint: false,
            checkpoint_kind,
            scoped_paths: None,
        });
    }

    // Check timeout again after blacklist evaluation
    if start.elapsed().as_millis() > BASH_CHECKPOINT_TIMEOUT_MS {
        return Err(timeout_error("bash_checkpoint_blacklist", command));
    }

    // Try to extract scoped file paths from common patterns
    let scoped_paths = extract_scoped_paths(trimmed);

    Ok(BashCheckpointResult {
        should_checkpoint: true,
        checkpoint_kind,
        scoped_paths,
    })
}

/// Commands (or command prefixes) that are read-only and should never trigger
/// a checkpoint. We match against the first token of each pipeline segment.
const BLACKLISTED_COMMANDS: &[&str] = &[
    // Read-only file inspection
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "file",
    "stat",
    "du",
    "df",
    "md5sum",
    "sha256sum",
    "sha1sum",
    "shasum",
    "xxd",
    "hexdump",
    "strings",
    "od",
    // Navigation & listing
    "ls",
    "dir",
    "pwd",
    "cd",
    "pushd",
    "popd",
    "tree",
    "exa",
    // Search
    "find",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "fd",
    "locate",
    "which",
    "whereis",
    "type",
    "whence",
    // Environment / identity
    "echo",
    "printf",
    "whoami",
    "id",
    "env",
    "printenv",
    "set",
    "export",
    "unset",
    "alias",
    "date",
    "uname",
    "hostname",
    "uptime",
    "free",
    "top",
    "htop",
    "ps",
    "pgrep",
    "lsof",
    // Diff / comparison (read-only)
    "diff",
    "cmp",
    "comm",
    "sort",
    "uniq",
    "cut",
    "tr",
    "awk",
    "jq",
    "yq",
    "xargs",
    // Network inspection
    "curl",
    "wget",
    "ping",
    "dig",
    "nslookup",
    "host",
    "nc",
    "netstat",
    "ss",
    // Help / manuals
    "man",
    "info",
    "help",
    // Build / test / run (typically don't modify tracked files)
    "make",
    "cargo",
    "npm",
    "npx",
    "yarn",
    "pnpm",
    "node",
    "python",
    "python3",
    "ruby",
    "go",
    "java",
    "javac",
    "mvn",
    "gradle",
    "dotnet",
    "pytest",
    "jest",
    "mocha",
    "vitest",
    // Shell builtins
    "true",
    "false",
    "test",
    "[",
    "exit",
    "return",
    "source",
    ".",
    "eval",
    "exec",
    "wait",
    "sleep",
    "time",
    "nohup",
    "nice",
    "history",
    // Process management
    "kill",
    "killall",
    "pkill",
    "bg",
    "fg",
    "jobs",
    "disown",
];

/// Git sub-commands that are read-only.
const GIT_READONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "describe",
    "tag",
    "blame",
    "shortlog",
    "reflog",
    "stash list",
    "config",
    "name-rev",
    "rev-list",
    "for-each-ref",
    "count-objects",
    "fsck",
    "verify-commit",
    "verify-tag",
];

/// Returns true if the command should NOT trigger a checkpoint.
fn is_blacklisted_command(command: &str) -> bool {
    // Handle pipelines: check each segment
    // If ALL segments are blacklisted, the whole pipeline is blacklisted
    // If any segment could write files, we should checkpoint
    let segments: Vec<&str> = split_pipeline(command);

    // For single commands or all-blacklisted pipelines, skip checkpoint
    segments
        .iter()
        .all(|segment| is_single_command_blacklisted(segment.trim()))
}

/// Check if a single command (not a pipeline) is blacklisted.
fn is_single_command_blacklisted(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return true;
    }

    // Skip leading env vars (FOO=bar cmd ...) and sudo
    let effective = skip_env_prefixes(trimmed);
    if effective.is_empty() {
        return true;
    }

    let first_token = first_word(effective);

    // Handle `git` specifically
    if first_token == "git" {
        return is_git_command_readonly(effective);
    }

    // Check against the blacklist
    BLACKLISTED_COMMANDS.contains(&first_token)
}

/// Skip leading `VAR=value` assignments and `sudo`/`env` wrappers.
fn skip_env_prefixes(command: &str) -> &str {
    let mut rest = command;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return trimmed;
        }
        let token = first_word(trimmed);
        if token == "sudo" || token == "env" {
            rest = &trimmed[token.len()..];
            continue;
        }
        // Skip VAR=value patterns (e.g. FOO=bar, PATH=/usr/bin)
        if token.contains('=') && !token.starts_with('=') && !token.starts_with('-') {
            rest = &trimmed[token.len()..];
            continue;
        }
        return trimmed;
    }
}

/// Extract the first whitespace-delimited token.
fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

/// Check if a `git ...` command is read-only.
fn is_git_command_readonly(command: &str) -> bool {
    // Extract git subcommand: skip `git` and any flags like `git -C /path`
    let parts: Vec<&str> = command.split_whitespace().collect();
    let mut i = 1; // skip "git"

    // Skip global git options that take an argument
    while i < parts.len() {
        let part = parts[i];
        if part.starts_with('-') {
            // Options that take a following argument
            if matches!(part, "-C" | "-c" | "--git-dir" | "--work-tree") {
                i += 2; // skip the option and its argument
            } else {
                i += 1; // skip the option
            }
        } else {
            break;
        }
    }

    if i >= parts.len() {
        return true; // bare `git` with no subcommand
    }

    let subcommand = parts[i];

    GIT_READONLY_SUBCOMMANDS.contains(&subcommand)
}

/// Split a command string on pipe operators (`|`), but not inside quotes.
fn split_pipeline(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = command.char_indices().peekable();
    let mut prev_char = None;

    while let Some((i, c)) = chars.next() {
        match c {
            '\'' if !in_double_quote && prev_char != Some('\\') => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote && prev_char != Some('\\') => {
                in_double_quote = !in_double_quote;
            }
            '|' if !in_single_quote && !in_double_quote => {
                // Check for || (logical OR) vs | (pipe)
                if chars.peek().map(|(_, c)| *c) == Some('|') {
                    // This is || (logical OR) — treat as command separator
                    chars.next();
                    segments.push(&command[start..i]);
                    start = i + 2;
                } else {
                    segments.push(&command[start..i]);
                    start = i + 1;
                }
            }
            ';' | '&' if !in_single_quote && !in_double_quote => {
                // Command separator: treat as separate commands
                // Handle && (logical AND)
                if c == '&' && chars.peek().map(|(_, c)| *c) == Some('&') {
                    chars.next();
                }
                segments.push(&command[start..i]);
                start = if c == '&' && command[i..].starts_with("&&") {
                    i + 2
                } else {
                    i + 1
                };
            }
            _ => {}
        }
        prev_char = Some(c);
    }

    // Last segment
    let last = &command[start..];
    if !last.trim().is_empty() {
        segments.push(last);
    }

    if segments.is_empty() {
        segments.push(command);
    }

    segments
}

/// Try to extract file paths from common file-modifying command patterns.
/// Returns `None` for unscoped (unknown pattern).
fn extract_scoped_paths(command: &str) -> Option<Vec<String>> {
    let effective = skip_env_prefixes(command.trim());
    let first = first_word(effective);

    match first {
        // sed -i <expr> <file>...
        "sed" if effective.contains("-i") => extract_sed_targets(effective),
        // mv <src> <dst>
        "mv" => extract_mv_targets(effective),
        // cp <src> <dst>
        "cp" => extract_last_arg(effective),
        // touch <file>...
        "touch" => extract_non_flag_args(effective),
        // rm <file>...
        "rm" => extract_non_flag_args(effective),
        // mkdir <dir>...
        "mkdir" => extract_non_flag_args(effective),
        // patch <file>
        "patch" => extract_non_flag_args(effective),
        // chmod / chown
        "chmod" | "chown" => extract_non_flag_args_skip_first(effective),
        _ => None,
    }
}

/// For `sed -i ... <file>`, extract the file arguments.
fn extract_sed_targets(command: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    // Find files after the expression: sed -i[suffix] 'expr' file1 file2 ...
    // or: sed -i 'expr' file1 file2 ...
    let mut found_i_flag = false;
    let mut skip_next = false;
    let mut past_expression = false;
    let mut files = Vec::new();

    for part in parts.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !found_i_flag {
            if part.starts_with("-i") {
                found_i_flag = true;
                // -i'' or -i.bak is the suffix inline, -i followed by space might be followed by expr
                continue;
            }
            if part.starts_with('-') {
                if *part == "-e" || *part == "-f" {
                    skip_next = true;
                }
                continue;
            }
        }
        if found_i_flag
            && !past_expression
            && !part.starts_with('-')
            && !part.starts_with('/')
            && files.is_empty()
        {
            // This is likely the expression, skip it
            past_expression = true;
            continue;
        }
        if !part.starts_with('-') {
            files.push(part.to_string());
        }
    }

    if files.is_empty() { None } else { Some(files) }
}

/// For `mv src dst`, extract the destination.
fn extract_mv_targets(command: &str) -> Option<Vec<String>> {
    let args = non_flag_args(command);
    // mv has src... dst — both src and dst are affected
    if args.len() >= 2 { Some(args) } else { None }
}

/// Extract the last non-flag argument (e.g. destination for `cp`).
fn extract_last_arg(command: &str) -> Option<Vec<String>> {
    let args = non_flag_args(command);
    args.last().map(|a| vec![a.clone()])
}

/// Extract all non-flag arguments (skip command name).
fn extract_non_flag_args(command: &str) -> Option<Vec<String>> {
    let args = non_flag_args(command);
    if args.is_empty() { None } else { Some(args) }
}

/// Extract non-flag arguments, skipping the first non-flag arg after command
/// (e.g., for `chmod 755 file` — skip the mode).
fn extract_non_flag_args_skip_first(command: &str) -> Option<Vec<String>> {
    let args = non_flag_args(command);
    if args.len() <= 1 {
        None
    } else {
        Some(args[1..].to_vec())
    }
}

/// Helper: collect non-flag arguments, skipping the command itself.
fn non_flag_args(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .skip(1) // skip command
        .filter(|arg| !arg.starts_with('-'))
        .map(|s| s.to_string())
        .collect()
}

/// Create a timeout error and log it to Sentry.
fn timeout_error(operation: &str, command: &str) -> GitAiError {
    let truncated_cmd = if command.len() > 200 {
        format!("{}...", &command[..200])
    } else {
        command.to_string()
    };
    let err_msg = format!(
        "Bash checkpoint timed out (>{}ms) during {}: {}",
        BASH_CHECKPOINT_TIMEOUT_MS, operation, truncated_cmd
    );
    let error = GitAiError::Generic(err_msg.clone());
    log_error(
        &error,
        Some(serde_json::json!({
            "operation": operation,
            "timeout_ms": BASH_CHECKPOINT_TIMEOUT_MS,
            "command_preview": truncated_cmd,
        })),
    );
    error
}

/// Known bash/shell tool names for each agent.
/// Returns true if the given tool name is a bash/shell tool for the specified agent.
pub fn is_bash_tool(agent: &str, tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    match agent {
        "claude" => lower == "bash" || lower == "terminal",
        "gemini" => lower == "shell" || lower == "run_shell_command",
        "droid" => lower == "bash" || lower == "shell" || lower == "terminal",
        "github-copilot" | "vscode" => {
            lower == "runinterminal"
                || lower == "terminal"
                || lower == "run_in_terminal"
                || lower == "runcommand"
        }
        "codex" => lower == "shell" || lower == "bash",
        "amp" => lower == "bash" || lower == "shell" || lower == "terminal",
        "opencode" => lower == "bash" || lower == "shell",
        "cursor" => lower == "terminal" || lower == "runinterminal" || lower == "bash",
        "continue" => lower == "terminal" || lower == "bash" || lower == "shell",
        _ => {
            // Generic fallback: common bash tool names
            matches!(
                lower.as_str(),
                "bash" | "shell" | "terminal" | "runinterminal" | "run_in_terminal" | "runcommand"
            )
        }
    }
}

/// Known file-editing tool names for each agent.
/// Returns true if the given tool name is a known file-editing tool for the specified agent.
pub fn is_file_edit_tool(agent: &str, tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();

    match agent {
        "claude" => matches!(lower.as_str(), "write" | "edit" | "multiedit"),
        "gemini" => matches!(lower.as_str(), "write_file" | "replace"),
        "droid" => matches!(lower.as_str(), "edit" | "write" | "create" | "applypatch"),
        "github-copilot" | "vscode" => is_supported_vscode_edit_tool_name(tool_name),
        "codex" => {
            // Codex uses `notify` for all events, filtering not needed at this level
            matches!(lower.as_str(), "write" | "edit" | "patch" | "create")
        }
        "amp" => {
            matches!(
                lower.as_str(),
                "write" | "edit" | "multiedit" | "create" | "applypatch"
            )
        }
        "opencode" => matches!(lower.as_str(), "edit" | "write"),
        "cursor" => {
            // Cursor uses beforeSubmitPrompt/afterFileEdit, so file editing is implicit
            // But for completeness:
            matches!(lower.as_str(), "edit" | "write" | "create")
        }
        "continue" => matches!(lower.as_str(), "edit" | "write" | "create"),
        _ => {
            // Generic fallback using the vscode heuristic
            is_supported_vscode_edit_tool_name(tool_name)
        }
    }
}

/// Ported from the existing `is_supported_vscode_edit_tool_name` function in agent_presets.rs.
/// Determines if a tool name corresponds to a file-editing operation.
fn is_supported_vscode_edit_tool_name(tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();

    // Quick reject: tools that are clearly read-only
    let non_edit_keywords = [
        "find", "search", "read", "grep", "glob", "list", "ls", "fetch", "web", "open", "todo",
        "terminal", "run", "execute",
    ];
    if non_edit_keywords.iter().any(|kw| lower.contains(kw)) {
        return false;
    }

    // Exact matches for known edit tools
    let exact_edit_tools = [
        "write",
        "edit",
        "multiedit",
        "applypatch",
        "copilot_insertedit",
        "copilot_replacestring",
        "vscode_editfile_internal",
        "create_file",
        "delete_file",
        "rename_file",
        "move_file",
        "replace_string_in_file",
        "insert_edit_into_file",
    ];
    if exact_edit_tools.iter().any(|name| lower == *name) {
        return true;
    }

    // Partial matches
    lower.contains("edit") || lower.contains("write") || lower.contains("replace")
}

/// Determine the tool category for checkpoint filtering.
/// Returns one of: "file_edit", "bash", "skip"
pub fn classify_tool(agent: &str, tool_name: &str) -> ToolClassification {
    if is_file_edit_tool(agent, tool_name) {
        ToolClassification::FileEdit
    } else if is_bash_tool(agent, tool_name) {
        ToolClassification::Bash
    } else {
        ToolClassification::Skip
    }
}

/// Classification of a tool for checkpoint purposes.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolClassification {
    /// Known file-editing tool — use standard checkpoint logic.
    FileEdit,
    /// Bash/shell tool — use bash checkpoint logic with blacklist evaluation.
    Bash,
    /// Other tool (read-only, search, etc.) — skip checkpoint.
    Skip,
}

/// Extract the bash command string from a hook payload's tool_input.
/// Different agents encode the command differently; this handles common patterns.
pub fn extract_bash_command(tool_input: &serde_json::Value) -> Option<String> {
    // Try common field names for the command string
    for key in [
        "command",
        "cmd",
        "input",
        "script",
        "code",
        "content",
        "shell_command",
    ] {
        if let Some(cmd) = tool_input.get(key).and_then(|v| v.as_str()) {
            let trimmed = cmd.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // Some agents put the command as the top-level string value
    if let Some(cmd) = tool_input.as_str() {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Blacklist Tests ====================

    #[test]
    fn test_blacklisted_readonly_commands() {
        let readonly_commands = vec![
            "ls -la",
            "cat file.txt",
            "grep -r pattern .",
            "find . -name '*.rs'",
            "git status",
            "git log --oneline",
            "git diff HEAD~1",
            "pwd",
            "echo hello",
            "whoami",
            "env",
            "date",
            "uname -a",
            "head -n 10 file.txt",
            "tail -f log.txt",
            "wc -l file.txt",
            "which git",
            "tree src/",
            "rg pattern",
            "fd '*.rs'",
            "diff file1 file2",
            "sort file.txt",
            "curl https://example.com",
            "python -c 'print(1)'",
            "node -e 'console.log(1)'",
            "cargo test",
            "npm test",
            "pytest tests/",
            "make test",
        ];

        for cmd in readonly_commands {
            let result = evaluate_bash_command(cmd, false).unwrap();
            assert!(
                !result.should_checkpoint,
                "Expected '{}' to be blacklisted (no checkpoint)",
                cmd
            );
        }
    }

    #[test]
    fn test_non_blacklisted_modifying_commands() {
        let modifying_commands = vec![
            "sed -i 's/foo/bar/' file.txt",
            "mv old.txt new.txt",
            "cp src.txt dst.txt",
            "rm -rf build/",
            "mkdir -p new_dir",
            "touch new_file.txt",
            "git checkout feature-branch",
            "git merge main",
            "git reset HEAD~1",
            "patch -p1 < fix.patch",
            "chmod 755 script.sh",
        ];

        for cmd in modifying_commands {
            let result = evaluate_bash_command(cmd, false).unwrap();
            assert!(
                result.should_checkpoint,
                "Expected '{}' to trigger a checkpoint",
                cmd
            );
        }
    }

    #[test]
    fn test_empty_command_no_checkpoint() {
        let result = evaluate_bash_command("", false).unwrap();
        assert!(!result.should_checkpoint);

        let result = evaluate_bash_command("   ", true).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_pre_command_is_human_checkpoint() {
        let result = evaluate_bash_command("touch file.txt", true).unwrap();
        assert!(result.should_checkpoint);
        assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    }

    #[test]
    fn test_post_command_is_ai_checkpoint() {
        let result = evaluate_bash_command("touch file.txt", false).unwrap();
        assert!(result.should_checkpoint);
        assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    }

    // ==================== Pipeline Tests ====================

    #[test]
    fn test_all_blacklisted_pipeline() {
        // All segments read-only => no checkpoint
        let result = evaluate_bash_command("cat file.txt | grep pattern | sort", false).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_mixed_pipeline_triggers_checkpoint() {
        // Pipe into tee (file-writing) => checkpoint
        let result = evaluate_bash_command("echo hello | tee output.txt", false).unwrap();
        assert!(result.should_checkpoint);
    }

    #[test]
    fn test_chained_commands_with_semicolons() {
        // All blacklisted
        let result = evaluate_bash_command("cd /tmp; ls; pwd", false).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_chained_commands_with_modifying() {
        // Has a modifying command
        let result = evaluate_bash_command("cd /tmp && touch file.txt && ls", false).unwrap();
        assert!(result.should_checkpoint);
    }

    // ==================== Env Prefix Tests ====================

    #[test]
    fn test_env_var_prefix_skipped() {
        let result = evaluate_bash_command("FOO=bar cat file.txt", false).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_sudo_prefix_skipped() {
        let result = evaluate_bash_command("sudo cat file.txt", false).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_sudo_with_modifying_command() {
        let result = evaluate_bash_command("sudo rm -rf /tmp/foo", false).unwrap();
        assert!(result.should_checkpoint);
    }

    // ==================== Git Command Tests ====================

    #[test]
    fn test_git_readonly_subcommands() {
        let readonly = vec![
            "git status",
            "git log --oneline",
            "git diff HEAD",
            "git show HEAD:file.txt",
            "git branch -a",
            "git remote -v",
            "git rev-parse HEAD",
            "git ls-files",
            "git blame file.txt",
            "git -C /path/to/repo status",
        ];

        for cmd in readonly {
            let result = evaluate_bash_command(cmd, false).unwrap();
            assert!(
                !result.should_checkpoint,
                "Expected git readonly '{}' to be blacklisted",
                cmd
            );
        }
    }

    #[test]
    fn test_git_modifying_subcommands() {
        let modifying = vec![
            "git checkout feature",
            "git merge main",
            "git reset HEAD~1",
            "git stash pop",
            "git cherry-pick abc123",
            "git rebase main",
            "git commit -m 'test'",
            "git add file.txt",
            "git rm file.txt",
            "git mv old.txt new.txt",
            "git pull",
            "git push",
        ];

        for cmd in modifying {
            let result = evaluate_bash_command(cmd, false).unwrap();
            assert!(
                result.should_checkpoint,
                "Expected git modifying '{}' to trigger checkpoint",
                cmd
            );
        }
    }

    // ==================== Scope Extraction Tests ====================

    #[test]
    fn test_sed_scope_extraction() {
        let result = evaluate_bash_command("sed -i 's/foo/bar/' file.txt", false).unwrap();
        assert!(result.should_checkpoint);
        // sed scope extraction is best-effort
        if let Some(paths) = &result.scoped_paths {
            assert!(paths.contains(&"file.txt".to_string()));
        }
    }

    #[test]
    fn test_mv_scope_extraction() {
        let result = evaluate_bash_command("mv old.txt new.txt", false).unwrap();
        assert!(result.should_checkpoint);
        if let Some(paths) = &result.scoped_paths {
            assert!(paths.contains(&"old.txt".to_string()));
            assert!(paths.contains(&"new.txt".to_string()));
        }
    }

    #[test]
    fn test_touch_scope_extraction() {
        let result = evaluate_bash_command("touch file1.txt file2.txt", false).unwrap();
        assert!(result.should_checkpoint);
        if let Some(paths) = &result.scoped_paths {
            assert!(paths.contains(&"file1.txt".to_string()));
            assert!(paths.contains(&"file2.txt".to_string()));
        }
    }

    #[test]
    fn test_rm_scope_extraction() {
        let result = evaluate_bash_command("rm file.txt", false).unwrap();
        assert!(result.should_checkpoint);
        if let Some(paths) = &result.scoped_paths {
            assert!(paths.contains(&"file.txt".to_string()));
        }
    }

    #[test]
    fn test_unknown_command_unscoped() {
        let result = evaluate_bash_command("custom-build-tool --output dist/", false).unwrap();
        assert!(result.should_checkpoint);
        assert!(result.scoped_paths.is_none());
    }

    // ==================== Tool Classification Tests ====================

    #[test]
    fn test_classify_claude_tools() {
        assert_eq!(
            classify_tool("claude", "Write"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("claude", "Edit"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("claude", "MultiEdit"),
            ToolClassification::FileEdit
        );
        assert_eq!(classify_tool("claude", "Bash"), ToolClassification::Bash);
        assert_eq!(classify_tool("claude", "Read"), ToolClassification::Skip);
        assert_eq!(classify_tool("claude", "Search"), ToolClassification::Skip);
        assert_eq!(
            classify_tool("claude", "TodoRead"),
            ToolClassification::Skip
        );
    }

    #[test]
    fn test_classify_gemini_tools() {
        assert_eq!(
            classify_tool("gemini", "write_file"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("gemini", "replace"),
            ToolClassification::FileEdit
        );
        assert_eq!(classify_tool("gemini", "shell"), ToolClassification::Bash);
        assert_eq!(
            classify_tool("gemini", "read_file"),
            ToolClassification::Skip
        );
    }

    #[test]
    fn test_classify_droid_tools() {
        assert_eq!(classify_tool("droid", "Edit"), ToolClassification::FileEdit);
        assert_eq!(
            classify_tool("droid", "Write"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("droid", "Create"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("droid", "ApplyPatch"),
            ToolClassification::FileEdit
        );
        assert_eq!(classify_tool("droid", "Bash"), ToolClassification::Bash);
        assert_eq!(classify_tool("droid", "Shell"), ToolClassification::Bash);
    }

    #[test]
    fn test_classify_copilot_tools() {
        assert_eq!(
            classify_tool("github-copilot", "write"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("github-copilot", "edit"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("github-copilot", "runInTerminal"),
            ToolClassification::Bash
        );
        assert_eq!(
            classify_tool("github-copilot", "search"),
            ToolClassification::Skip
        );
    }

    #[test]
    fn test_classify_opencode_tools() {
        assert_eq!(
            classify_tool("opencode", "edit"),
            ToolClassification::FileEdit
        );
        assert_eq!(
            classify_tool("opencode", "write"),
            ToolClassification::FileEdit
        );
        assert_eq!(classify_tool("opencode", "bash"), ToolClassification::Bash);
    }

    #[test]
    fn test_classify_amp_tools() {
        assert_eq!(classify_tool("amp", "Write"), ToolClassification::FileEdit);
        assert_eq!(classify_tool("amp", "Edit"), ToolClassification::FileEdit);
        assert_eq!(classify_tool("amp", "Bash"), ToolClassification::Bash);
    }

    // ==================== is_bash_tool Tests ====================

    #[test]
    fn test_is_bash_tool_various_agents() {
        assert!(is_bash_tool("claude", "Bash"));
        assert!(is_bash_tool("claude", "bash"));
        assert!(!is_bash_tool("claude", "Write"));

        assert!(is_bash_tool("gemini", "shell"));
        assert!(!is_bash_tool("gemini", "write_file"));

        assert!(is_bash_tool("github-copilot", "runInTerminal"));
        assert!(is_bash_tool("github-copilot", "terminal"));

        assert!(is_bash_tool("amp", "Bash"));
        assert!(is_bash_tool("opencode", "bash"));
        assert!(is_bash_tool("opencode", "shell"));
    }

    // ==================== is_file_edit_tool Tests ====================

    #[test]
    fn test_is_file_edit_tool_various_agents() {
        assert!(is_file_edit_tool("claude", "Write"));
        assert!(is_file_edit_tool("claude", "Edit"));
        assert!(is_file_edit_tool("claude", "MultiEdit"));
        assert!(!is_file_edit_tool("claude", "Bash"));
        assert!(!is_file_edit_tool("claude", "Read"));

        assert!(is_file_edit_tool("gemini", "write_file"));
        assert!(is_file_edit_tool("gemini", "replace"));
        assert!(!is_file_edit_tool("gemini", "shell"));

        assert!(is_file_edit_tool("droid", "Edit"));
        assert!(is_file_edit_tool("droid", "ApplyPatch"));
    }

    // ==================== extract_bash_command Tests ====================

    #[test]
    fn test_extract_bash_command_from_various_formats() {
        let input1 = serde_json::json!({"command": "ls -la"});
        assert_eq!(extract_bash_command(&input1), Some("ls -la".to_string()));

        let input2 = serde_json::json!({"cmd": "echo hello"});
        assert_eq!(
            extract_bash_command(&input2),
            Some("echo hello".to_string())
        );

        let input3 = serde_json::json!({"input": "touch file.txt"});
        assert_eq!(
            extract_bash_command(&input3),
            Some("touch file.txt".to_string())
        );

        let input4 = serde_json::json!("mkdir -p dir");
        assert_eq!(
            extract_bash_command(&input4),
            Some("mkdir -p dir".to_string())
        );

        let input5 = serde_json::json!({"unrelated_field": "value"});
        assert_eq!(extract_bash_command(&input5), None);

        let input6 = serde_json::json!({"command": ""});
        assert_eq!(extract_bash_command(&input6), None);
    }

    // ==================== Pipeline Splitting Tests ====================

    #[test]
    fn test_split_pipeline_simple() {
        let segments = split_pipeline("cat file | grep pattern");
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn test_split_pipeline_with_logical_or() {
        // || is a command separator (like && and ;), so it should split into 2 segments
        let segments = split_pipeline("cmd1 || cmd2");
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn test_split_pipeline_with_logical_and() {
        let segments = split_pipeline("cmd1 && cmd2");
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn test_split_pipeline_with_semicolons() {
        let segments = split_pipeline("cmd1; cmd2; cmd3");
        assert_eq!(segments.len(), 3);
    }

    #[test]
    fn test_split_pipeline_quoted_pipe() {
        // Pipe inside quotes should not split
        let segments = split_pipeline("echo 'hello | world'");
        assert_eq!(segments.len(), 1);
    }

    // ==================== Edge Cases ====================

    #[test]
    fn test_redirection_not_blacklisted() {
        // Commands with redirection operators should typically checkpoint
        // since they write to files, but the base command might be blacklisted.
        // The full redirection handling is intentionally simple — if the base
        // command is unknown (not blacklisted), we checkpoint.
        let result = evaluate_bash_command("custom-tool > output.txt", false).unwrap();
        assert!(result.should_checkpoint);
    }

    #[test]
    fn test_complex_command_not_blacklisted() {
        let result = evaluate_bash_command("perl -i -pe 's/foo/bar/g' file.txt", false).unwrap();
        assert!(result.should_checkpoint);
    }

    #[test]
    fn test_npm_install_blacklisted() {
        // npm (the base command) is blacklisted since most npm commands
        // only modify node_modules which is in the default ignore list
        let result = evaluate_bash_command("npm install express", false).unwrap();
        assert!(!result.should_checkpoint);
    }

    #[test]
    fn test_cargo_build_blacklisted() {
        let result = evaluate_bash_command("cargo build", false).unwrap();
        assert!(!result.should_checkpoint);
    }
}
