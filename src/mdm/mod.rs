//! MDM (Managed Device Management) module — table-driven auto-installer for AI coding tool hooks.
//!
//! Detects which AI coding tools are installed, and writes hook configurations so they fire
//! `git-ai checkpoint <agent>` on every file edit.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Config table
// ---------------------------------------------------------------------------

/// How a tool's hooks config file is structured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookFormat {
    /// Cursor-style: `{ "hooks": { "preToolUse": [...], "postToolUse": [...] } }`
    CursorStyle,
    /// Same structure as Cursor but keys are PascalCase: PreToolUse / PostToolUse
    PascalCursorStyle,
    /// Claude Code style: `{ "hooks": { "PreToolUse": [{matcher, hooks: [...]}], ... } }`
    ClaudeStyle,
    /// JetBrains External Tools XML format
    JetBrainsXml,
    /// Standard git hook scripts (for Fork, Sublime Merge, etc.)
    GitHookScript,
}

/// Static configuration for a single AI coding tool.
#[derive(Debug, Clone)]
struct AgentConfig {
    /// Tool/preset name (matches checkpoint agent name).
    name: &'static str,
    /// Config file path relative to $HOME. `None` means we can't auto-install yet.
    config_path: Option<&'static str>,
    /// Directory to check (relative to $HOME) to detect if the tool is installed.
    detect_dir: &'static str,
    /// Hook format variant.
    hook_format: Option<HookFormat>,
}

