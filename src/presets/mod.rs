//! Unified agent preset system.
//!
//! Parses hook payloads from AI coding agents (Cursor, Claude, Copilot, etc.)
//! into a standardized `ParsedHookEvent` that the checkpoint processor consumes.
//!
//! Design: One parametrized parser with per-agent configuration tables (approach C),
//! rather than N bespoke parsers. The tables are embedded in this binary but
//! structured for future promotion to a config file (approach A).

pub mod tool_classification;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use tool_classification::{Agent, ToolClass, classify_tool};

/// The output of preset parsing — what the checkpoint processor needs.
#[derive(Debug, Clone)]
pub enum ParsedHookEvent {
    PreFileEdit(PreFileEdit),
    PostFileEdit(PostFileEdit),
    PreBashCall(PreBashCall),
    PostBashCall(PostBashCall),
    KnownHumanEdit(KnownHumanEdit),
    UntrackedEdit(UntrackedEdit),
}

#[derive(Debug, Clone)]
pub struct PreFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PostFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub transcript_path: Option<PathBuf>,
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
}

#[derive(Debug, Clone)]
pub struct PostBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
    pub transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct KnownHumanEdit {
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
}

#[derive(Debug, Clone)]
pub struct UntrackedEdit {
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PresetContext {
    pub agent_tool: String,
    pub agent_session_id: String,
    pub agent_model: String,
    pub cwd: PathBuf,
    pub metadata: HashMap<String, String>,
}

/// Agent-specific field configuration.
/// Each entry tells the parser which JSON field names to probe for standard concepts.
struct AgentConfig {
    agent: Agent,
    tool_name: &'static str,
    session_id_fields: &'static [&'static str],
    cwd_fields: &'static [&'static str],
    hook_event_fields: &'static [&'static str],
    tool_name_fields: &'static [&'static str],
    model_fields: &'static [&'static str],
    file_path_fields: &'static [&'static str],
    pre_event_names: &'static [&'static str],
    post_event_names: &'static [&'static str],
    legacy_event_names: &'static [&'static str],
    uses_workspace_roots: bool,
    path_normalize: PathNormalize,
    /// When true, unknown hook events fall through to PostFileEdit instead of erroring.
    /// Used by windsurf which has a catch-all post behavior.
    unknown_event_is_post: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PathNormalize {
    None,
    #[cfg(windows)]
    WindowsDriveLetter,
}

fn get_agent_config(name: &str) -> Option<&'static AgentConfig> {
    AGENT_CONFIGS.iter().find(|c| c.tool_name == name)
}

