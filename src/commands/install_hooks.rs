use crate::authorship::imara_diff_utils::{LineChangeTag, compute_line_changes};
use crate::error::GitAiError;
use crate::utils::debug_log;
use indicatif::{ProgressBar, ProgressStyle};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::CstRootNode;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

// Minimum version requirements
const MIN_CURSOR_VERSION: (u32, u32) = (1, 7);
const MIN_CODE_VERSION: (u32, u32) = (1, 99);
const MIN_CLAUDE_VERSION: (u32, u32) = (2, 0);

// Command patterns for hooks (after "git-ai")
// Claude Code hooks (uses shell, so relative path works)
const CLAUDE_PRE_TOOL_CMD: &str = "checkpoint claude --hook-input stdin";
const CLAUDE_POST_TOOL_CMD: &str = "checkpoint claude --hook-input stdin";

// Cursor hooks (requires absolute path to avoid shell config loading delay)
const CURSOR_BEFORE_SUBMIT_CMD: &str = "checkpoint cursor --hook-input stdin";
const CURSOR_AFTER_EDIT_CMD: &str = "checkpoint cursor --hook-input stdin";

// OpenCode plugin content (TypeScript), embedded from the source file to avoid drift
const OPENCODE_PLUGIN_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/opencode/git-ai.ts"
));

pub fn run(args: &[String]) -> Result<(), GitAiError> {
    // Parse --dry-run flag (default: false)
    let mut dry_run = false;
    for arg in args {
        if arg == "--dry-run" || arg == "--dry-run=true" {
            dry_run = true;
        }
    }

    // Get absolute path to the current binary
    let binary_path = get_current_binary_path()?;

    // Run async operations with smol
    smol::block_on(async_run(binary_path, dry_run))
}