static AGENT_INSTALL_CONFIGS: &[AgentConfig] = &[
    AgentConfig {
        name: "cursor",
        config_path: Some(".cursor/hooks/hooks.json"),
        detect_dir: ".cursor",
        hook_format: Some(HookFormat::CursorStyle),
    },
    AgentConfig {
        name: "claude",
        config_path: Some(".claude/settings.json"),
        detect_dir: ".claude",
        hook_format: Some(HookFormat::ClaudeStyle),
    },
    AgentConfig {
        name: "windsurf",
        config_path: Some(".windsurf/hooks/hooks.json"),
        detect_dir: ".windsurf",
        hook_format: Some(HookFormat::CursorStyle),
    },
    AgentConfig {
        name: "amp",
        config_path: Some(".amp/hooks/hooks.json"),
        detect_dir: ".amp",
        hook_format: Some(HookFormat::PascalCursorStyle),
    },
    AgentConfig {
        name: "codex",
        config_path: Some(".codex/hooks/hooks.json"),
        detect_dir: ".codex",
        hook_format: Some(HookFormat::PascalCursorStyle),
    },
    AgentConfig {
        name: "gemini",
        config_path: None,
        detect_dir: ".gemini",
        hook_format: None,
    },
    AgentConfig {
        name: "pi",
        config_path: None,
        detect_dir: ".pi",
        hook_format: None,
    },
    AgentConfig {
        name: "opencode",
        config_path: None,
        detect_dir: ".opencode",
        hook_format: None,
    },
    AgentConfig {
        name: "droid",
        config_path: None,
        detect_dir: ".droid",
        hook_format: None,
    },
    AgentConfig {
        name: "github-copilot",
        config_path: None,
        detect_dir: ".github-copilot",
        hook_format: None,
    },
    AgentConfig {
        name: "firebender",
        config_path: None,
        detect_dir: ".firebender",
        hook_format: None,
    },
    AgentConfig {
        name: "continue-cli",
        config_path: None,
        detect_dir: ".continue",
        hook_format: None,
    },
    // VS Code — detected via .vscode dir; install uses `code --install-extension`
    AgentConfig {
        name: "vscode",
        config_path: None,
        detect_dir: ".vscode",
        hook_format: None,
    },
    // JetBrains IDEs — install External Tools XML
    AgentConfig {
        name: "intellij",
        config_path: Some(".config/JetBrains/IntelliJIdea/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/IntelliJIdea",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "webstorm",
        config_path: Some(".config/JetBrains/WebStorm/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/WebStorm",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "pycharm",
        config_path: Some(".config/JetBrains/PyCharm/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/PyCharm",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "goland",
        config_path: Some(".config/JetBrains/GoLand/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/GoLand",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "rider",
        config_path: Some(".config/JetBrains/Rider/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/Rider",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "clion",
        config_path: Some(".config/JetBrains/CLion/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/CLion",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    AgentConfig {
        name: "rustrover",
        config_path: Some(".config/JetBrains/RustRover/tools/git-ai.xml"),
        detect_dir: ".config/JetBrains/RustRover",
        hook_format: Some(HookFormat::JetBrainsXml),
    },
    // Git clients — use standard git hook scripts
    AgentConfig {
        name: "fork",
        config_path: None, // Repo-level install via git hooks directory
        #[cfg(target_os = "macos")]
        detect_dir: "Library/Application Support/Fork",
        #[cfg(not(target_os = "macos"))]
        detect_dir: ".fork",
        hook_format: Some(HookFormat::GitHookScript),
    },
    AgentConfig {
        name: "sublime-merge",
        config_path: None, // Repo-level install via git hooks directory
        #[cfg(target_os = "macos")]
        detect_dir: "Library/Application Support/Sublime Merge",
        #[cfg(not(target_os = "macos"))]
        detect_dir: ".config/sublime-merge",
        hook_format: Some(HookFormat::GitHookScript),
    },
];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An agent that was detected as installed on the system.
#[derive(Debug, Clone)]
pub struct InstalledAgent {
    pub name: String,
    pub config_path: Option<PathBuf>,
}

/// Status of a known agent.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    /// Whether the tool's directory was detected on the system.
    pub detected: bool,
    /// Whether hooks are currently installed. `None` if detection dir missing or no config path.
    pub hooks_installed: Option<bool>,
    /// Whether auto-install is supported for this agent.
    pub installable: bool,
}

/// Result of a dry-run install/uninstall operation showing what WOULD change.
#[derive(Debug, Clone)]
pub struct DryRunResult {
    /// Path to the config file that would be modified.
    pub path: PathBuf,
    /// Current file content, or `None` if the file does not yet exist.
    pub current_content: Option<String>,
    /// The proposed new content after install/uninstall.
    pub proposed_content: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns which AI coding tools are detected on the system.
pub fn detect_installed() -> Vec<InstalledAgent> {
    let home = match home_dir() {
        Some(h) => h,
        None => return vec![],
    };

    AGENT_INSTALL_CONFIGS
        .iter()
        .filter(|cfg| home.join(cfg.detect_dir).is_dir())
        .map(|cfg| InstalledAgent {
            name: cfg.name.to_string(),
            config_path: cfg.config_path.map(|p| home.join(p)),
        })
        .collect()
}

/// Installs hooks for a specific tool by name.
pub fn install_hooks(tool: &str) -> Result<(), String> {
    // Check if this is a GitHookScript agent first (before resolve_agent_config which needs config_path)
    let cfg = AGENT_INSTALL_CONFIGS
        .iter()
        .find(|c| c.name == tool)
        .ok_or_else(|| format!("unknown agent: {tool}"))?;

    if cfg.hook_format == Some(HookFormat::GitHookScript) {
        return Err(format!(
            "{} uses standard git hooks — use `git-ai install --repo` in a git repository instead",
            tool
        ));
    }

    let (full_path, hook_format) = resolve_agent_config(tool)?;
    match hook_format {
        HookFormat::JetBrainsXml => install_jetbrains_xml(&full_path, tool),
        HookFormat::GitHookScript => unreachable!(), // handled above
        _ => {
            let command = format!(
                "$HOME/.git-ai/bin/git-ai checkpoint {} --hook-input stdin",
                tool
            );
            install_hook_to_file(&full_path, &command, hook_format)
        }
    }
}

/// Installs hooks for all detected tools that support auto-install.
pub fn install_all() -> Vec<(String, Result<(), String>)> {
    let installed = detect_installed();
    installed
        .into_iter()
        .map(|agent| {
            let result = install_hooks(&agent.name);
            (agent.name, result)
        })
        .collect()
}

/// Returns install status for all known agents.
pub fn status() -> Vec<AgentStatus> {
    let home = match home_dir() {
        Some(h) => h,
        None => {
            return AGENT_INSTALL_CONFIGS
                .iter()
                .map(|cfg| AgentStatus {
                    name: cfg.name.to_string(),
                    detected: false,
                    hooks_installed: None,
                    installable: cfg.config_path.is_some(),
                })
                .collect();
        }
    };

    AGENT_INSTALL_CONFIGS
        .iter()
        .map(|cfg| {
            let detected = home.join(cfg.detect_dir).is_dir();
            let hooks_installed = if detected {
                cfg.config_path.and_then(|p| {
                    let full_path = home.join(p);
                    check_hooks_installed(&full_path, cfg.name)
                })
            } else {
                None
            };

            AgentStatus {
                name: cfg.name.to_string(),
                detected,
                hooks_installed,
                installable: cfg.config_path.is_some(),
            }
        })
        .collect()
}

/// Uninstalls hooks for a specific tool by name.
/// Parses the config JSON, removes all git-ai hook entries, and writes back.
/// If removing leaves an empty hooks object, removes the hooks key entirely.
pub fn uninstall_hooks(tool: &str) -> Result<(), String> {
    let (full_path, hook_format) = resolve_agent_config(tool)?;
    match hook_format {
        HookFormat::JetBrainsXml => {
            if full_path.exists() {
                fs::remove_file(&full_path)
                    .map_err(|e| format!("failed to remove {}: {}", full_path.display(), e))?;
            }
            Ok(())
        }
        HookFormat::GitHookScript => {
            Err(format!(
                "{} uses standard git hooks — use `git-ai install --repo` in a git repository to manage hooks",
                tool
            ))
        }
        _ => {
            let command_needle = format!("git-ai checkpoint {}", tool);
            uninstall_hook_from_file(&full_path, &command_needle, hook_format)
        }
    }
}

/// Uninstalls hooks for all detected tools that support auto-install.
pub fn uninstall_all() -> Vec<(String, Result<(), String>)> {
    let installed = detect_installed();
    installed
        .into_iter()
        .map(|agent| {
            let result = uninstall_hooks(&agent.name);
            (agent.name, result)
        })
        .collect()
}

/// Dry-run for a single tool: returns what WOULD be changed without writing anything.
pub fn install_hooks_dry_run(tool: &str) -> Result<DryRunResult, String> {
    let (full_path, hook_format) = resolve_agent_config(tool)?;
    match hook_format {
        HookFormat::JetBrainsXml => {
            let current_content = fs::read_to_string(&full_path).ok();
            let proposed_content = generate_jetbrains_xml(tool);
            Ok(DryRunResult {
                path: full_path,
                current_content,
                proposed_content,
            })
        }
        HookFormat::GitHookScript => {
            Err(format!(
                "{} uses standard git hooks — use `git-ai install --repo` in a git repository",
                tool
            ))
        }
        _ => {
            let command = format!(
                "$HOME/.git-ai/bin/git-ai checkpoint {} --hook-input stdin",
                tool
            );
            compute_install_dry_run(&full_path, &command, hook_format)
        }
    }
}

/// Dry-run for all detected tools that support auto-install.
pub fn install_all_dry_run() -> Vec<(String, Result<DryRunResult, String>)> {
    let installed = detect_installed();
    installed
        .into_iter()
        .map(|agent| {
            let result = install_hooks_dry_run(&agent.name);
            (agent.name, result)
        })
        .collect()
}

/// Installs the git-ai VS Code extension via `code --install-extension`.
/// Returns Ok(()) on success or an error message if the command fails or `code` is not found.
pub fn install_vscode_extension() -> Result<(), String> {
    let output = Command::new("code")
        .args(["--install-extension", "git-ai.git-ai"])
        .output()
        .map_err(|e| format!("failed to run `code --install-extension`: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "`code --install-extension git-ai.git-ai` failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ))
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

/// Resolve the config for a given tool, returning (full_path, hook_format).
fn resolve_agent_config(tool: &str) -> Result<(PathBuf, HookFormat), String> {
    let home = home_dir().ok_or_else(|| "could not determine HOME directory".to_string())?;
    let cfg = AGENT_INSTALL_CONFIGS
        .iter()
        .find(|c| c.name == tool)
        .ok_or_else(|| format!("unknown agent: {tool}"))?;

    let config_path = cfg
        .config_path
        .ok_or_else(|| format!("auto-install not supported for {tool}"))?;
    let hook_format = cfg
        .hook_format
        .ok_or_else(|| format!("hook format unknown for {tool}"))?;

    Ok((home.join(config_path), hook_format))
}

/// Check if git-ai hooks are already present in the config file.
fn check_hooks_installed(path: &Path, agent_name: &str) -> Option<bool> {
    let content = fs::read_to_string(path).ok()?;
    // For JetBrains XML, check for the git-ai tool name
    if path.extension().and_then(|e| e.to_str()) == Some("xml") {
        return Some(content.contains("git-ai checkpoint"));
    }
    let needle = format!("git-ai checkpoint {}", agent_name);
    Some(content.contains(&needle))
}

/// Core uninstall logic: read/parse/remove git-ai entries/write.
fn uninstall_hook_from_file(
    path: &Path,
    command_needle: &str,
    format: HookFormat,
) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }

    let (mut root, content) = read_config_json(path)?;
    if content.as_ref().is_some_and(|c| c.trim().is_empty()) {
        return Ok(());
    }

    // If the hook needle isn't even present, nothing to do.
    let serialized = serde_json::to_string(&root).unwrap_or_default();
    if !serialized.contains(command_needle) {
        return Ok(());
    }

    remove_hooks_from_value(&mut root, command_needle, format)?;

    let output = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("failed to serialize JSON: {}", e))?;
    fs::write(path, output.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

    Ok(())
}

/// Remove git-ai hook entries from a parsed JSON value.
fn remove_hooks_from_value(
    root: &mut Value,
    command_needle: &str,
    format: HookFormat,
) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;

    let hooks_obj = match root_obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        Some(obj) => obj,
        None => return Ok(()),
    };

    let keys: &[&str] = match format {
        HookFormat::CursorStyle => &["preToolUse", "postToolUse"],
        HookFormat::PascalCursorStyle | HookFormat::ClaudeStyle => &["PreToolUse", "PostToolUse"],
        HookFormat::JetBrainsXml | HookFormat::GitHookScript => &[],
    };

    for key in keys {
        if let Some(arr) = hooks_obj.get_mut(*key).and_then(|v| v.as_array_mut()) {
            arr.retain(|entry| {
                let s = serde_json::to_string(entry).unwrap_or_default();
                !s.contains(command_needle)
            });
        }
    }

    // If all hook arrays are now empty, remove the hooks key entirely.
    let all_empty = hooks_obj.values().all(|v| {
        v.as_array().is_some_and(|a| a.is_empty())
    });
    if all_empty {
        root_obj.remove("hooks");
    }

    Ok(())
}

/// Read a config file into a JSON Value, or return an empty object if missing/empty.
fn read_config_json(path: &Path) -> Result<(Value, Option<String>), String> {
    if !path.exists() {
        return Ok((Value::Object(serde_json::Map::new()), None));
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if content.trim().is_empty() {
        return Ok((Value::Object(serde_json::Map::new()), Some(content)));
    }
    let root = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;
    Ok((root, Some(content)))
}

/// Merge hook command into a JSON root value (mutates in place). Skips if already present.
fn merge_hooks_into(root: &mut Value, command: &str, format: HookFormat) -> Result<(), String> {
    let serialized = serde_json::to_string(root).unwrap_or_default();
    if serialized.contains(command) {
        return Ok(());
    }
    match format {
        HookFormat::CursorStyle => merge_cursor_style(root, command, "preToolUse", "postToolUse"),
        HookFormat::PascalCursorStyle => merge_cursor_style(root, command, "PreToolUse", "PostToolUse"),
        HookFormat::ClaudeStyle => merge_claude_style(root, command),
        HookFormat::JetBrainsXml | HookFormat::GitHookScript => {
            // These formats don't use JSON merging — handled by dedicated install functions
            Ok(())
        }
    }
}

/// Compute what install would produce without writing to disk.
fn compute_install_dry_run(
    path: &Path,
    command: &str,
    format: HookFormat,
) -> Result<DryRunResult, String> {
    let (mut root, current_content) = read_config_json(path)?;
    merge_hooks_into(&mut root, command, format)?;

    let proposed_content = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("failed to serialize JSON: {}", e))?;

    Ok(DryRunResult {
        path: path.to_path_buf(),
        current_content,
        proposed_content,
    })
}

/// Core install logic: read/parse/merge/write.
fn install_hook_to_file(path: &Path, command: &str, format: HookFormat) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directories for {}: {}", path.display(), e))?;
    }

    let (mut root, _) = read_config_json(path)?;

    // Check idempotency: if hook command already present, skip.
    let serialized = serde_json::to_string(&root).unwrap_or_default();
    if serialized.contains(command) {
        return Ok(());
    }

    merge_hooks_into(&mut root, command, format)?;

    let output = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("failed to serialize JSON: {}", e))?;
    fs::write(path, output.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

    Ok(())
}

/// Merge Cursor-style hooks: `{ "hooks": { "<pre_key>": [...], "<post_key>": [...] } }`
fn merge_cursor_style(
    root: &mut Value,
    command: &str,
    pre_key: &str,
    post_key: &str,
) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;

    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "\"hooks\" field is not a JSON object".to_string())?;

    let entry = serde_json::json!({"command": command});

    for key in [pre_key, post_key] {
        let arr = hooks_obj
            .entry(key)
            .or_insert_with(|| Value::Array(vec![]));
        let arr_vec = arr
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{}\" is not an array", key))?;
        arr_vec.push(entry.clone());
    }

    Ok(())
}