static AGENT_CONFIGS: &[AgentConfig] = &[
    AgentConfig {
        agent: Agent::Cursor,
        tool_name: "cursor",
        session_id_fields: &["conversation_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["preToolUse"],
        post_event_names: &["postToolUse"],
        legacy_event_names: &["beforeSubmitPrompt", "afterFileEdit"],
        uses_workspace_roots: true,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Claude,
        tool_name: "claude",
        session_id_fields: &["session_id", "thread_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path", "filepath", "path"],
        pre_event_names: &["PreToolUse"],
        post_event_names: &["PostToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Gemini,
        tool_name: "gemini",
        session_id_fields: &["session_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["BeforeTool"],
        post_event_names: &["AfterTool"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Codex,
        tool_name: "codex",
        session_id_fields: &["session_id", "thread_id", "thread-id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name", "hookEventName"],
        tool_name_fields: &["tool_name", "toolName"],
        model_fields: &["model"],
        file_path_fields: &["file_path", "filepath", "path"],
        pre_event_names: &["PreToolUse", "preToolUse"],
        post_event_names: &["PostToolUse", "postToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::GithubCopilot,
        tool_name: "github-copilot",
        session_id_fields: &["sessionId", "session_id"],
        cwd_fields: &["workspaceFolder", "workspace_folder", "cwd"],
        hook_event_fields: &["hook_event_name", "hookEventName"],
        tool_name_fields: &["tool_name", "toolName"],
        model_fields: &["model"],
        file_path_fields: &["file_path", "filepath", "filePath", "path"],
        pre_event_names: &["before_edit", "PreToolUse"],
        post_event_names: &["after_edit", "PostToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Windsurf,
        tool_name: "windsurf",
        session_id_fields: &["trajectory_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["agent_action_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model_name", "model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["pre_write_code", "pre_run_command"],
        post_event_names: &["post_write_code", "post_run_command", "post_cascade_response_with_transcript", "post_code_action"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: true,
    },
    AgentConfig {
        agent: Agent::Amp,
        tool_name: "amp",
        session_id_fields: &["thread_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["path", "file_path"],
        pre_event_names: &["PreToolUse"],
        post_event_names: &["PostToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::ContinueCli,
        tool_name: "continue-cli",
        session_id_fields: &["session_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["PreToolUse"],
        post_event_names: &["PostToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Droid,
        tool_name: "droid",
        session_id_fields: &["sessionId", "session_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hookEventName", "hook_event_name"],
        tool_name_fields: &["toolName", "tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["PreToolUse", "preToolUse"],
        post_event_names: &["PostToolUse", "postToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Firebender,
        tool_name: "firebender",
        session_id_fields: &["completion_id"],
        cwd_fields: &["repo_working_dir"],
        hook_event_fields: &["hook_event_name", "hookEventName"],
        tool_name_fields: &["tool_name", "toolName"],
        model_fields: &["model"],
        file_path_fields: &["file_path", "path"],
        pre_event_names: &["preToolUse"],
        post_event_names: &["postToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: true,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::OpenCode,
        tool_name: "opencode",
        session_id_fields: &["session_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["filePath", "file_path"],
        pre_event_names: &["PreToolUse", "preToolUse"],
        post_event_names: &["PostToolUse", "postToolUse"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::Pi,
        tool_name: "pi",
        session_id_fields: &["session_id"],
        cwd_fields: &["cwd"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool_name"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["before_edit"],
        post_event_names: &["after_edit"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
    AgentConfig {
        agent: Agent::AiTab,
        tool_name: "ai_tab",
        session_id_fields: &["completion_id"],
        cwd_fields: &["repo_working_dir", "workspace_folder"],
        hook_event_fields: &["hook_event_name"],
        tool_name_fields: &["tool"],
        model_fields: &["model"],
        file_path_fields: &["file_path"],
        pre_event_names: &["before_edit"],
        post_event_names: &["after_edit"],
        legacy_event_names: &[],
        uses_workspace_roots: false,
        path_normalize: PathNormalize::None,
        unknown_event_is_post: false,
    },
];

/// Parse a hook payload from stdin for the given agent preset name.
/// Returns the parsed event(s) or an error message.
pub fn parse_hook_input(agent_name: &str, hook_input: &str) -> Result<Vec<ParsedHookEvent>, String> {
    // Strip UTF-8 BOM if present
    let input = hook_input.strip_prefix('\u{feff}').unwrap_or(hook_input);

    // Built-in simple presets (no JSON parsing needed)
    match agent_name {
        "human" | "mock_ai" | "mock_known_human" | "known_human" => {
            return parse_simple_preset(agent_name, input);
        }
        "agent-v1" => {
            return parse_agent_v1_preset(input);
        }
        _ => {}
    }

    let config = get_agent_config(agent_name)
        .ok_or_else(|| format!("Unknown agent preset: {}", agent_name))?;

    let data: serde_json::Value = serde_json::from_str(input)
        .map_err(|e| format!("Invalid JSON in hook_input: {}", e))?;

    // Extract hook event name
    let hook_event = extract_multi(&data, config.hook_event_fields)
        .ok_or_else(|| format!("{}: hook_event_name not found in hook_input (tried: {:?})",
            agent_name, config.hook_event_fields))?;

    // Reject legacy events
    if config.legacy_event_names.contains(&hook_event.as_str()) {
        return Err(format!("Legacy {} hook event '{}' is no longer supported", agent_name, hook_event));
    }

    // Determine pre vs post
    let is_pre = config.pre_event_names.contains(&hook_event.as_str());
    let is_post = config.post_event_names.contains(&hook_event.as_str());
    if !is_pre && !is_post {
        if config.unknown_event_is_post {
            // Agents like windsurf treat unknown events as post file edits
        } else {
            return Err(format!("{}: invalid hook_event_name '{}'. Expected one of: {:?} or {:?}",
                agent_name, hook_event, config.pre_event_names, config.post_event_names));
        }
    }

    // Extract session ID
    let session_id = extract_multi(&data, config.session_id_fields)
        .unwrap_or_else(|| "unknown".to_string());

    // Extract tool name and classify.
    // For agents like windsurf that encode tool type in the event name,
    // derive tool_name from the event when the field isn't present.
    let tool_name = extract_multi(&data, config.tool_name_fields).unwrap_or_default();
    let effective_tool_name = if tool_name.is_empty() && config.agent == Agent::Windsurf {
        if hook_event.contains("run_command") { "run_command" } else { "code_action" }
    } else {
        &tool_name
    };
    let mut tool_class = classify_tool(config.agent, effective_tool_name);
    // When tool_name is missing and classify_tool returns Skip, infer from the
    // presence of file_path in tool_input. This handles agents (e.g., Claude) that
    // may omit tool_name in hook payloads but provide file_path.
    if tool_class == ToolClass::Skip && tool_name.is_empty() {
        let has_file_path = extract_file_path(&data, config.file_path_fields).is_some();
        tool_class = if has_file_path { ToolClass::FileEdit } else { ToolClass::Bash };
    }
    if tool_class == ToolClass::Skip {
        return Err(format!("Skipping {} hook for unsupported tool_name '{}'", agent_name, effective_tool_name));
    }

    // Extract model
    let model = extract_multi(&data, config.model_fields).unwrap_or_else(|| "unknown".to_string());

    // Extract file path from tool_input
    let file_path = extract_file_path(&data, config.file_path_fields);
    let file_path = file_path.map(|p| normalize_path(&p, config.path_normalize));

    // Resolve CWD
    let cwd = if config.uses_workspace_roots {
        resolve_cwd_from_workspace_roots(&data, file_path.as_deref())
    } else {
        extract_multi(&data, config.cwd_fields)
    }.ok_or_else(|| format!("{}: could not determine working directory", agent_name))?;

    // Resolve file paths to absolute
    let file_paths = if let Some(fp) = &file_path {
        vec![resolve_absolute(fp, &cwd)]
    } else {
        // Try will_edit_filepaths / edited_filepaths for agents that use those
        extract_file_path_array(&data, &cwd)
    };

    // Extract transcript path
    let transcript_path = extract_multi(&data, &["transcript_path", "session_path"])
        .map(PathBuf::from);

    // Extract tool_use_id
    let tool_use_id = extract_multi(&data, &["tool_use_id", "toolUseId", "execution_id"])
        .unwrap_or_else(|| "bash".to_string());

    // Build metadata
    let mut metadata = HashMap::new();
    if let Some(ref tp) = transcript_path {
        metadata.insert("transcript_path".to_string(), tp.to_string_lossy().to_string());
    }

    let context = PresetContext {
        agent_tool: agent_name.to_string(),
        agent_session_id: session_id,
        agent_model: model,
        cwd: PathBuf::from(&cwd),
        metadata,
    };

    // Validate that pre-file-edit events have at least one file path
    if tool_class == ToolClass::FileEdit && is_pre && file_paths.is_empty() {
        return Err(format!("{}: will_edit_filepaths cannot be empty for before_edit events", agent_name));
    }

    let event = match (tool_class, is_pre) {
        (ToolClass::Bash, true) => ParsedHookEvent::PreBashCall(PreBashCall {
            context,
            tool_use_id,
        }),
        (ToolClass::Bash, false) => ParsedHookEvent::PostBashCall(PostBashCall {
            context,
            tool_use_id,
            transcript_path,
        }),
        (ToolClass::FileEdit, true) => {
            // For file-creation tools (create_file, create), synthesize empty
            // dirty_files so the Pre checkpoint records an empty "before" state.
            // This ensures the Post checkpoint correctly attributes all lines to AI.
            let dirty_files = if is_file_creation_tool(effective_tool_name) {
                let mut empty_map = HashMap::new();
                for fp in &file_paths {
                    empty_map.insert(fp.clone(), String::new());
                }
                Some(empty_map)
            } else {
                extract_dirty_files(&data, &cwd)
            };
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files,
                tool_use_id: Some(tool_use_id),
            })
        }
        (ToolClass::FileEdit, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
            context,
            file_paths,
            dirty_files: extract_dirty_files(&data, &cwd),
            transcript_path,
            tool_use_id: Some(tool_use_id),
        }),
        (ToolClass::Skip, _) => unreachable!(),
    };

    Ok(vec![event])
}

/// Read hook input from stdin.
pub fn read_stdin() -> String {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    input
}

/// Returns true if the tool name indicates a file-creation operation
/// (as opposed to a file-edit operation).
fn is_file_creation_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "create_file" | "create" | "Write" | "write_file" | "write"
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn parse_simple_preset(name: &str, input: &str) -> Result<Vec<ParsedHookEvent>, String> {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."));

    let file_paths = if !input.trim().is_empty() {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(input) {
            if let Some(fp) = data.get("file_path").and_then(|v| v.as_str()) {
                vec![resolve_absolute(fp, &cwd.to_string_lossy())]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let event = match name {
        "known_human" | "mock_known_human" => ParsedHookEvent::KnownHumanEdit(KnownHumanEdit {
            cwd,
            file_paths,
            dirty_files: None,
        }),
        "mock_ai" => {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context: PresetContext {
                    agent_tool: "mock_ai".to_string(),
                    agent_session_id: format!("test_session_{}", ts),
                    agent_model: "test".to_string(),
                    cwd,
                    metadata: HashMap::new(),
                },
                file_paths,
                dirty_files: None,
                transcript_path: None,
                tool_use_id: None,
            })
        }
        _ => ParsedHookEvent::UntrackedEdit(UntrackedEdit {
            cwd,
            file_paths,
        }),
    };

    Ok(vec![event])
}

/// Parse the agent-v1 preset format.
/// This is a special format used by the generic agent protocol:
/// - `type: "human"` with `will_edit_filepaths` => PreFileEdit (pre-edit snapshot)
/// - `type: "ai_agent"` with `edited_filepaths` => PostFileEdit (post-edit snapshot)
fn parse_agent_v1_preset(input: &str) -> Result<Vec<ParsedHookEvent>, String> {
    let data: serde_json::Value = serde_json::from_str(input)
        .map_err(|e| format!("agent-v1: invalid JSON: {}", e))?;

    let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("human");
    let cwd = data.get("repo_working_dir")
        .and_then(|v| v.as_str())
        .map(|s| PathBuf::from(s))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd_str = cwd.to_string_lossy().to_string();

    // Extract dirty_files if present
    let dirty_files = data.get("dirty_files")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    v.as_str().map(|content| {
                        (resolve_absolute(k, &cwd_str), content.to_string())
                    })
                })
                .collect::<HashMap<PathBuf, String>>()
        })
        .filter(|m| !m.is_empty());

    match event_type {
        "human" => {
            // Pre-edit: extract will_edit_filepaths
            let file_paths = data.get("will_edit_filepaths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|p| resolve_absolute(p, &cwd_str))
                        .collect::<Vec<PathBuf>>()
                })
                .unwrap_or_default();

            Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                context: PresetContext {
                    agent_tool: "agent-v1".to_string(),
                    agent_session_id: "agent-v1".to_string(),
                    agent_model: "unknown".to_string(),
                    cwd,
                    metadata: HashMap::new(),
                },
                file_paths,
                dirty_files,
                tool_use_id: None,
            })])
        }
        "ai_agent" => {
            // Post-edit: extract edited_filepaths
            let file_paths = data.get("edited_filepaths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|p| resolve_absolute(p, &cwd_str))
                        .collect::<Vec<PathBuf>>()
                })
                .unwrap_or_default();

            let agent_name = data.get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("agent-v1");
            let model = data.get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let conversation_id = data.get("conversation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
                context: PresetContext {
                    agent_tool: agent_name.to_string(),
                    agent_session_id: conversation_id.to_string(),
                    agent_model: model.to_string(),
                    cwd,
                    metadata: HashMap::new(),
                },
                file_paths,
                dirty_files,
                transcript_path: None,
                tool_use_id: None,
            })])
        }
        _ => {
            Err(format!("agent-v1: unknown type '{}'", event_type))
        }
    }
}

fn extract_multi(data: &serde_json::Value, keys: &[&str]) -> Option<String> {
    // Check top-level fields first
    for key in keys {
        if let Some(v) = data.get(*key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // Fall back to checking inside tool_info / tool_input (windsurf, etc.)
    for nested in &["tool_info", "tool_input", "toolInput"] {
        if let Some(obj) = data.get(*nested) {
            for key in keys {
                if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn extract_file_path(data: &serde_json::Value, field_names: &[&str]) -> Option<String> {
    for container_key in &["tool_input", "toolInput", "tool_info"] {
        if let Some(container) = data.get(*container_key) {
            for field in field_names {
                if let Some(v) = container.get(*field).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn extract_file_path_array(data: &serde_json::Value, cwd: &str) -> Vec<PathBuf> {
    // Try will_edit_filepaths, edited_filepaths (used by copilot, ai_tab, firebender)
    for key in &["will_edit_filepaths", "edited_filepaths", "file_paths", "filepaths"] {
        if let Some(arr) = data.get(*key).and_then(|v| v.as_array()) {
            let paths: Vec<PathBuf> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|p| resolve_absolute(p, cwd))
                .collect();
            if !paths.is_empty() {
                return paths;
            }
        }
    }
    vec![]
}

fn extract_dirty_files(data: &serde_json::Value, cwd: &str) -> Option<HashMap<PathBuf, String>> {
    let df = data.get("dirty_files")?;
    let obj = df.as_object()?;
    let mut result = HashMap::new();
    for (key, value) in obj {
        if let Some(content) = value.as_str() {
            let path = resolve_absolute(key, cwd);
            result.insert(path, content.to_string());
        }
    }
    if result.is_empty() { None } else { Some(result) }
}

fn resolve_cwd_from_workspace_roots(data: &serde_json::Value, file_path: Option<&str>) -> Option<String> {
    let roots = data.get("workspace_roots")?.as_array()?;
    let workspace_roots: Vec<String> = roots
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.to_string())
        .collect();

    if workspace_roots.is_empty() {
        return None;
    }

    if let Some(fp) = file_path {
        if !fp.is_empty() {
            if let Some(matched) = matching_workspace_root(fp, &workspace_roots) {
                return Some(matched);
            }
        }
    }

    Some(workspace_roots[0].clone())
}

fn matching_workspace_root(file_path: &str, workspace_roots: &[String]) -> Option<String> {
    workspace_roots
        .iter()
        .find(|root| {
            let root_str = root.as_str();
            file_path.starts_with(root_str)
                && (file_path.len() == root_str.len()
                    || file_path[root_str.len()..].starts_with('/')
                    || file_path[root_str.len()..].starts_with('\\')
                    || root_str.ends_with('/')
                    || root_str.ends_with('\\'))
        })
        .cloned()
}

fn resolve_absolute(path: &str, cwd: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    }
}

fn normalize_path(path: &str, _mode: PathNormalize) -> String {
    #[cfg(windows)]
    if _mode == PathNormalize::WindowsDriveLetter {
        let mut chars = path.chars();
        if chars.next() == Some('/')
            && let (Some(drive), Some(':')) = (chars.next(), chars.next())
            && drive.is_ascii_alphabetic()
        {
            let rest: String = chars.collect();
            let normalized_rest = rest.replace('/', "\\");
            return format!("{}:{}", drive.to_ascii_uppercase(), normalized_rest);
        }
    }
    path.to_string()
}

/// Returns all known agent preset names.
pub fn known_presets() -> Vec<&'static str> {
    let mut names: Vec<&str> = AGENT_CONFIGS.iter().map(|c| c.tool_name).collect();
    names.extend_from_slice(&["human", "mock_ai", "mock_known_human", "known_human", "agent-v1"]);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_pre_file_edit() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "model": "claude-3-5-sonnet",
            "transcript_path": "/home/user/.cursor/transcripts/conv-123.jsonl",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_tool, "cursor");
                assert_eq!(e.context.agent_session_id, "conv-123");
                assert_eq!(e.context.agent_model, "claude-3-5-sonnet");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/main.rs")]);
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_post_file_edit() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "postToolUse",
            "tool_name": "Write",
            "model": "claude-3-5-sonnet",
            "transcript_path": "/home/user/.cursor/transcripts/conv-123.jsonl",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_tool, "cursor");
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/main.rs")]);
                assert_eq!(e.transcript_path, Some(PathBuf::from("/home/user/.cursor/transcripts/conv-123.jsonl")));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_cursor_skips_non_edit_tools() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let result = parse_hook_input("cursor", &input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported tool_name"));
    }

    #[test]
    fn test_cursor_rejects_legacy_events() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "beforeSubmitPrompt",
        }).to_string();

        let result = parse_hook_input("cursor", &input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no longer supported"));
    }

    #[test]
    fn test_cursor_requires_conversation_id() {
        let input = serde_json::json!({
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                // v2 behavior: falls back to "unknown" instead of erroring
                assert_eq!(e.context.agent_session_id, "unknown");
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_missing_workspace_roots() {
        let input = serde_json::json!({
            "conversation_id": "test-conv",
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let result = parse_hook_input("cursor", &input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("could not determine working directory"));
    }

    #[test]
    fn test_cursor_absolute_file_path() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "StrReplace",
            "tool_input": {"file_path": "/home/user/project/src/lib.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/lib.rs")]);
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_multiple_workspace_roots() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project-a", "/home/user/project-b"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "/home/user/project-b/src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project-b"));
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_cursor_shell_tool_pre() {
        let input = serde_json::json!({
            "conversation_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "preToolUse",
            "tool_name": "Shell",
            "tool_use_id": "tu-shell-1",
            "model": "composer-2",
            "tool_input": {"command": "date > current_time.txt"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_tool, "cursor");
                assert_eq!(e.context.agent_session_id, "conv-shell");
                assert_eq!(e.context.agent_model, "composer-2");
                assert_eq!(e.context.cwd, PathBuf::from("/Users/aidan/Desktop/test-repo"));
                assert_eq!(e.tool_use_id, "tu-shell-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_cursor_shell_tool_post() {
        let input = serde_json::json!({
            "conversation_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "postToolUse",
            "tool_name": "Shell",
            "tool_use_id": "tu-shell-2",
            "model": "composer-2",
            "tool_input": {"command": "date > current_time.txt"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_tool, "cursor");
                assert_eq!(e.tool_use_id, "tu-shell-2");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_cursor_shell_default_tool_use_id() {
        let input = serde_json::json!({
            "conversation_id": "conv-shell",
            "workspace_roots": ["/Users/aidan/Desktop/test-repo"],
            "hook_event_name": "preToolUse",
            "tool_name": "Shell",
            "tool_input": {"command": "ls"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_cursor_delete_tool() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "postToolUse",
            "tool_name": "Delete",
            "model": "claude-3-5-sonnet",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        assert!(matches!(events[0], ParsedHookEvent::PostFileEdit(_)));
    }

    #[test]
    fn test_cursor_no_transcript_path() {
        let input = serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "postToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();

        let events = parse_hook_input("cursor", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(e.transcript_path.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_utf8_bom_stripped() {
        let input = format!("\u{feff}{}", serde_json::json!({
            "conversation_id": "conv-123",
            "workspace_roots": ["/home/user/project"],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        }));

        let events = parse_hook_input("cursor", &input).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_unknown_preset() {
        let result = parse_hook_input("nonexistent-agent", "{}");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown agent preset"));
    }

    #[test]
    fn test_claude_pre_file_edit() {
        let input = serde_json::json!({
            "cwd": "/home/user/project",
            "session_id": "session-abc",
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "model": "claude-sonnet-4-20250514",
            "tool_input": {"file_path": "src/lib.rs"}
        }).to_string();

        let events = parse_hook_input("claude", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_tool, "claude");
                assert_eq!(e.context.agent_session_id, "session-abc");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/lib.rs")]);
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_gemini_after_tool() {
        let input = serde_json::json!({
            "cwd": "/repo",
            "session_id": "gem-session",
            "hook_event_name": "AfterTool",
            "tool_name": "write_file",
            "model": "gemini-2.5-pro",
            "tool_input": {"file_path": "main.py"}
        }).to_string();

        let events = parse_hook_input("gemini", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_tool, "gemini");
                assert_eq!(e.context.agent_model, "gemini-2.5-pro");
                assert_eq!(e.file_paths, vec![PathBuf::from("/repo/main.py")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codex_pre_bash() {
        let input = serde_json::json!({
            "cwd": "/project",
            "session_id": "codex-s1",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tu-1",
            "tool_input": {"command": "cargo build"}
        }).to_string();

        let events = parse_hook_input("codex", &input).unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_tool, "codex");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_matching_workspace_root_basic() {
        let roots = vec![
            "/home/user/project-a".to_string(),
            "/home/user/project-b".to_string(),
        ];
        assert_eq!(
            matching_workspace_root("/home/user/project-b/src/main.rs", &roots),
            Some("/home/user/project-b".to_string())
        );
        assert_eq!(matching_workspace_root("/other/path/file.rs", &roots), None);
    }

    #[test]
    fn test_workspace_root_ambiguous_prefix() {
        let roots = vec![
            "/home/user/workspace1".to_string(),
            "/home/user/workspace10".to_string(),
        ];
        // workspace10/file.rs should NOT match workspace1 (no path separator after "1")
        assert_eq!(
            matching_workspace_root("/home/user/workspace10/src/file.rs", &roots),
            Some("/home/user/workspace10".to_string())
        );
    }

    #[test]
    fn test_known_presets_list() {
        let presets = known_presets();
        assert!(presets.contains(&"cursor"));
        assert!(presets.contains(&"claude"));
        assert!(presets.contains(&"human"));
        assert!(presets.contains(&"mock_ai"));
    }

    #[test]
    fn test_windsurf_post_code_action() {
        let input = serde_json::json!({
            "trajectory_id": "traj-456",
            "agent_action_name": "post_code_action",
            "model_name": "gpt-4",
            "tool_info": {
                "cwd": "/home/user/project",
                "file_path": "src/lib.rs"
            }
        }).to_string();
        let events = parse_hook_input("windsurf", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_tool, "windsurf");
                assert_eq!(e.context.agent_session_id, "traj-456");
                assert_eq!(e.context.agent_model, "gpt-4");
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/lib.rs")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_windsurf_pre_run_command() {
        let input = serde_json::json!({
            "trajectory_id": "traj-789",
            "agent_action_name": "pre_run_command",
            "cwd": "/home/user/project",
            "execution_id": "exec-1"
        }).to_string();
        let events = parse_hook_input("windsurf", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_tool, "windsurf");
                assert_eq!(e.tool_use_id, "exec-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_windsurf_post_run_command() {
        let input = serde_json::json!({
            "trajectory_id": "traj-789",
            "agent_action_name": "post_run_command",
            "cwd": "/home/user/project",
            "execution_id": "exec-2"
        }).to_string();
        let events = parse_hook_input("windsurf", &input).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_tool, "windsurf");
                assert_eq!(e.tool_use_id, "exec-2");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_windsurf_unknown_event_fallback() {
        let input = serde_json::json!({
            "trajectory_id": "traj-000",
            "agent_action_name": "some_future_event",
            "tool_info": {
                "cwd": "/home/user/project",
                "file_path": "file.txt"
            }
        }).to_string();
        let events = parse_hook_input("windsurf", &input).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ParsedHookEvent::PostFileEdit(_)));
    }
}