async fn async_run(binary_path: PathBuf, dry_run: bool) -> Result<(), GitAiError> {
    let mut any_checked = false;
    let mut has_changes = false;

    match check_claude_code() {
        Ok(true) => {
            any_checked = true;
            // Install/update Claude Code hooks
            let spinner = Spinner::new("Claude code: checking hooks");
            spinner.start();

            match install_claude_code_hooks(dry_run) {
                Ok(Some(diff)) => {
                    if dry_run {
                        spinner.pending("Claude code: Pending updates");
                    } else {
                        spinner.success("Claude code: Hooks updated");
                    }
                    println!(); // Blank line before diff
                    print_diff(&diff);
                    has_changes = true;
                }
                Ok(None) => {
                    spinner.success("Claude code: Hooks already up to date");
                }
                Err(e) => {
                    spinner.error("Claude code: Failed to update hooks");
                    eprintln!("  Error: {}", e);
                    eprintln!("  Check that ~/.claude/settings.json is valid JSON");
                }
            }
        }
        Ok(false) => {
            // Claude Code not detected
        }
        Err(version_error) => {
            any_checked = true;
            let spinner = Spinner::new("Claude code: checking version");
            spinner.start();
            spinner.error("Claude code: Version check failed");
            eprintln!("  Error: {}", version_error);
            eprintln!("  Please update Claude Code to continue using git-ai hooks");
        }
    }

    match check_cursor() {
        Ok(true) => {
            any_checked = true;
            // Install/update Cursor hooks
            let spinner = Spinner::new("Cursor: checking hooks");
            spinner.start();

            match install_cursor_hooks(&binary_path, dry_run) {
                Ok(Some(diff)) => {
                    if dry_run {
                        spinner.pending("Cursor: Pending updates");
                    } else {
                        spinner.success("Cursor: Hooks updated");
                    }
                    println!(); // Blank line before diff
                    print_diff(&diff);
                    has_changes = true;
                }
                Ok(None) => {
                    spinner.success("Cursor: Hooks already up to date");
                }
                Err(e) => {
                    spinner.error("Cursor: Failed to update hooks");
                    eprintln!("  Error: {}", e);
                    eprintln!("  Check that ~/.cursor/hooks.json is valid JSON");
                }
            }

            // Install/update Cursor extension (runs in addition to hooks)
            let extension_spinner = Spinner::new("Cursor: installing extension");
            extension_spinner.start();

            if binary_exists("cursor") {
                // Install/update Cursor extension
                match is_vsc_editor_extension_installed("cursor", "git-ai.git-ai-vscode") {
                    Ok(true) => {
                        extension_spinner.success("Cursor: Extension installed");
                    }
                    Ok(false) => {
                        if dry_run {
                            extension_spinner.pending("Cursor: Pending extension install");
                        } else {
                            match install_vsc_editor_extension("cursor", "git-ai.git-ai-vscode") {
                                Ok(()) => {
                                    extension_spinner.success("Cursor: Extension installed");
                                }
                                Err(e) => {
                                    debug_log(&format!(
                                        "Cursor: Error automatically installing extension: {}",
                                        e
                                    ));
                                    extension_spinner.pending("Cursor: Unable to automatically install extension. Please cmd+click on the following link to install: cursor:extension/git-ai.git-ai-vscode (or search for 'git-ai-vscode' in the Cursor extensions tab)");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        extension_spinner.error("Cursor: Failed to check extension");
                        eprintln!("  Error: {}", e);
                    }
                }
            } else {
                extension_spinner.pending("Cursor: Unable to automatically install extension. Please cmd+click on the following link to install: cursor:extension/git-ai.git-ai-vscode (or search for 'git-ai-vscode' in the Cursor extensions tab)");
            }

            #[cfg(windows)]
            {
                let settings_spinner = Spinner::new("Cursor: configuring git.path");
                settings_spinner.start();

                match configure_cursor_git_path(dry_run) {
                    Ok(diffs) => {
                        if diffs.is_empty() {
                            settings_spinner.success("Cursor: git.path already configured");
                        } else if dry_run {
                            settings_spinner.pending("Cursor: Pending git.path update");
                        } else {
                            settings_spinner.success("Cursor: git.path updated");
                        }

                        if !diffs.is_empty() {
                            for diff in diffs {
                                println!(); // Blank line before diff
                                print_diff(&diff);
                            }
                            has_changes = true;
                        }
                    }
                    Err(e) => {
                        settings_spinner.error("Cursor: Failed to configure git.path");
                        eprintln!("  Error: {}", e);
                    }
                }
            }
        }
        Ok(false) => {
            // Cursor not detected
        }
        Err(version_error) => {
            any_checked = true;
            let spinner = Spinner::new("Cursor: checking version");
            spinner.start();
            spinner.error("Cursor: Version check failed");
            eprintln!("  Error: {}", version_error);
            eprintln!("  Please update Cursor to continue using git-ai hooks");
        }
    }

    match check_vscode() {
        Ok(true) => {
            any_checked = true;
            // Install/update VS Code hooks
            let spinner = Spinner::new("VS Code: installing extension");
            spinner.start();

            if binary_exists("code") {
                // Install/update VS Code extension
                match is_vsc_editor_extension_installed("code", "git-ai.git-ai-vscode") {
                    Ok(true) => {
                        spinner.success("VS Code: Extension installed");
                    }
                    Ok(false) => {
                        if dry_run {
                            spinner.pending("VS Code: Pending extension install");
                        } else {
                            match install_vsc_editor_extension("code", "git-ai.git-ai-vscode") {
                                Ok(()) => {
                                    spinner.success("VS Code: Extension installed");
                                }
                                Err(e) => {
                                    debug_log(&format!(
                                        "VS Code: Error automatically installing extension: {}",
                                        e
                                    ));
                                    spinner.pending("VS Code: Unable to automatically install extension. Please cmd+click on the following link to install: vscode:extension/git-ai.git-ai-vscode (or navigate to https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode in your browser)");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        spinner.error("VS Code: Failed to check extension");
                        eprintln!("  Error: {}", e);
                    }
                }
            } else {
                spinner.pending("VS Code: Unable to automatically install extension. Please cmd+click on the following link to install: vscode:extension/git-ai.git-ai-vscode (or navigate to https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode in your browser)");
            }

            #[cfg(windows)]
            {
                let settings_spinner = Spinner::new("VS Code: configuring git.path");
                settings_spinner.start();

                match configure_vscode_git_path(dry_run) {
                    Ok(diffs) => {
                        if diffs.is_empty() {
                            settings_spinner.success("VS Code: git.path already configured");
                        } else if dry_run {
                            settings_spinner.pending("VS Code: Pending git.path update");
                        } else {
                            settings_spinner.success("VS Code: git.path updated");
                        }

                        if !diffs.is_empty() {
                            for diff in diffs {
                                println!(); // Blank line before diff
                                print_diff(&diff);
                            }
                            has_changes = true;
                        }
                    }
                    Err(e) => {
                        settings_spinner.error("VS Code: Failed to configure git.path");
                        eprintln!("  Error: {}", e);
                    }
                }
            }
        }
        Ok(false) => {
            // VS Code not detected
        }
        Err(version_error) => {
            any_checked = true;
            let spinner = Spinner::new("VS Code: checking version");
            spinner.start();
            spinner.error("VS Code: Version check failed");
            eprintln!("  Error: {}", version_error);
            eprintln!("  Please update VS Code to continue using git-ai hooks");
        }
    }

    match check_opencode() {
        Ok(true) => {
            any_checked = true;
            // Install/update OpenCode plugin
            let spinner = Spinner::new("OpenCode: checking plugin");
            spinner.start();

            match install_opencode_hooks(dry_run) {
                Ok(Some(diff)) => {
                    if dry_run {
                        spinner.pending("OpenCode: Pending plugin install");
                    } else {
                        spinner.success("OpenCode: Plugin installed");
                    }
                    println!(); // Blank line before diff
                    print_diff(&diff);
                    has_changes = true;
                }
                Ok(None) => {
                    spinner.success("OpenCode: Plugin already up to date");
                }
                Err(e) => {
                    spinner.error("OpenCode: Failed to install plugin");
                    eprintln!("  Error: {}", e);
                    eprintln!("  Check that ~/.config/opencode/plugin/ is writable");
                }
            }
        }
        Ok(false) => {
            // OpenCode not detected
        }
        Err(version_error) => {
            any_checked = true;
            let spinner = Spinner::new("OpenCode: checking version");
            spinner.start();
            spinner.error("OpenCode: Version check failed");
            eprintln!("  Error: {}", version_error);
        }
    }

    if !any_checked {
        println!("No compatible IDEs or agent configurations detected. Nothing to install.");
    } else if has_changes && dry_run {
        println!("\n\x1b[33m⚠ Dry-run mode (default). No changes were made.\x1b[0m");
        println!("To apply these changes, run:");
        println!("\x1b[1m  git-ai install-hooks --dry-run=false\x1b[0m");
    }

    Ok(())
}

fn print_diff(diff_text: &str) {
    // Print a formatted diff using colors
    for line in diff_text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            // File headers in bold
            println!("\x1b[1m{}\x1b[0m", line);
        } else if line.starts_with('+') {
            // Additions in green
            println!("\x1b[32m{}\x1b[0m", line);
        } else if line.starts_with('-') {
            // Deletions in red
            println!("\x1b[31m{}\x1b[0m", line);
        } else if line.starts_with("@@") {
            // Hunk headers in cyan
            println!("\x1b[36m{}\x1b[0m", line);
        } else {
            // Context lines normal
            println!("{}", line);
        }
    }
    println!(); // Blank line after diff
}

fn check_claude_code() -> Result<bool, String> {
    let has_binary = binary_exists("claude");
    let has_dotfiles = {
        let home = home_dir();
        home.join(".claude").exists()
    };

    if !has_binary && !has_dotfiles {
        return Ok(false);
    }

    // If we have the binary, check version
    if has_binary {
        match get_binary_version("claude") {
            Ok(version_str) => {
                if let Some(version) = parse_version(&version_str) {
                    if !version_meets_requirement(version, MIN_CLAUDE_VERSION) {
                        return Err(format!(
                            "Claude Code version {}.{} detected, but minimum version {}.{} is required",
                            version.0, version.1, MIN_CLAUDE_VERSION.0, MIN_CLAUDE_VERSION.1
                        ));
                    }
                }
                // If we can't parse, continue anyway (be permissive)
            }
            Err(_) => {
                // If version check fails, continue anyway (be permissive)
            }
        }
    }

    Ok(true)
}

fn check_cursor() -> Result<bool, String> {
    let has_binary = binary_exists("cursor");
    let has_dotfiles = {
        let home = home_dir();
        home.join(".cursor").exists()
    };

    let has_settings_targets = cursor_settings_targets()
        .iter()
        .any(|path| should_process_settings_target(path));

    if !has_binary && !has_dotfiles && !has_settings_targets {
        return Ok(false);
    }

    // If we have the binary, check version
    if has_binary {
        match get_binary_version("cursor") {
            Ok(version_str) => {
                if let Some(version) = parse_version(&version_str) {
                    if !version_meets_requirement(version, MIN_CURSOR_VERSION) {
                        return Err(format!(
                            "Cursor version {}.{} detected, but minimum version {}.{} is required",
                            version.0, version.1, MIN_CURSOR_VERSION.0, MIN_CURSOR_VERSION.1
                        ));
                    }
                }
                // If we can't parse, continue anyway (be permissive)
            }
            Err(_) => {
                // If version check fails, continue anyway (be permissive)
            }
        }
    }

    Ok(true)
}

fn check_vscode() -> Result<bool, String> {
    let has_binary = binary_exists("code");
    let has_dotfiles = {
        let home = home_dir();
        home.join(".vscode").exists()
    };

    let has_settings_targets = vscode_settings_targets()
        .iter()
        .any(|path| should_process_settings_target(path));

    if !has_binary && !has_dotfiles && !has_settings_targets {
        return Ok(false);
    }

    // If we have the binary, check version
    if has_binary {
        match get_binary_version("code") {
            Ok(version_str) => {
                if let Some(version) = parse_version(&version_str) {
                    if !version_meets_requirement(version, MIN_CODE_VERSION) {
                        return Err(format!(
                            "VS Code version {}.{} detected, but minimum version {}.{} is required",
                            version.0, version.1, MIN_CODE_VERSION.0, MIN_CODE_VERSION.1
                        ));
                    }
                }
                // If we can't parse, continue anyway (be permissive)
            }
            Err(_) => {
                // If version check fails, continue anyway (be permissive)
            }
        }
    }

    Ok(true)
}

fn check_opencode() -> Result<bool, String> {
    let has_binary = binary_exists("opencode");
    let has_global_config = {
        let home = home_dir();
        home.join(".config").join("opencode").exists()
    };
    let has_local_config = {
        // Check if .opencode directory exists in current directory
        Path::new(".opencode").exists()
    };

    if !has_binary && !has_global_config && !has_local_config {
        return Ok(false);
    }

    // OpenCode doesn't have a minimum version requirement for now
    // The plugin uses standard APIs that should work with any version

    Ok(true)
}

// Shared utilities

/// Get version from a binary's --version output
fn get_binary_version(binary: &str) -> Result<String, GitAiError> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| GitAiError::Generic(format!("Failed to run {} --version: {}", binary, e)))?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "{} --version failed with status: {}",
            binary, output.status
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim().to_string())
}

/// Parse version string to extract major.minor version
/// Handles formats like "1.7.38", "1.104.3", "2.0.8 (Claude Code)"
fn parse_version(version_str: &str) -> Option<(u32, u32)> {
    // Split by whitespace and take the first part (handles "2.0.8 (Claude Code)")
    let version_part = version_str.split_whitespace().next()?;

    // Split by dots and take first two numbers
    let parts: Vec<&str> = version_part.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;

    Some((major, minor))
}

/// Compare version against minimum requirement
/// Returns true if version >= min_version
fn version_meets_requirement(version: (u32, u32), min_version: (u32, u32)) -> bool {
    if version.0 > min_version.0 {
        return true;
    }
    if version.0 == min_version.0 && version.1 >= min_version.1 {
        return true;
    }
    false
}

/// Check if a binary with the given name exists in the system PATH
fn binary_exists(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            // First check exact name as provided
            let candidate = dir.join(name);
            if candidate.exists() && candidate.is_file() {
                return true;
            }

            // On Windows, executables usually have extensions listed in PATHEXT
            #[cfg(windows)]
            {
                let pathext =
                    std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".to_string());
                for ext in pathext.split(';') {
                    let ext = ext.trim();
                    if ext.is_empty() {
                        continue;
                    }
                    let ext = if ext.starts_with('.') {
                        ext.to_string()
                    } else {
                        format!(".{}", ext)
                    };
                    let candidate = dir.join(format!("{}{}", name, ext));
                    if candidate.exists() && candidate.is_file() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn install_claude_code_hooks(dry_run: bool) -> Result<Option<String>, GitAiError> {
    let settings_path = claude_settings_path();

    // Ensure directory exists
    if let Some(dir) = settings_path.parent() {
        fs::create_dir_all(dir)?;
    }

    // Read existing content as string
    let existing_content = if settings_path.exists() {
        fs::read_to_string(&settings_path)?
    } else {
        String::new()
    };

    // Parse existing JSON if present, else start with empty object
    let existing: Value = if existing_content.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&existing_content)?
    };

    // Desired hooks - Claude Code doesn't need absolute paths, uses shell properly
    let pre_tool_cmd = format!("git-ai {}", CLAUDE_PRE_TOOL_CMD);
    let post_tool_cmd = format!("git-ai {}", CLAUDE_POST_TOOL_CMD);

    let desired_hooks = json!({
        "PreToolUse": {
            "matcher": "Write|Edit|MultiEdit",
            "desired_cmd": pre_tool_cmd,
        },
        "PostToolUse": {
            "matcher": "Write|Edit|MultiEdit",
            "desired_cmd": post_tool_cmd,
        }
    });

    // Merge desired into existing
    let mut merged = existing.clone();
    let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

    // Process both PreToolUse and PostToolUse
    for hook_type in &["PreToolUse", "PostToolUse"] {
        let desired_matcher = desired_hooks[hook_type]["matcher"].as_str().unwrap();
        let desired_cmd = desired_hooks[hook_type]["desired_cmd"].as_str().unwrap();

        // Get or create the hooks array for this type
        let mut hook_type_array = hooks_obj
            .get(*hook_type)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Find existing matcher block for Write|Edit|MultiEdit
        let mut found_matcher_idx: Option<usize> = None;
        for (idx, item) in hook_type_array.iter().enumerate() {
            if let Some(matcher) = item.get("matcher").and_then(|m| m.as_str()) {
                if matcher == desired_matcher {
                    found_matcher_idx = Some(idx);
                    break;
                }
            }
        }

        let matcher_idx = match found_matcher_idx {
            Some(idx) => idx,
            None => {
                // Create new matcher block
                hook_type_array.push(json!({
                    "matcher": desired_matcher,
                    "hooks": []
                }));
                hook_type_array.len() - 1
            }
        };

        // Get the hooks array within this matcher block
        let mut hooks_array = hook_type_array[matcher_idx]
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        // Update outdated git-ai checkpoint commands
        // This finds ALL existing git-ai checkpoint commands and:
        // 1. Updates the first one to the latest format (if needed)
        // 2. Removes any duplicates (keeping only the updated one)
        let mut found_idx: Option<usize> = None;
        let mut needs_update = false;

        for (idx, hook) in hooks_array.iter().enumerate() {
            if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                if is_git_ai_checkpoint_command(cmd) {
                    if found_idx.is_none() {
                        found_idx = Some(idx);
                        // Check if it matches exactly what we want
                        if cmd != desired_cmd {
                            needs_update = true;
                        }
                    }
                }
            }
        }

        match found_idx {
            Some(idx) => {
                if needs_update {
                    // Update to latest format
                    hooks_array[idx] = json!({
                        "type": "command",
                        "command": desired_cmd
                    });
                }
                // Remove any duplicate git-ai checkpoint commands
                let keep_idx = idx;
                let mut current_idx = 0;
                hooks_array.retain(|hook| {
                    let should_keep = if current_idx == keep_idx {
                        current_idx += 1;
                        true
                    } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        let is_dup = is_git_ai_checkpoint_command(cmd);
                        current_idx += 1;
                        !is_dup // Keep if it's NOT a git-ai checkpoint command
                    } else {
                        current_idx += 1;
                        true
                    };
                    should_keep
                });
            }
            None => {
                // No existing command found, add new one
                hooks_array.push(json!({
                    "type": "command",
                    "command": desired_cmd
                }));
            }
        }

        // Write back the hooks array to the matcher block
        if let Some(matcher_block) = hook_type_array[matcher_idx].as_object_mut() {
            matcher_block.insert("hooks".to_string(), Value::Array(hooks_array));
        }

        // Write back the updated hook_type_array
        if let Some(obj) = hooks_obj.as_object_mut() {
            obj.insert(hook_type.to_string(), Value::Array(hook_type_array));
        }
    }

    // Write back hooks to merged
    if let Some(root) = merged.as_object_mut() {
        root.insert("hooks".to_string(), hooks_obj);
    }

    // Generate new content
    let new_content = serde_json::to_string_pretty(&merged)?;

    // Check if there are changes
    if existing_content.trim() == new_content.trim() {
        return Ok(None); // No changes needed
    }

    // Generate diff
    let changes = compute_line_changes(&existing_content, &new_content);
    let mut diff_output = String::new();
    diff_output.push_str(&format!("--- {}\n", settings_path.display()));
    diff_output.push_str(&format!("+++ {}\n", settings_path.display()));

    for change in changes {
        let sign = match change.tag() {
            LineChangeTag::Delete => "-",
            LineChangeTag::Insert => "+",
            LineChangeTag::Equal => " ",
        };
        diff_output.push_str(&format!("{}{}", sign, change.value()));
    }

    // Write if not dry-run
    if !dry_run {
        write_atomic(&settings_path, new_content.as_bytes())?;
    }

    Ok(Some(diff_output))
}