/// Merge Claude Code style hooks.
fn merge_claude_style(root: &mut Value, command: &str) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;

    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "\"hooks\" field is not a JSON object".to_string())?;

    let entry = serde_json::json!({
        "matcher": "*",
        "hooks": [{"type": "command", "command": command}]
    });

    for key in ["PreToolUse", "PostToolUse"] {
        let arr = hooks_obj
            .entry(key)
            .or_insert_with(|| Value::Array(vec![]));
        let arr_vec = arr
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{}\" is not an array", key))?;
        arr_vec.push(entry.clone());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JetBrains XML helpers
// ---------------------------------------------------------------------------

/// Generate the JetBrains External Tools XML content for a given IDE/agent name.
pub fn generate_jetbrains_xml(agent_name: &str) -> String {
    format!(
        r#"<toolSet name="git-ai">
  <tool name="git-ai checkpoint" description="AI attribution checkpoint" showInMainMenu="false" showInEditor="false" showInProject="false" showInSearchPopup="false" disabled="false" useConsole="false" showConsoleOnStdOut="false" showConsoleOnStdErr="false" synchronizeAfterRun="true">
    <exec>
      <option name="COMMAND" value="$USER_HOME$/.git-ai/bin/git-ai" />
      <option name="PARAMETERS" value="checkpoint {agent_name} --hook-input stdin" />
      <option name="WORKING_DIRECTORY" value="$ProjectFileDir$" />
    </exec>
  </tool>
</toolSet>
"#
    )
}