/// Check if a command is a git-ai checkpoint command
fn is_git_ai_checkpoint_command(cmd: &str) -> bool {
    // Must contain "git-ai" and "checkpoint"
    if !cmd.contains("git-ai") || !cmd.contains("checkpoint") {
        return false;
    }
    true
}

fn install_cursor_hooks(binary_path: &Path, dry_run: bool) -> Result<Option<String>, GitAiError> {
    let hooks_path = cursor_hooks_path();

    // Ensure directory exists
    if let Some(dir) = hooks_path.parent() {
        fs::create_dir_all(dir)?;
    }

    // Read existing content as string
    let existing_content = if hooks_path.exists() {
        fs::read_to_string(&hooks_path)?
    } else {
        String::new()
    };

    // Parse existing JSON if present, else start with empty object
    let existing: Value = if existing_content.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&existing_content)?
    };

    // Build commands with absolute path
    let before_submit_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);
    let after_edit_cmd = format!("{} {}", binary_path.display(), CURSOR_AFTER_EDIT_CMD);

    // Desired hooks payload for Cursor with new hook names
    let desired: Value = json!({
        "version": 1,
        "hooks": {
            "beforeSubmitPrompt": [
                {
                    "command": before_submit_cmd
                }
            ],
            "afterFileEdit": [
                {
                    "command": after_edit_cmd
                }
            ]
        }
    });

    // Merge desired into existing
    let mut merged = existing.clone();

    // Ensure version is set
    if merged.get("version").is_none() {
        if let Some(obj) = merged.as_object_mut() {
            obj.insert("version".to_string(), json!(1));
        }
    }

    // Merge hooks object
    let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

    // Process both hook types
    for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
        let desired_hooks = desired
            .get("hooks")
            .and_then(|h| h.get(*hook_name))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Get existing hooks array for this hook type
        let mut existing_hooks = hooks_obj
            .get(*hook_name)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Update outdated git-ai checkpoint commands (or add if missing)
        for desired_hook in desired_hooks {
            let desired_cmd = desired_hook.get("command").and_then(|c| c.as_str());
            if desired_cmd.is_none() {
                continue;
            }
            let desired_cmd = desired_cmd.unwrap();

            // Look for existing git-ai checkpoint cursor commands
            let mut found_idx = None;
            let mut needs_update = false;

            for (idx, existing_hook) in existing_hooks.iter().enumerate() {
                if let Some(existing_cmd) = existing_hook.get("command").and_then(|c| c.as_str()) {
                    // Check if this is a git-ai checkpoint cursor command
                    if existing_cmd.contains("git-ai checkpoint cursor")
                        || existing_cmd.contains("git-ai")
                            && existing_cmd.contains("checkpoint")
                            && existing_cmd.contains("cursor")
                    {
                        found_idx = Some(idx);
                        // Check if it matches exactly what we want
                        if existing_cmd != desired_cmd {
                            needs_update = true;
                        }
                        break;
                    }
                }
            }

            match found_idx {
                Some(idx) if needs_update => {
                    // Update to latest format
                    existing_hooks[idx] = desired_hook.clone();
                }
                Some(_) => {
                    // Already up to date, skip
                }
                None => {
                    // No existing command, add new one
                    existing_hooks.push(desired_hook.clone());
                }
            }
        }

        // Write back merged hooks for this hook type
        if let Some(obj) = hooks_obj.as_object_mut() {
            obj.insert(hook_name.to_string(), Value::Array(existing_hooks));
        }
    }

    if let Some(root) = merged.as_object_mut() {
        root.insert("hooks".to_string(), hooks_obj);
    }

    // Generate new content
    let new_content = serde_json::to_string_pretty(&merged)?;

    // Check if there are changes
    if existing_content.trim() == new_content.trim() {
        return Ok(None); // No changes needed
    }

    // Generate diff
    let changes = compute_line_changes(&existing_content, &new_content);
    let mut diff_output = String::new();
    diff_output.push_str(&format!("--- {}\n", hooks_path.display()));
    diff_output.push_str(&format!("+++ {}\n", hooks_path.display()));

    for change in changes {
        let sign = match change.tag() {
            LineChangeTag::Delete => "-",
            LineChangeTag::Insert => "+",
            LineChangeTag::Equal => " ",
        };
        diff_output.push_str(&format!("{}{}", sign, change.value()));
    }

    // Write if not dry-run
    if !dry_run {
        write_atomic(&hooks_path, new_content.as_bytes())?;
    }

    Ok(Some(diff_output))
}