/// Install JetBrains External Tools XML file.
fn install_jetbrains_xml(path: &Path, agent_name: &str) -> Result<(), String> {
    // Check if already installed (idempotent)
    if path.exists() {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        if content.contains("git-ai checkpoint") {
            return Ok(());
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directories for {}: {}", path.display(), e))?;
    }

    let xml_content = generate_jetbrains_xml(agent_name);
    fs::write(path, xml_content.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

    Ok(())
}

/// Detect which JetBrains IDEs are installed by scanning ~/.config/JetBrains/.
/// Returns a list of (ide_name, config_dir) pairs.
pub fn detect_jetbrains_ides() -> Vec<(String, PathBuf)> {
    let home = match home_dir() {
        Some(h) => h,
        None => return vec![],
    };

    let jetbrains_dir = home.join(".config/JetBrains");
    if !jetbrains_dir.is_dir() {
        return vec![];
    }

    let known_ides = [
        ("IntelliJIdea", "intellij"),
        ("WebStorm", "webstorm"),
        ("PyCharm", "pycharm"),
        ("GoLand", "goland"),
        ("Rider", "rider"),
        ("CLion", "clion"),
        ("RustRover", "rustrover"),
    ];

    let mut results = vec![];
    if let Ok(entries) = fs::read_dir(&jetbrains_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            for (prefix, ide_name) in &known_ides {
                if name.starts_with(prefix) && entry.path().is_dir() {
                    results.push((ide_name.to_string(), entry.path()));
                    break;
                }
            }
        }
    }

    results
}

/// Install JetBrains hooks for a specific IDE by name.
pub fn install_jetbrains(ide_name: &str) -> Result<(), String> {
    install_hooks(ide_name)
}

// ---------------------------------------------------------------------------
// Git hook script helpers (Fork, Sublime Merge)
// ---------------------------------------------------------------------------

/// Generate the content of a git hook script that invokes git-ai.
pub fn generate_git_hook_script(hook_name: &str) -> String {
    match hook_name {
        "post-commit" => "#!/bin/sh\n\
             # git-ai: automatic attribution tracking\n\
             git-ai post-commit\n"
            .to_string(),
        "pre-commit" => "#!/bin/sh\n\
             # git-ai: capture pre-commit state for attribution\n\
             git-ai checkpoint human 2>/dev/null || true\n"
            .to_string(),
        _ => format!(
            "#!/bin/sh\n\
             # git-ai hook: {}\n\
             git-ai {} \"$@\" 2>/dev/null || true\n",
            hook_name, hook_name
        ),
    }
}

/// Install git hook scripts to a repository's hooks directory.
/// This is used by Fork, Sublime Merge, and other git clients that respect
/// the standard git hooks mechanism.
/// If a hook script already exists, the git-ai invocation is appended (unless already present).
pub fn install_git_hooks(repo_path: &Path) -> Result<(), String> {
    let git_dir = repo_path.join(".git");
    if !git_dir.is_dir() {
        return Err(format!(
            "not a git repository: {} (no .git directory)",
            repo_path.display()
        ));
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("failed to create hooks dir: {}", e))?;

    let hooks_to_install = ["post-commit", "pre-commit"];

    for hook_name in &hooks_to_install {
        let hook_path = hooks_dir.join(hook_name);
        install_single_git_hook(&hook_path, hook_name)?;
    }

    Ok(())
}

/// Install a single git hook script, appending if one already exists.
fn install_single_git_hook(hook_path: &Path, hook_name: &str) -> Result<(), String> {
    let git_ai_marker = "# git-ai:";
    let hook_content = generate_git_hook_script(hook_name);

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path)
            .map_err(|e| format!("failed to read {}: {}", hook_path.display(), e))?;

        // Already installed
        if existing.contains(git_ai_marker) {
            return Ok(());
        }

        // Append our hook invocation to the existing script
        let appended = format!(
            "{}\n{}",
            existing.trim_end(),
            hook_content.trim_start_matches("#!/bin/sh\n")
        );
        fs::write(hook_path, appended.as_bytes())
            .map_err(|e| format!("failed to write {}: {}", hook_path.display(), e))?;
    } else {
        fs::write(hook_path, hook_content.as_bytes())
            .map_err(|e| format!("failed to write {}: {}", hook_path.display(), e))?;
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(hook_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to chmod {}: {}", hook_path.display(), e))?;
    }

    Ok(())
}

/// Uninstall git hook scripts from a repository's hooks directory.
/// Removes git-ai lines from existing hook scripts rather than deleting the files entirely.
pub fn uninstall_git_hooks(repo_path: &Path) -> Result<(), String> {
    let git_dir = repo_path.join(".git");
    if !git_dir.is_dir() {
        return Err(format!(
            "not a git repository: {} (no .git directory)",
            repo_path.display()
        ));
    }

    let hooks_dir = git_dir.join("hooks");
    let hooks_to_uninstall = ["post-commit", "pre-commit"];

    for hook_name in &hooks_to_uninstall {
        let hook_path = hooks_dir.join(hook_name);
        if !hook_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&hook_path)
            .map_err(|e| format!("failed to read {}: {}", hook_path.display(), e))?;

        // Filter out git-ai lines
        let filtered: Vec<&str> = content
            .lines()
            .filter(|line| !line.contains("git-ai"))
            .collect();

        // If only the shebang (or nothing) remains, remove the file
        let meaningful: Vec<&&str> = filtered
            .iter()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("#!"))
            .collect();

        if meaningful.is_empty() {
            let _ = fs::remove_file(&hook_path);
        } else {
            let new_content = filtered.join("\n") + "\n";
            fs::write(&hook_path, new_content.as_bytes())
                .map_err(|e| format!("failed to write {}: {}", hook_path.display(), e))?;
        }
    }

    Ok(())
}