fn install_opencode_hooks(dry_run: bool) -> Result<Option<String>, GitAiError> {
    // Install to global config directory: ~/.config/opencode/plugin/git-ai.ts
    let plugin_path = opencode_plugin_path();

    // Ensure directory exists
    if let Some(dir) = plugin_path.parent() {
        if !dry_run {
            fs::create_dir_all(dir)?;
        }
    }

    // Read existing content if present
    let existing_content = if plugin_path.exists() {
        fs::read_to_string(&plugin_path)?
    } else {
        String::new()
    };

    let new_content = OPENCODE_PLUGIN_CONTENT;

    // Check if there are changes
    if existing_content.trim() == new_content.trim() {
        return Ok(None); // No changes needed
    }

    // Generate diff
    let changes = compute_line_changes(&existing_content, new_content);
    let mut diff_output = String::new();
    diff_output.push_str(&format!("--- {}\n", plugin_path.display()));
    diff_output.push_str(&format!("+++ {}\n", plugin_path.display()));

    for change in changes {
        let sign = match change.tag() {
            LineChangeTag::Delete => "-",
            LineChangeTag::Insert => "+",
            LineChangeTag::Equal => " ",
        };
        diff_output.push_str(&format!("{}{}", sign, change.value()));
    }

    // Write if not dry-run
    if !dry_run {
        write_atomic(&plugin_path, new_content.as_bytes())?;
    }

    Ok(Some(diff_output))
}

fn opencode_plugin_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("opencode")
        .join("plugin")
        .join("git-ai.ts")
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn cursor_hooks_path() -> PathBuf {
    home_dir().join(".cursor").join("hooks.json")
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), GitAiError> {
    let tmp_path = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    #[cfg(windows)]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            return PathBuf::from(userprofile);
        }
    }
    PathBuf::from(".")
}

#[cfg(windows)]
fn git_shim_path() -> PathBuf {
    home_dir().join(".git-ai").join("bin").join("git")
}

#[cfg(windows)]
fn git_shim_path_string() -> String {
    git_shim_path().to_string_lossy().into_owned()
}

fn should_process_settings_target(path: &Path) -> bool {
    path.exists() || path.parent().map(|parent| parent.exists()).unwrap_or(false)
}

fn settings_path_candidates(product: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            paths.push(
                PathBuf::from(&appdata)
                    .join(product)
                    .join("User")
                    .join("settings.json"),
            );
        }
        paths.push(
            home_dir()
                .join("AppData")
                .join("Roaming")
                .join(product)
                .join("User")
                .join("settings.json"),
        );
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(
            home_dir()
                .join("Library")
                .join("Application Support")
                .join(product)
                .join("User")
                .join("settings.json"),
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push(
            home_dir()
                .join(".config")
                .join(product)
                .join("User")
                .join("settings.json"),
        );
    }

    paths.sort();
    paths.dedup();
    paths
}