/// Returns the list of all known agent names (for doctor checks).
pub fn all_agent_names() -> Vec<&'static str> {
    AGENT_INSTALL_CONFIGS.iter().map(|c| c.name).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    fn make_command(agent: &str) -> String {
        format!("$HOME/.git-ai/bin/git-ai checkpoint {} --hook-input stdin", agent)
    }

    // --- Cursor-style tests ---

    #[test]
    fn test_cursor_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let pre = hooks["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["command"].as_str().unwrap(), cmd);

        let post = hooks["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_cursor_style_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        // Pre-existing config with other settings.
        let existing = serde_json::json!({
            "someOtherSetting": true,
            "hooks": {
                "preToolUse": [{"command": "echo existing"}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Other settings preserved.
        assert_eq!(content["someOtherSetting"], Value::Bool(true));

        // Existing hook preserved, new hook added.
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["command"].as_str().unwrap(), "echo existing");
        assert_eq!(pre[1]["command"].as_str().unwrap(), cmd);

        // Post hook created fresh.
        let post = content["hooks"]["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_cursor_style_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "hook should not be duplicated");
    }

    // --- PascalCursor-style tests ---

    #[test]
    fn test_pascal_cursor_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("amp");
        install_hook_to_file(&path, &cmd, HookFormat::PascalCursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("PostToolUse"));
        assert_eq!(hooks["PreToolUse"].as_array().unwrap().len(), 1);
    }

    // --- Claude-style tests ---

    #[test]
    fn test_claude_style_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        let pre = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"].as_str().unwrap(), "*");
        let inner = pre[0]["hooks"].as_array().unwrap();
        assert_eq!(inner[0]["type"].as_str().unwrap(), "command");
        assert_eq!(inner[0]["command"].as_str().unwrap(), cmd);
    }

    #[test]
    fn test_claude_style_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let existing = serde_json::json!({
            "permissions": {"allow": ["bash"]},
            "hooks": {
                "PreToolUse": [{"matcher": "write", "hooks": [{"type": "command", "command": "echo lint"}]}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Permissions preserved.
        assert!(content["permissions"]["allow"].as_array().unwrap().len() == 1);

        // Existing hook preserved, new hook appended.
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"].as_str().unwrap(), "write");
        assert_eq!(pre[1]["matcher"].as_str().unwrap(), "*");
    }

    #[test]
    fn test_claude_style_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "hook should not be duplicated");
    }

    // --- detect/status tests ---

    #[test]
    #[serial]
    fn test_detect_with_fake_home() {
        let tmp = TempDir::new().unwrap();
        // Create .cursor dir to simulate cursor installed.
        fs::create_dir(tmp.path().join(".cursor")).unwrap();

        // Temporarily override HOME.
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let detected = detect_installed();

        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"cursor"));
        assert!(!names.contains(&"claude"));
    }

    #[test]
    #[serial]
    fn test_status_with_fake_home() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();

        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let statuses = status();

        let cursor_status = statuses.iter().find(|s| s.name == "cursor").unwrap();
        assert!(cursor_status.detected);
        assert!(cursor_status.installable);
        // No config file exists yet, so hooks_installed is None (file doesn't exist).
        assert_eq!(cursor_status.hooks_installed, None);

        let gemini_status = statuses.iter().find(|s| s.name == "gemini").unwrap();
        assert!(!gemini_status.detected);
        assert!(!gemini_status.installable);
    }

    #[test]
    #[serial]
    fn test_install_hooks_creates_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks("cursor");
        assert!(result.is_ok(), "install_hooks failed: {:?}", result);

        let hook_path = tmp.path().join(".cursor/hooks/hooks.json");
        assert!(hook_path.exists());

        let content: Value =
            serde_json::from_str(&fs::read_to_string(&hook_path).unwrap()).unwrap();
        assert!(content["hooks"]["preToolUse"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn test_install_unknown_agent() {
        let result = install_hooks("nonexistent-tool");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown agent"));
    }

    #[test]
    #[serial]
    fn test_install_unsupported_agent() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".gemini")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks("gemini");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not supported"));
    }

    #[test]
    #[serial]
    fn test_install_all() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        // SAFETY: test is single-threaded and we accept the risk of env mutation.
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let results = install_all();
        let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"cursor"));
        assert!(names.contains(&"claude"));

        for (name, result) in &results {
            assert!(result.is_ok(), "{} install failed: {:?}", name, result);
        }
    }

    // --- Edge case: updating outdated hooks ---

    #[test]
    fn test_cursor_style_updates_outdated_command() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        // Old-style git-ai command from a previous version
        let existing = serde_json::json!({
            "hooks": {
                "preToolUse": [{"command": "/old/path/git-ai checkpoint cursor 2>/dev/null || true"}],
                "postToolUse": [{"command": "/old/path/git-ai checkpoint cursor 2>/dev/null || true"}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        // Both old and new hook present (we append, don't replace)
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[1]["command"].as_str().unwrap(), cmd);
    }

    // --- Edge case: multiple third-party hooks preserved ---

    #[test]
    fn test_cursor_preserves_multiple_third_party_hooks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let existing = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {"command": "eslint --fix"},
                    {"command": "prettier --write"},
                    {"command": "custom-lint-hook"}
                ],
                "postToolUse": [
                    {"command": "notify-team-bot"}
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre = content["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 4); // 3 existing + 1 new
        assert_eq!(pre[0]["command"].as_str().unwrap(), "eslint --fix");
        assert_eq!(pre[1]["command"].as_str().unwrap(), "prettier --write");
        assert_eq!(pre[2]["command"].as_str().unwrap(), "custom-lint-hook");
        assert_eq!(pre[3]["command"].as_str().unwrap(), cmd);

        let post = content["hooks"]["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2); // 1 existing + 1 new
        assert_eq!(post[0]["command"].as_str().unwrap(), "notify-team-bot");
    }

    // --- Edge case: empty/whitespace file ---

    #[test]
    fn test_cursor_style_empty_whitespace_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, "   \n\n  ").unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["hooks"]["preToolUse"].as_array().unwrap().len(), 1);
    }

    // --- Edge case: malformed JSON ---

    #[test]
    fn test_cursor_style_malformed_json_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, "{ invalid json !!!").unwrap();

        let cmd = make_command("cursor");
        let result = install_hook_to_file(&path, &cmd, HookFormat::CursorStyle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to parse"));
    }

    // --- Edge case: config with extra fields (version, etc.) ---

    #[test]
    fn test_cursor_style_preserves_version_field() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let existing = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": []
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["version"], 1);
        assert_eq!(content["hooks"]["preToolUse"].as_array().unwrap().len(), 1);
    }

    // --- Edge case: status after install ---

    #[test]
    #[serial]
    fn test_status_shows_hooks_installed_after_install() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        // Before install: hooks not installed (no file)
        let statuses = status();
        let cursor = statuses.iter().find(|s| s.name == "cursor").unwrap();
        assert_eq!(cursor.hooks_installed, None);

        // Install
        install_hooks("cursor").unwrap();

        // After install: hooks installed
        let statuses = status();
        let cursor = statuses.iter().find(|s| s.name == "cursor").unwrap();
        assert_eq!(cursor.hooks_installed, Some(true));
    }

    // --- Claude-style: complex existing config ---

    #[test]
    fn test_claude_style_preserves_complex_existing_config() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let existing = serde_json::json!({
            "env": {"API_KEY": "sk-xxx"},
            "model": "opus",
            "permissions": {"allow": ["bash", "read", "write"]},
            "hooks": {
                "PreToolUse": [
                    {"matcher": "write", "hooks": [{"type": "command", "command": "echo checking"}]},
                    {"matcher": "bash", "hooks": [{"type": "command", "command": "echo auditing"}]}
                ],
                "PostToolUse": []
            },
            "enabledPlugins": {"rust-analyzer": true}
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let cmd = make_command("claude");
        install_hook_to_file(&path, &cmd, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // All non-hook settings preserved
        assert_eq!(content["env"]["API_KEY"], "sk-xxx");
        assert_eq!(content["model"], "opus");
        assert_eq!(content["permissions"]["allow"].as_array().unwrap().len(), 3);
        assert_eq!(content["enabledPlugins"]["rust-analyzer"], true);

        // Existing hooks preserved, new one appended
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 3);
        assert_eq!(pre[0]["matcher"], "write");
        assert_eq!(pre[1]["matcher"], "bash");
        assert_eq!(pre[2]["matcher"], "*");

        // PostToolUse now has one entry
        let post = content["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["matcher"], "*");
    }

    // --- Edge case: non-object root ---

    #[test]
    fn test_cursor_style_non_object_root_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, "[1, 2, 3]").unwrap();

        let cmd = make_command("cursor");
        let result = install_hook_to_file(&path, &cmd, HookFormat::CursorStyle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a JSON object"));
    }

    // --- Edge case: hooks field is not an object ---

    #[test]
    fn test_cursor_style_hooks_not_object_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, r#"{"hooks": "not an object"}"#).unwrap();

        let cmd = make_command("cursor");
        let result = install_hook_to_file(&path, &cmd, HookFormat::CursorStyle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a JSON object"));
    }

    // --- Edge case: hooks array entry is not an array ---

    #[test]
    fn test_cursor_style_hook_entry_not_array_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, r#"{"hooks": {"preToolUse": "wrong"}}"#).unwrap();

        let cmd = make_command("cursor");
        let result = install_hook_to_file(&path, &cmd, HookFormat::CursorStyle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not an array"));
    }

    // --- Edge case: all supported agents produce valid JSON ---

    #[test]
    #[serial]
    fn test_all_supported_agents_produce_valid_config() {
        let tmp = TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        for cfg in AGENT_INSTALL_CONFIGS.iter().filter(|c| c.config_path.is_some()) {
            // Skip GitHookScript agents (they need --repo, not global install)
            if cfg.hook_format == Some(HookFormat::GitHookScript) {
                continue;
            }

            fs::create_dir_all(tmp.path().join(cfg.detect_dir)).unwrap();
            let result = install_hooks(cfg.name);
            assert!(result.is_ok(), "install_hooks({}) failed: {:?}", cfg.name, result);

            let full_path = tmp.path().join(cfg.config_path.unwrap());
            let content = fs::read_to_string(&full_path).unwrap();

            if cfg.hook_format == Some(HookFormat::JetBrainsXml) {
                // JetBrains uses XML, not JSON
                assert!(content.contains("<toolSet"), "{} should contain XML toolSet", cfg.name);
                assert!(content.contains("git-ai checkpoint"), "{} should contain checkpoint command", cfg.name);
            } else {
                let parsed: Value = serde_json::from_str(&content).unwrap();
                assert!(parsed["hooks"].is_object(), "{} hooks not an object", cfg.name);
            }
        }
    }

    // --- Edge case: file permissions (read-only) ---

    #[cfg(unix)]
    #[test]
    fn test_cursor_style_read_only_file_returns_error() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

        let cmd = make_command("cursor");
        let result = install_hook_to_file(&path, &cmd, HookFormat::CursorStyle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to write"));

        // Restore permissions for cleanup
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // ===================================================================
    // Uninstall tests
    // ===================================================================

    #[test]
    fn test_uninstall_cursor_style_removes_hook() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();

        // Verify hook is present.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("git-ai checkpoint cursor"));

        // Uninstall.
        let needle = "git-ai checkpoint cursor";
        uninstall_hook_from_file(&path, needle, HookFormat::CursorStyle).unwrap();

        // Verify hook removed and hooks key removed (was empty).
        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(content.get("hooks").is_none(), "empty hooks object should be removed");
    }

    #[test]
    fn test_uninstall_preserves_other_hooks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let existing = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {"command": "eslint --fix"},
                    {"command": "$HOME/.git-ai/bin/git-ai checkpoint cursor --hook-input stdin"}
                ],
                "postToolUse": [
                    {"command": "notify-team"},
                    {"command": "$HOME/.git-ai/bin/git-ai checkpoint cursor --hook-input stdin"}
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let needle = "git-ai checkpoint cursor";
        uninstall_hook_from_file(&path, needle, HookFormat::CursorStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = content["hooks"].as_object().unwrap();

        // Other hooks preserved.
        let pre = hooks["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["command"].as_str().unwrap(), "eslint --fix");

        let post = hooks["postToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["command"].as_str().unwrap(), "notify-team");
    }

    #[test]
    fn test_uninstall_claude_style_removes_hook() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let existing = serde_json::json!({
            "permissions": {"allow": ["bash"]},
            "hooks": {
                "PreToolUse": [
                    {"matcher": "write", "hooks": [{"type": "command", "command": "echo lint"}]},
                    {"matcher": "*", "hooks": [{"type": "command", "command": "$HOME/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"}]}
                ],
                "PostToolUse": [
                    {"matcher": "*", "hooks": [{"type": "command", "command": "$HOME/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"}]}
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let needle = "git-ai checkpoint claude";
        uninstall_hook_from_file(&path, needle, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Permissions preserved.
        assert_eq!(content["permissions"]["allow"].as_array().unwrap().len(), 1);

        // Only the lint hook remains in PreToolUse.
        let pre = content["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"].as_str().unwrap(), "write");

        // PostToolUse is empty so hooks still exists (pre has entries).
        let post = content["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 0);
    }

    #[test]
    fn test_uninstall_removes_hooks_key_when_all_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        let existing = serde_json::json!({
            "someField": true,
            "hooks": {
                "PreToolUse": [
                    {"matcher": "*", "hooks": [{"type": "command", "command": "$HOME/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"}]}
                ],
                "PostToolUse": [
                    {"matcher": "*", "hooks": [{"type": "command", "command": "$HOME/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"}]}
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let needle = "git-ai checkpoint claude";
        uninstall_hook_from_file(&path, needle, HookFormat::ClaudeStyle).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // hooks key removed entirely.
        assert!(content.get("hooks").is_none());
        // Other fields preserved.
        assert_eq!(content["someField"], Value::Bool(true));
    }

    #[test]
    fn test_uninstall_nonexistent_file_is_ok() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does_not_exist.json");

        let result = uninstall_hook_from_file(&path, "git-ai checkpoint cursor", HookFormat::CursorStyle);
        assert!(result.is_ok());
    }

    #[test]
    fn test_uninstall_when_no_hook_present_is_noop() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let existing = serde_json::json!({
            "hooks": {
                "preToolUse": [{"command": "eslint --fix"}],
                "postToolUse": [{"command": "notify-team"}]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let needle = "git-ai checkpoint cursor";
        uninstall_hook_from_file(&path, needle, HookFormat::CursorStyle).unwrap();

        // File unchanged (needle not found, so we short-circuit).
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    #[serial]
    fn test_uninstall_hooks_public_api() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        install_hooks("cursor").unwrap();

        // Verify installed.
        let hook_path = tmp.path().join(".cursor/hooks/hooks.json");
        let content = fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains("git-ai checkpoint cursor"));

        // Uninstall.
        uninstall_hooks("cursor").unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&hook_path).unwrap()).unwrap();
        assert!(content.get("hooks").is_none());
    }

    #[test]
    #[serial]
    fn test_uninstall_all_removes_all_hooks() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        install_all();
        let results = uninstall_all();

        for (name, result) in &results {
            assert!(result.is_ok(), "{} uninstall failed: {:?}", name, result);
        }

        // Verify hooks removed.
        let hook_path = tmp.path().join(".cursor/hooks/hooks.json");
        let content: Value = serde_json::from_str(&fs::read_to_string(&hook_path).unwrap()).unwrap();
        assert!(content.get("hooks").is_none());
    }

    // ===================================================================
    // Dry-run tests
    // ===================================================================

    #[test]
    fn test_dry_run_does_not_write_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        // File does not exist initially.
        assert!(!path.exists());

        let command = make_command("cursor");
        let result = compute_install_dry_run(&path, &command, HookFormat::CursorStyle).unwrap();

        // File still does not exist.
        assert!(!path.exists());

        // Result contains the proposed content.
        assert!(result.current_content.is_none());
        assert!(result.proposed_content.contains("git-ai checkpoint cursor"));
        assert_eq!(result.path, path);
    }

    #[test]
    fn test_dry_run_existing_file_shows_diff() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let existing = serde_json::json!({"someField": true});
        fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        let command = make_command("cursor");
        let result = compute_install_dry_run(&path, &command, HookFormat::CursorStyle).unwrap();

        // Current content preserved.
        assert!(result.current_content.is_some());
        let current = result.current_content.unwrap();
        assert!(current.contains("someField"));
        assert!(!current.contains("git-ai"));

        // Proposed content has the hook.
        assert!(result.proposed_content.contains("git-ai checkpoint cursor"));
        assert!(result.proposed_content.contains("someField"));
    }

    #[test]
    fn test_dry_run_idempotent_returns_same_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");

        let cmd = make_command("cursor");
        // Install for real first.
        install_hook_to_file(&path, &cmd, HookFormat::CursorStyle).unwrap();
        let installed_content = fs::read_to_string(&path).unwrap();

        // Dry-run should show no change.
        let result = compute_install_dry_run(&path, &cmd, HookFormat::CursorStyle).unwrap();
        assert_eq!(result.current_content.as_deref(), Some(installed_content.as_str()));
        assert_eq!(result.proposed_content, installed_content);
    }

    #[test]
    #[serial]
    fn test_install_hooks_dry_run_public_api() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks_dry_run("cursor").unwrap();

        // The file should NOT have been created.
        let hook_path = tmp.path().join(".cursor/hooks/hooks.json");
        assert!(!hook_path.exists());

        // But the result should show what would be written.
        assert!(result.proposed_content.contains("git-ai checkpoint cursor"));
        assert_eq!(result.path, hook_path);
    }

    #[test]
    #[serial]
    fn test_install_all_dry_run() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let results = install_all_dry_run();
        let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"cursor"));
        assert!(names.contains(&"claude"));

        // No files written.
        assert!(!tmp.path().join(".cursor/hooks/hooks.json").exists());
        assert!(!tmp.path().join(".claude/settings.json").exists());

        // All results successful with proposed content.
        for (name, result) in &results {
            let dr = result.as_ref().unwrap_or_else(|e| panic!("{name} failed: {e}"));
            assert!(dr.proposed_content.contains("git-ai checkpoint"));
        }
    }

    // ===================================================================
    // VS Code / JetBrains / Git client detection tests
    // ===================================================================

    #[test]
    #[serial]
    fn test_detect_vscode() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".vscode")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let detected = detect_installed();
        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"vscode"));
    }

    #[test]
    #[serial]
    fn test_detect_jetbrains_intellij() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".config/JetBrains/IntelliJIdea")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let detected = detect_installed();
        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"intellij"));
    }

    #[test]
    #[serial]
    fn test_detect_jetbrains_webstorm() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".config/JetBrains/WebStorm")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let detected = detect_installed();
        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"webstorm"));
    }

    #[test]
    #[serial]
    fn test_detect_fork() {
        let tmp = TempDir::new().unwrap();
        // On Linux, Fork uses .fork
        #[cfg(not(target_os = "macos"))]
        fs::create_dir(tmp.path().join(".fork")).unwrap();
        #[cfg(target_os = "macos")]
        fs::create_dir_all(tmp.path().join("Library/Application Support/Fork")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let detected = detect_installed();
        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"fork"));
    }

    #[test]
    #[serial]
    fn test_detect_sublime_merge() {
        let tmp = TempDir::new().unwrap();
        #[cfg(not(target_os = "macos"))]
        fs::create_dir_all(tmp.path().join(".config/sublime-merge")).unwrap();
        #[cfg(target_os = "macos")]
        fs::create_dir_all(tmp.path().join("Library/Application Support/Sublime Merge")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let detected = detect_installed();
        let names: Vec<&str> = detected.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"sublime-merge"));
    }

    #[test]
    #[serial]
    fn test_jetbrains_is_installable() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".config/JetBrains/IntelliJIdea")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let statuses = status();
        let intellij = statuses.iter().find(|s| s.name == "intellij").unwrap();
        assert!(intellij.detected);
        assert!(intellij.installable);
    }

    #[test]
    #[serial]
    fn test_vscode_not_installable_via_hooks() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".vscode")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let statuses = status();
        let vscode = statuses.iter().find(|s| s.name == "vscode").unwrap();
        assert!(vscode.detected);
        assert!(!vscode.installable);
    }

    // --- JetBrains XML tests ---

    #[test]
    fn test_jetbrains_xml_generation() {
        let xml = generate_jetbrains_xml("intellij");
        assert!(xml.contains("<toolSet name=\"git-ai\">"));
        assert!(xml.contains("checkpoint intellij --hook-input stdin"));
        assert!(xml.contains("$USER_HOME$/.git-ai/bin/git-ai"));
        assert!(xml.contains("$ProjectFileDir$"));
    }

    #[test]
    fn test_jetbrains_xml_install() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tools/git-ai.xml");

        install_jetbrains_xml(&path, "webstorm").unwrap();

        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("git-ai checkpoint"));
        assert!(content.contains("checkpoint webstorm"));
    }

    #[test]
    fn test_jetbrains_xml_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tools/git-ai.xml");

        install_jetbrains_xml(&path, "intellij").unwrap();
        let first_content = fs::read_to_string(&path).unwrap();

        install_jetbrains_xml(&path, "intellij").unwrap();
        let second_content = fs::read_to_string(&path).unwrap();

        assert_eq!(first_content, second_content);
    }

    #[test]
    #[serial]
    fn test_jetbrains_install_via_public_api() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".config/JetBrains/IntelliJIdea")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = install_hooks("intellij");
        assert!(result.is_ok(), "install_hooks(intellij) failed: {:?}", result);

        let xml_path = tmp.path().join(".config/JetBrains/IntelliJIdea/tools/git-ai.xml");
        assert!(xml_path.exists());
        let content = fs::read_to_string(&xml_path).unwrap();
        assert!(content.contains("checkpoint intellij"));
    }

    // --- Git hook script tests ---

    #[test]
    fn test_git_hook_script_generation_post_commit() {
        let script = generate_git_hook_script("post-commit");
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("# git-ai:"));
        assert!(script.contains("git-ai post-commit"));
    }

    #[test]
    fn test_git_hook_script_generation_pre_commit() {
        let script = generate_git_hook_script("pre-commit");
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("# git-ai:"));
        assert!(script.contains("git-ai checkpoint human"));
    }

    #[test]
    fn test_install_git_hooks_fresh_repo() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        install_git_hooks(&repo).unwrap();

        let post_commit = repo.join(".git/hooks/post-commit");
        assert!(post_commit.exists());
        let content = fs::read_to_string(&post_commit).unwrap();
        assert!(content.contains("git-ai post-commit"));

        let pre_commit = repo.join(".git/hooks/pre-commit");
        assert!(pre_commit.exists());
        let content = fs::read_to_string(&pre_commit).unwrap();
        assert!(content.contains("git-ai checkpoint human"));
    }

    #[test]
    fn test_install_git_hooks_idempotent() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        install_git_hooks(&repo).unwrap();
        let first = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();

        install_git_hooks(&repo).unwrap();
        let second = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_install_git_hooks_appends_to_existing() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        // Write a pre-existing hook
        let existing_hook = "#!/bin/sh\necho 'existing hook'\n";
        fs::write(repo.join(".git/hooks/post-commit"), existing_hook).unwrap();

        install_git_hooks(&repo).unwrap();

        let content = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
        assert!(content.contains("echo 'existing hook'"));
        assert!(content.contains("git-ai"));
    }

    #[test]
    fn test_uninstall_git_hooks() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        install_git_hooks(&repo).unwrap();
        assert!(repo.join(".git/hooks/post-commit").exists());

        uninstall_git_hooks(&repo).unwrap();
        // Hook file removed since only git-ai content was there
        assert!(!repo.join(".git/hooks/post-commit").exists());
    }

    #[test]
    fn test_uninstall_git_hooks_preserves_other_content() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(repo.join(".git/hooks")).unwrap();

        // Write a hook with both git-ai and other content
        let hook = "#!/bin/sh\necho 'other stuff'\n# git-ai: automatic attribution tracking\ngit-ai post-commit\n";
        fs::write(repo.join(".git/hooks/post-commit"), hook).unwrap();

        uninstall_git_hooks(&repo).unwrap();

        let content = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
        assert!(content.contains("echo 'other stuff'"));
        assert!(!content.contains("git-ai"));
    }

    #[test]
    fn test_install_git_hooks_not_a_repo() {
        let tmp = TempDir::new().unwrap();
        let result = install_git_hooks(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a git repository"));
    }

    #[test]
    #[serial]
    fn test_fork_uses_git_hook_script() {
        let tmp = TempDir::new().unwrap();
        #[cfg(not(target_os = "macos"))]
        fs::create_dir(tmp.path().join(".fork")).unwrap();
        #[cfg(target_os = "macos")]
        fs::create_dir_all(tmp.path().join("Library/Application Support/Fork")).unwrap();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        // install_hooks("fork") should error directing to --repo
        let result = install_hooks("fork");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("git hooks"));
    }
}