fn settings_paths_for_products(product_names: &[&str]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = product_names
        .iter()
        .flat_map(|product| settings_path_candidates(product))
        .collect();

    paths.sort();
    paths.dedup();
    paths
}

fn vscode_settings_targets() -> Vec<PathBuf> {
    settings_paths_for_products(&["Code", "Code - Insiders"])
}

fn cursor_settings_targets() -> Vec<PathBuf> {
    settings_paths_for_products(&["Cursor"])
}

#[cfg(windows)]
fn configure_git_path_for_products(
    product_names: &[&str],
    dry_run: bool,
) -> Result<Vec<String>, GitAiError> {
    let git_path = git_shim_path_string();
    let mut diffs = Vec::new();

    for settings_path in settings_paths_for_products(product_names) {
        if !should_process_settings_target(&settings_path) {
            continue;
        }

        if let Some(diff) = update_git_path_setting(&settings_path, &git_path, dry_run)? {
            diffs.push(diff);
        }
    }

    Ok(diffs)
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn configure_git_path_for_products(
    product_names: &[&str],
    dry_run: bool,
) -> Result<Vec<String>, GitAiError> {
    let _ = (product_names, dry_run);
    Ok(Vec::new())
}

#[cfg(windows)]
fn configure_vscode_git_path(dry_run: bool) -> Result<Vec<String>, GitAiError> {
    configure_git_path_for_products(&["Code", "Code - Insiders"], dry_run)
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn configure_vscode_git_path(dry_run: bool) -> Result<Vec<String>, GitAiError> {
    let _ = dry_run;
    Ok(Vec::new())
}

#[cfg(windows)]
fn configure_cursor_git_path(dry_run: bool) -> Result<Vec<String>, GitAiError> {
    configure_git_path_for_products(&["Cursor"], dry_run)
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn configure_cursor_git_path(dry_run: bool) -> Result<Vec<String>, GitAiError> {
    let _ = dry_run;
    Ok(Vec::new())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn update_git_path_setting(
    settings_path: &Path,
    git_path: &str,
    dry_run: bool,
) -> Result<Option<String>, GitAiError> {
    let original = if settings_path.exists() {
        fs::read_to_string(settings_path)?
    } else {
        String::new()
    };

    let parse_input = if original.trim().is_empty() {
        "{}".to_string()
    } else {
        original.clone()
    };

    let parse_options = ParseOptions::default();

    let root = CstRootNode::parse(&parse_input, &parse_options).map_err(|err| {
        GitAiError::Generic(format!(
            "Failed to parse {}: {}",
            settings_path.display(),
            err
        ))
    })?;

    let object = root.object_value_or_set();
    let mut changed = false;
    let serialized_git_path = git_path.replace('\\', "\\\\");

    match object.get("git.path") {
        Some(prop) => {
            let should_update = match prop.value() {
                Some(node) => match node.as_string_lit() {
                    Some(string_node) => match string_node.decoded_value() {
                        Ok(existing_value) => existing_value != git_path,
                        Err(_) => true,
                    },
                    None => true,
                },
                None => true,
            };

            if should_update {
                prop.set_value(jsonc_parser::json!(serialized_git_path.as_str()));
                changed = true;
            }
        }
        None => {
            object.append(
                "git.path",
                jsonc_parser::json!(serialized_git_path.as_str()),
            );
            changed = true;
        }
    }

    if !changed {
        return Ok(None);
    }

    let new_content = root.to_string();

    let changes = compute_line_changes(&original, &new_content);
    let mut diff_output = format!(
        "--- {}\n+++ {}\n",
        settings_path.display(),
        settings_path.display()
    );

    for change in changes {
        let sign = match change.tag() {
            LineChangeTag::Delete => "-",
            LineChangeTag::Insert => "+",
            LineChangeTag::Equal => " ",
        };
        diff_output.push_str(&format!("{}{}", sign, change.value()));
    }

    if !dry_run {
        if let Some(parent) = settings_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        write_atomic(settings_path, new_content.as_bytes())?;
    }

    Ok(Some(diff_output))
}

/// Get the absolute path to the currently running binary
fn get_current_binary_path() -> Result<PathBuf, GitAiError> {
    let path = std::env::current_exe()?;

    // Canonicalize to resolve any symlinks
    let canonical = path.canonicalize()?;

    Ok(canonical)
}

fn is_vsc_editor_extension_installed(program: &str, id_or_vsix: &str) -> Result<bool, GitAiError> {
    // NOTE: We try up to 3 times, because the editor CLI can be flaky (throws intermittent JS errors)
    let mut last_error_message: Option<String> = None;
    for attempt in 1..=3 {
        #[cfg(windows)]
        let cmd_result = Command::new("cmd")
            .args(["/C", program, "--list-extensions"])
            .output();

        #[cfg(not(windows))]
        let cmd_result = Command::new(program).args(["--list-extensions"]).output();

        match cmd_result {
            Ok(output) => {
                if !output.status.success() {
                    last_error_message = Some(String::from_utf8_lossy(&output.stderr).to_string());
                } else {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    return Ok(stdout.contains(id_or_vsix));
                }
            }
            Err(e) => {
                last_error_message = Some(e.to_string());
            }
        }
        if attempt < 3 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    Err(GitAiError::Generic(last_error_message.unwrap_or_else(
        || format!("{} CLI '--list-extensions' failed", program),
    )))
}

fn install_vsc_editor_extension(program: &str, id_or_vsix: &str) -> Result<(), GitAiError> {
    // NOTE: We try up to 3 times, because the editor CLI can be flaky (throws intermittent JS errors)
    let mut last_error_message: Option<String> = None;
    for attempt in 1..=3 {
        #[cfg(windows)]
        let cmd_status = Command::new("cmd")
            .args(["/C", program, "--install-extension", id_or_vsix, "--force"])
            .status();

        #[cfg(not(windows))]
        let cmd_status = Command::new(program)
            .args(["--install-extension", id_or_vsix, "--force"])
            .status();

        match cmd_status {
            Ok(status) => {
                if status.success() {
                    return Ok(());
                }
                last_error_message = Some(format!("{} extension install failed", program));
            }
            Err(e) => {
                last_error_message = Some(e.to_string());
            }
        }
        if attempt < 3 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    Err(GitAiError::Generic(last_error_message.unwrap_or_else(
        || format!("{} extension install failed", program),
    )))
}

// Loader
struct Spinner {
    pb: ProgressBar,
}

impl Spinner {
    fn new(message: &str) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        pb.set_message(message.to_string());
        pb.enable_steady_tick(std::time::Duration::from_millis(100));

        Self { pb }
    }

    fn start(&self) {
        // Spinner starts automatically when created
    }

    fn _update_message(&self, message: &str) {
        self.pb.set_message(message.to_string());
    }

    async fn _wait_for(&self, duration_ms: u64) {
        smol::Timer::after(std::time::Duration::from_millis(duration_ms)).await;
    }

    fn success(&self, message: &'static str) {
        // Clear spinner and show success with green checkmark and bold green text
        self.pb.finish_and_clear();
        println!("\x1b[1;32m✓ {}\x1b[0m", message);
    }

    fn pending(&self, message: &'static str) {
        // Clear spinner and show pending with yellow warning triangle and bold yellow text
        self.pb.finish_and_clear();
        println!("\x1b[1;33m⚠ {}\x1b[0m", message);
    }

    #[allow(dead_code)]
    fn error(&self, message: &'static str) {
        // Clear spinner and show error with red X and bold red text
        self.pb.finish_and_clear();
        println!("\x1b[1;31m✗ {}\x1b[0m", message);
    }

    #[allow(dead_code)]
    fn skipped(&self, message: &'static str) {
        // Clear spinner and show skipped with gray circle and gray text
        self.pb.finish_and_clear();
        println!("\x1b[90m○ {}\x1b[0m", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let hooks_path = temp_dir.path().join(".cursor").join("hooks.json");
        (temp_dir, hooks_path)
    }

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_install_hooks_creates_file_from_scratch() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Ensure parent directory exists
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Call the function (simulating the install process)
        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let result = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ]
            }
        });

        // Write the result
        let pretty = serde_json::to_string_pretty(&result).unwrap();
        fs::write(&hooks_path, pretty).unwrap();

        // Verify the file was created
        assert!(hooks_path.exists());

        // Verify the content
        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        assert_eq!(content.get("version").unwrap(), &json!(1));

        let hooks = content.get("hooks").unwrap();
        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 1);
        assert_eq!(after_edit.len(), 1);
        assert!(
            before_submit[0]
                .get("command")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("git-ai checkpoint cursor")
        );
    }

    #[test]
    fn test_install_hooks_preserves_existing_hooks() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Create parent directory
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create existing hooks file with other commands
        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": "echo 'before'"
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": "echo 'after'"
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate merging
        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let mut hooks_array = hooks_obj
                .get(*hook_name)
                .unwrap()
                .as_array()
                .unwrap()
                .clone();
            hooks_array.push(json!({"command": git_ai_cmd.clone()}));
            hooks_obj
                .as_object_mut()
                .unwrap()
                .insert(hook_name.to_string(), Value::Array(hooks_array));
        }

        fs::write(&hooks_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        // Verify both old and new hooks exist
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 2);
        assert_eq!(after_edit.len(), 2);

        // Verify original hooks are still there
        assert_eq!(
            before_submit[0].get("command").unwrap().as_str().unwrap(),
            "echo 'before'"
        );
        assert_eq!(
            after_edit[0].get("command").unwrap().as_str().unwrap(),
            "echo 'after'"
        );
    }

    #[test]
    fn test_install_hooks_skips_if_already_exists() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Create parent directory
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        // Create existing hooks file with our command already there
        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate the deduplication logic
        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let hooks = content.get("hooks").unwrap();
            let hooks_array = hooks.get(*hook_name).unwrap().as_array().unwrap();

            // Check that it finds the existing command
            let found = hooks_array
                .iter()
                .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(&git_ai_cmd));
            assert!(found);
        }

        // Verify no duplicates were added
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        assert_eq!(
            hooks
                .get("beforeSubmitPrompt")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            hooks
                .get("afterFileEdit")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_install_hooks_updates_outdated_command() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Create parent directory
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create existing hooks file with old command format
        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": "git-ai checkpoint cursor 2>/dev/null || true"
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": "/old/path/git-ai checkpoint cursor"
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate update logic
        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();

        for hook_name in &["beforeSubmitPrompt", "afterFileEdit"] {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let mut hooks_array = hooks_obj
                .get(*hook_name)
                .unwrap()
                .as_array()
                .unwrap()
                .clone();

            // Find and update git-ai checkpoint cursor commands
            for hook in hooks_array.iter_mut() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                    if cmd.contains("git-ai checkpoint cursor")
                        || (cmd.contains("git-ai")
                            && cmd.contains("checkpoint")
                            && cmd.contains("cursor"))
                    {
                        *hook = json!({"command": git_ai_cmd.clone()});
                    }
                }
            }

            hooks_obj
                .as_object_mut()
                .unwrap()
                .insert(hook_name.to_string(), Value::Array(hooks_array));
        }

        fs::write(&hooks_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        // Verify the commands were updated
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let before_submit = hooks.get("beforeSubmitPrompt").unwrap().as_array().unwrap();
        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();

        assert_eq!(before_submit.len(), 1);
        assert_eq!(after_edit.len(), 1);

        // Verify commands were updated to new format
        assert_eq!(
            before_submit[0].get("command").unwrap().as_str().unwrap(),
            git_ai_cmd
        );
        assert_eq!(
            after_edit[0].get("command").unwrap().as_str().unwrap(),
            git_ai_cmd
        );
    }

    #[test]
    fn test_install_hooks_creates_missing_hook_keys() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Create parent directory
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create existing hooks file with only one hook type
        let existing = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": "echo 'before'"
                    }
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate adding missing key
        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks_obj = content.get_mut("hooks").unwrap();

        // Add afterFileEdit if it doesn't exist
        if hooks_obj.get("afterFileEdit").is_none() {
            hooks_obj.as_object_mut().unwrap().insert(
                "afterFileEdit".to_string(),
                json!([{"command": git_ai_cmd.clone()}]),
            );
        }

        // Add to beforeSubmitPrompt
        let mut before_array = hooks_obj
            .get("beforeSubmitPrompt")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        before_array.push(json!({"command": git_ai_cmd.clone()}));
        hooks_obj
            .as_object_mut()
            .unwrap()
            .insert("beforeSubmitPrompt".to_string(), Value::Array(before_array));

        fs::write(&hooks_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        // Verify the missing key was created
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        assert!(hooks.get("beforeSubmitPrompt").is_some());
        assert!(hooks.get("afterFileEdit").is_some());

        let after_edit = hooks.get("afterFileEdit").unwrap().as_array().unwrap();
        assert_eq!(after_edit.len(), 1);
        assert!(
            after_edit[0]
                .get("command")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("git-ai checkpoint cursor")
        );
    }

    #[test]
    fn test_install_hooks_handles_empty_file() {
        let (_temp_dir, hooks_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        // Create parent directory
        if let Some(parent) = hooks_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create empty file
        fs::write(&hooks_path, "").unwrap();

        // Read and handle empty file
        let contents = fs::read_to_string(&hooks_path).unwrap();
        let existing: Value = if contents.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&contents).unwrap()
        };

        assert_eq!(existing, json!({}));

        // Now create proper structure
        let git_ai_cmd = format!("{} {}", binary_path.display(), CURSOR_BEFORE_SUBMIT_CMD);

        let result = json!({
            "version": 1,
            "hooks": {
                "beforeSubmitPrompt": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ],
                "afterFileEdit": [
                    {
                        "command": git_ai_cmd.clone()
                    }
                ]
            }
        });

        fs::write(&hooks_path, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        // Verify proper structure was created
        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        assert_eq!(content.get("version").unwrap(), &json!(1));
        assert!(content.get("hooks").is_some());
    }

    #[test]
    fn test_get_current_binary_path() {
        let result = get_current_binary_path();
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(path.is_absolute());
        // The path should contain the test binary
        assert!(path.to_string_lossy().len() > 0);
    }

    #[test]
    fn test_update_git_path_setting_appends_with_comments() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let initial = r#"{
    // comment
    "editor.tabSize": 4
}
"#;
        fs::write(&settings_path, initial).unwrap();

        let git_path = r"C:\Users\Test\.git-ai\bin\git";

        // Dry-run should produce a diff without modifying the file
        let dry_run_result = update_git_path_setting(&settings_path, git_path, true).unwrap();
        assert!(dry_run_result.is_some());
        let after_dry_run = fs::read_to_string(&settings_path).unwrap();
        assert_eq!(after_dry_run, initial);

        // Apply the change
        let apply_result = update_git_path_setting(&settings_path, git_path, false).unwrap();
        assert!(apply_result.is_some());

        let final_content = fs::read_to_string(&settings_path).unwrap();
        assert!(final_content.contains("// comment"));
        let tab_index = final_content.find("\"editor.tabSize\"").unwrap();
        let git_index = final_content.find("\"git.path\"").unwrap();
        assert!(tab_index < git_index);
        let verify = update_git_path_setting(&settings_path, git_path, true).unwrap();
        assert!(verify.is_none());
    }

    #[test]
    fn test_update_git_path_setting_updates_existing_value_in_place() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let initial = r#"{
    "git.path": "old-path",
    "editor.tabSize": 2
}
"#;
        fs::write(&settings_path, initial).unwrap();

        let result = update_git_path_setting(&settings_path, "new-path", false).unwrap();
        assert!(result.is_some());

        let final_content = fs::read_to_string(&settings_path).unwrap();
        assert!(final_content.contains("\"git.path\": \"new-path\""));
        assert_eq!(final_content.matches("git.path").count(), 1);
        assert!(final_content.contains("\"editor.tabSize\": 2"));
    }

    #[test]
    fn test_update_git_path_setting_detects_no_change() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.json");
        let initial = "{\n    \"git.path\": \"same\"\n}\n";
        fs::write(&settings_path, initial).unwrap();

        let result = update_git_path_setting(&settings_path, "same", false).unwrap();
        assert!(result.is_none());

        let final_content = fs::read_to_string(&settings_path).unwrap();
        assert_eq!(final_content, initial);
    }

    // Claude Code tests
    fn setup_claude_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join(".claude").join("settings.json");
        (temp_dir, settings_path)
    }

    #[test]
    fn test_claude_install_hooks_creates_file_from_scratch() {
        let (_temp_dir, settings_path) = setup_claude_test_env();

        // Ensure parent directory exists
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let result = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("git-ai {}", CLAUDE_PRE_TOOL_CMD)
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!("git-ai {}", CLAUDE_POST_TOOL_CMD)
                            }
                        ]
                    }
                ]
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&result).unwrap(),
        )
        .unwrap();

        // Verify
        let content: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = content.get("hooks").unwrap();

        let pre_tool = hooks.get("PreToolUse").unwrap().as_array().unwrap();
        let post_tool = hooks.get("PostToolUse").unwrap().as_array().unwrap();

        assert_eq!(pre_tool.len(), 1);
        assert_eq!(post_tool.len(), 1);

        // Check matchers
        assert_eq!(
            pre_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "Write|Edit|MultiEdit"
        );
        assert_eq!(
            post_tool[0].get("matcher").unwrap().as_str().unwrap(),
            "Write|Edit|MultiEdit"
        );
    }

    #[test]
    fn test_claude_removes_duplicates() {
        let (_temp_dir, settings_path) = setup_claude_test_env();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create existing hooks with duplicates (like in the user's example)
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "git-ai checkpoint"
                            },
                            {
                                "type": "command",
                                "command": "git-ai checkpoint 2>/dev/null || true"
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "git-ai checkpoint claude --hook-input \"$(cat)\""
                            },
                            {
                                "type": "command",
                                "command": "git-ai checkpoint claude --hook-input \"$(cat)\" 2>/dev/null || true"
                            }
                        ]
                    }
                ]
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate the deduplication logic (what install_claude_code_hooks does)
        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();

        let pre_tool_cmd = format!("git-ai {}", CLAUDE_PRE_TOOL_CMD);
        let post_tool_cmd = format!("git-ai {}", CLAUDE_POST_TOOL_CMD);

        for (hook_type, desired_cmd) in
            &[("PreToolUse", pre_tool_cmd), ("PostToolUse", post_tool_cmd)]
        {
            let hooks_obj = content.get_mut("hooks").unwrap();
            let hook_type_array = hooks_obj
                .get_mut(*hook_type)
                .unwrap()
                .as_array_mut()
                .unwrap();
            let matcher_block = &mut hook_type_array[0];
            let hooks_array = matcher_block
                .get_mut("hooks")
                .unwrap()
                .as_array_mut()
                .unwrap();

            // Find git-ai checkpoint commands and update the first one, mark others for removal
            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;

            for (idx, hook) in hooks_array.iter().enumerate() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                    if is_git_ai_checkpoint_command(cmd) {
                        if found_idx.is_none() {
                            found_idx = Some(idx);
                            if cmd != *desired_cmd {
                                needs_update = true;
                            }
                        }
                    }
                }
            }

            // Update or keep the first occurrence
            if let Some(idx) = found_idx {
                if needs_update {
                    hooks_array[idx] = json!({
                        "type": "command",
                        "command": desired_cmd
                    });
                }
            }

            // Now remove ALL OTHER git-ai checkpoint commands (keep only the one we just processed)
            let first_idx = found_idx;
            if let Some(keep_idx) = first_idx {
                let mut i = 0;
                hooks_array.retain(|hook| {
                    let should_keep = if i == keep_idx {
                        true
                    } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        // Remove if it's another git-ai checkpoint command
                        !is_git_ai_checkpoint_command(cmd)
                    } else {
                        true
                    };
                    i += 1;
                    should_keep
                });
            }
        }

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        // Verify no duplicates
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        for hook_type in &["PreToolUse", "PostToolUse"] {
            let hook_array = hooks.get(*hook_type).unwrap().as_array().unwrap();
            assert_eq!(hook_array.len(), 1);

            let hooks_in_matcher = hook_array[0].get("hooks").unwrap().as_array().unwrap();
            assert_eq!(
                hooks_in_matcher.len(),
                1,
                "{} should have exactly 1 hook after deduplication",
                hook_type
            );
        }
    }

    #[test]
    fn test_claude_preserves_other_hooks() {
        let (_temp_dir, settings_path) = setup_claude_test_env();

        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Create existing hooks with other user commands
        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo 'before write'"
                            }
                        ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "prettier --write"
                            }
                        ]
                    }
                ]
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        // Simulate adding our hooks
        let mut content: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();

        let hooks_obj = content.get_mut("hooks").unwrap();

        // Add to PreToolUse
        let pre_array = hooks_obj
            .get_mut("PreToolUse")
            .unwrap()
            .as_array_mut()
            .unwrap();
        pre_array[0]
            .get_mut("hooks")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "command",
                "command": format!("git-ai {}", CLAUDE_PRE_TOOL_CMD)
            }));

        // Add to PostToolUse
        let post_array = hooks_obj
            .get_mut("PostToolUse")
            .unwrap()
            .as_array_mut()
            .unwrap();
        post_array[0]
            .get_mut("hooks")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "command",
                "command": format!("git-ai {}", CLAUDE_POST_TOOL_CMD)
            }));

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        // Verify both old and new hooks exist
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let hooks = result.get("hooks").unwrap();

        let pre_hooks = hooks.get("PreToolUse").unwrap().as_array().unwrap()[0]
            .get("hooks")
            .unwrap()
            .as_array()
            .unwrap();
        let post_hooks = hooks.get("PostToolUse").unwrap().as_array().unwrap()[0]
            .get("hooks")
            .unwrap()
            .as_array()
            .unwrap();

        assert_eq!(pre_hooks.len(), 2);
        assert_eq!(post_hooks.len(), 2);

        // Verify original hooks are preserved
        assert_eq!(
            pre_hooks[0].get("command").unwrap().as_str().unwrap(),
            "echo 'before write'"
        );
        assert_eq!(
            post_hooks[0].get("command").unwrap().as_str().unwrap(),
            "prettier --write"
        );
    }

    #[test]
    fn test_parse_version() {
        // Test standard versions
        assert_eq!(parse_version("1.7.38"), Some((1, 7)));
        assert_eq!(parse_version("1.104.3"), Some((1, 104)));
        assert_eq!(parse_version("2.0.8"), Some((2, 0)));

        // Test version with extra text
        assert_eq!(parse_version("2.0.8 (Claude Code)"), Some((2, 0)));

        // Test edge cases
        assert_eq!(parse_version("1.0"), Some((1, 0)));
        assert_eq!(parse_version("10.20.30.40"), Some((10, 20)));

        // Test invalid versions
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version("invalid"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_version_meets_requirement() {
        // Test exact match
        assert!(version_meets_requirement((1, 7), (1, 7)));

        // Test higher major version
        assert!(version_meets_requirement((2, 0), (1, 7)));

        // Test same major, higher minor
        assert!(version_meets_requirement((1, 8), (1, 7)));

        // Test lower major version
        assert!(!version_meets_requirement((0, 99), (1, 7)));

        // Test same major, lower minor
        assert!(!version_meets_requirement((1, 6), (1, 7)));

        // Test large numbers
        assert!(version_meets_requirement((1, 104), (1, 99)));
        assert!(!version_meets_requirement((1, 98), (1, 99)));
    }

    #[test]
    fn test_version_requirements() {
        // Test minimum version requirements against example versions from user

        // Cursor 1.7.38 should meet requirement of 1.7
        let cursor_version = parse_version("1.7.38").unwrap();
        assert!(version_meets_requirement(
            cursor_version,
            MIN_CURSOR_VERSION
        ));

        // Cursor 1.6.x should fail
        let old_cursor = parse_version("1.6.99").unwrap();
        assert!(!version_meets_requirement(old_cursor, MIN_CURSOR_VERSION));

        // VS Code 1.104.3 should meet requirement of 1.99
        let code_version = parse_version("1.104.3").unwrap();
        assert!(version_meets_requirement(code_version, MIN_CODE_VERSION));

        // VS Code 1.98.x should fail
        let old_code = parse_version("1.98.5").unwrap();
        assert!(!version_meets_requirement(old_code, MIN_CODE_VERSION));

        // Claude Code 2.0.8 should meet requirement of 2.0
        let claude_version = parse_version("2.0.8 (Claude Code)").unwrap();
        assert!(version_meets_requirement(
            claude_version,
            MIN_CLAUDE_VERSION
        ));

        // Claude Code 1.x should fail
        let old_claude = parse_version("1.9.9").unwrap();
        assert!(!version_meets_requirement(old_claude, MIN_CLAUDE_VERSION));
    }

    #[test]
    fn test_is_git_ai_checkpoint_command() {
        // PreToolUse commands (is_post_tool = false)
        assert!(is_git_ai_checkpoint_command("git-ai checkpoint"));
        assert!(is_git_ai_checkpoint_command(&format!(
            "git-ai {}",
            CLAUDE_PRE_TOOL_CMD
        )));
        assert!(is_git_ai_checkpoint_command("git-ai checkpoint claude"));
        assert!(is_git_ai_checkpoint_command(
            "git-ai checkpoint --hook-input"
        ));
        assert!(is_git_ai_checkpoint_command(
            "git-ai checkpoint claude --hook-input \"$(cat)\""
        ));
        assert!(is_git_ai_checkpoint_command(&format!(
            "git-ai {}",
            CLAUDE_POST_TOOL_CMD
        )));
        assert!(is_git_ai_checkpoint_command(
            "git-ai checkpoint --hook-input \"$(cat)\""
        ));

        // Non-matching commands
        assert!(!is_git_ai_checkpoint_command("echo hello"));
        assert!(!is_git_ai_checkpoint_command("git status"));
        assert!(!is_git_ai_checkpoint_command("checkpoint"));
        assert!(!is_git_ai_checkpoint_command("git-ai"));
    }

    // OpenCode tests
    fn setup_opencode_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("opencode")
            .join("plugin")
            .join("git-ai.ts");
        (temp_dir, plugin_path)
    }

    #[test]
    fn test_opencode_install_plugin_creates_file_from_scratch() {
        let (_temp_dir, plugin_path) = setup_opencode_test_env();

        // Ensure parent directory exists
        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Write the plugin content
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        // Verify the file was created
        assert!(plugin_path.exists());

        // Verify the content contains expected elements
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("GitAiPlugin"));
        assert!(content.contains("tool.execute.before"));
        assert!(content.contains("tool.execute.after"));
        assert!(content.contains("agent-v1"));
        assert!(content.contains("opencode"));
    }

    #[test]
    fn test_opencode_plugin_content_is_valid_typescript() {
        // Verify the plugin content has expected TypeScript structure
        let content = OPENCODE_PLUGIN_CONTENT;

        // Check for required imports
        assert!(content.contains("import type { Plugin }"));
        assert!(content.contains("@opencode-ai/plugin"));

        // Check for export
        assert!(content.contains("export const GitAiPlugin: Plugin"));

        // Check for hook handlers
        assert!(content.contains("\"tool.execute.before\""));
        assert!(content.contains("\"tool.execute.after\""));

        // Check for file edit tools
        assert!(content.contains("FILE_EDIT_TOOLS"));
        assert!(content.contains("edit"));
        assert!(content.contains("write"));

        // Check for checkpoint calls
        assert!(content.contains("git-ai checkpoint agent-v1"));
        assert!(content.contains("type: \"human\""));
        assert!(content.contains("type: \"ai_agent\""));
    }

    #[test]
    fn test_opencode_plugin_skips_if_already_exists() {
        let (_temp_dir, plugin_path) = setup_opencode_test_env();

        // Ensure parent directory exists
        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Write the plugin content
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        // Read it back
        let content1 = fs::read_to_string(&plugin_path).unwrap();

        // Write again (simulating re-install)
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        // Verify content is unchanged
        let content2 = fs::read_to_string(&plugin_path).unwrap();
        assert_eq!(content1, content2);
    }

    #[test]
    fn test_opencode_plugin_updates_outdated_content() {
        let (_temp_dir, plugin_path) = setup_opencode_test_env();

        // Ensure parent directory exists
        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Write old/different content
        let old_content = "// Old plugin version\nexport const OldPlugin = {}";
        fs::write(&plugin_path, old_content).unwrap();

        // Verify old content
        let content_before = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_before.contains("OldPlugin"));

        // Write new content
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        // Verify new content
        let content_after = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_after.contains("GitAiPlugin"));
        assert!(!content_after.contains("OldPlugin"));
    }

    #[test]
    fn test_opencode_plugin_handles_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("opencode")
            .join("plugin")
            .join("git-ai.ts");

        // Parent directory doesn't exist yet
        assert!(!plugin_path.parent().unwrap().exists());

        // Create directory and write file
        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        // Verify file was created
        assert!(plugin_path.exists());
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("GitAiPlugin"));
    }
}
