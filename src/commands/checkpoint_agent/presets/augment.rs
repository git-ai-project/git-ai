//! Augment Code preset — supports both the v1 (`auggie`) and v2
//! (`auggie-v2` / `cosmos-agent`) hook protocols from a single preset,
//! autodetected per-payload from its shape. Both generations write hook
//! commands into the same `~/.augment/settings.json`
//! (`mdm/agents/augment.rs`), so no installer/config change is needed to
//! support either.
//!
//! ## Autodetection
//!
//! Dispatch is purely shape-based (never config/env/flag-based), decided by
//! KEY PRESENCE (a JSON `null` counts as absent), never by the
//! discriminator's value type:
//!   - `hook_event_name` present, `hook_type` absent → v1
//!   - `hook_type` present, `hook_event_name` absent → v2
//!   - both present, or neither present → `PresetError` (never guess)
//!   - once routed, a present-but-non-string discriminator is also a
//!     `PresetError`, not a silent fallback
//!
//! ## v1 (`auggie`) protocol
//!
//! Single-line JSON on stdin per <https://docs.augmentcode.com/cli/hooks>.
//! Top level: `hook_event_name` (`PreToolUse`/`PostToolUse`/`SessionStart`/
//! `SessionEnd`/`Stop`), `conversation_id`, `workspace_roots[]`. Per-event:
//! `PreToolUse`/`PostToolUse` add `tool_name`, `tool_input`, and (Post only)
//! `tool_output`/`tool_error`/`file_changes[]`. Tool names are kebab-case:
//! `save-file`/`str-replace-editor` (field `tool_input.path`),
//! `remove-files` (field `tool_input.file_paths[]`), `apply_patch` (field
//! `tool_input.input`, a patch-format string with `*** Add/Update/Delete
//! File: <path>` markers), `launch-process` (bash; field
//! `tool_input.command`). There is no `transcript_path`; `stream_source` is
//! always `None`. `context.modelName` is present only when the hook config
//! sets `metadata.includeUserContext`; otherwise the model is `"unknown"`.
//! `SessionStart`/`SessionEnd`/`Stop` carry no tool/file data and are a
//! silent no-op.
//!
//! ## v2 (`auggie-v2` / `cosmos-agent`) protocol
//!
//! Discriminator is `hook_type` (same event names as v1, plus
//! `Notification`/`PromptSubmit`, also no-ops). `tool_name` uses generic
//! Claude-Code-style names (`read`/`write`/`edit`) instead of v1's
//! kebab-case for file tools; the shell tool is accepted as either
//! `terminal` (the documented current name, per
//! docs.augmentcode.com/cli/permissions) or `bash` (the ACP/session
//! tool-call model's name for shell calls), since both are routed to the
//! bash/stat-diff path. `tool_input` has no `workspace_roots`; the hook
//! subprocess inherits the CLI process's own cwd (it is not chdir'd per
//! hook), which is the workspace root when auggie is launched from the
//! workspace, so `std::env::current_dir()` recovers it. The ordinary v2
//! hook payload carries no session id, conversation id, cwd, transcript
//! reference, or tool-call id at all: lifecycle events are only
//! `{hook_type}` and PostToolUse is only `{hook_type, tool_name,
//! tool_input, tool_result/tool_error}`. A sole candidate file under
//! `~/.augment/sessions-v2/` is not a safe substitute either — it can be
//! stale, concurrent sessions can share a workspace, resume reuses a
//! stored id, and fork copies history under a new id — so v2 deliberately
//! never mines the session directory or reads environment variables to
//! recover one. Absent a payload id, session id is the stable
//! `generate_session_id(cwd, "augment")` hash and model is always
//! `"unknown"`, trading away best-effort real id/model recovery for the
//! guarantee that a v2 checkpoint can never be misattributed to the wrong
//! session. When the payload *does* carry a non-empty string
//! `conversation_id` — the same field v1 already documents — it is used
//! verbatim as the session id instead, so a future hook-API addition or an
//! extension bridge that forwards the real conversation id is picked up
//! without a git-ai release; a JSON `null` or absent key falls back to the
//! hash, and a present-but-wrong-type or present-but-empty value is a
//! `PresetError`.

use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct AugmentPreset;

/// Lifecycle/notification hook names (v1 and v2 combined) that carry no
/// tool/file information and are deliberately not checkpointed. Kept as an
/// explicit allowlist, distinct from `ToolClass::Skip`, so a genuinely
/// unknown/malformed discriminator value still fails loudly.
fn is_lifecycle_event(name: &str) -> bool {
    matches!(
        name,
        "SessionStart" | "SessionEnd" | "Stop" | "Notification" | "PromptSubmit"
    )
}

/// Human-readable JSON value type name for actionable type-mismatch errors.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Extracts the file path(s) touched by a Pre/PostToolUse `tool_input`,
/// shared by both v1 and v2 (v2's toolset only ever sends `path`, but this
/// also covers a hypothetical future `apply_patch` tool_name under v2).
fn extract_file_paths(data: &serde_json::Value, workspace_root: &str) -> Vec<PathBuf> {
    let Some(tool_input) = data.get("tool_input") else {
        return vec![];
    };

    // `remove-files` sends `file_paths` as an array (v1 only).
    if let Some(arr) = tool_input.get("file_paths").and_then(|v| v.as_array()) {
        let paths: Vec<PathBuf> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|p| parse::resolve_absolute(p, workspace_root))
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }

    // `save-file`/`str-replace-editor` (v1) and `write`/`edit` (v2) send `path`.
    if let Some(path) = tool_input.get("path").and_then(|v| v.as_str())
        && !path.is_empty()
    {
        return vec![parse::resolve_absolute(path, workspace_root)];
    }

    // `apply_patch` sends the whole patch as `input`; file path(s) are
    // embedded in `*** Add/Update/Delete File: <path>` marker lines.
    if let Some(patch) = tool_input.get("input").and_then(|v| v.as_str()) {
        let mut raw_paths: Vec<String> = Vec::new();
        parse::collect_apply_patch_paths_from_text(patch, &mut raw_paths);
        let paths: Vec<PathBuf> = raw_paths
            .iter()
            .map(|p| parse::resolve_absolute(p, workspace_root))
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }

    vec![]
}

/// v1's PostToolUse `file_changes[].path` is authoritative when present
/// (captures the actual mutation); callers fall back to `extract_file_paths`
/// when it's empty.
fn extract_post_file_changes(data: &serde_json::Value, workspace_root: &str) -> Vec<PathBuf> {
    data.get("file_changes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|change| change.get("path").and_then(|p| p.as_str()))
                .filter(|s| !s.is_empty())
                .map(|p| parse::resolve_absolute(p, workspace_root))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

impl AgentPreset for AugmentPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let has_v1_shape = data.get("hook_event_name").is_some_and(|v| !v.is_null());
        let has_v2_shape = data.get("hook_type").is_some_and(|v| !v.is_null());

        match (has_v1_shape, has_v2_shape) {
            (true, false) => parse_v1(&data, trace_id),
            (false, true) => parse_v2(&data, trace_id),
            (true, true) => Err(GitAiError::PresetError(
                "Ambiguous Augment hook_input: both hook_event_name (v1) and hook_type (v2) present"
                    .to_string(),
            )),
            (false, false) => Err(GitAiError::PresetError(
                "Unrecognized Augment hook_input: neither hook_event_name (v1) nor hook_type (v2) present"
                    .to_string(),
            )),
        }
    }
}

fn parse_v1(data: &serde_json::Value, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    if let Some(v) = data.get("hook_event_name")
        && v.as_str().is_none()
    {
        return Err(GitAiError::PresetError(format!(
            "Augment hook_event_name must be a string, got {}",
            json_type_name(v)
        )));
    }

    let conversation_id = parse::required_str(data, "conversation_id")?.to_string();
    let workspace_root = data
        .get("workspace_roots")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            GitAiError::PresetError(
                "workspace_roots[0] not found in Augment hook_input".to_string(),
            )
        })?
        .to_string();

    let tool_name = parse::optional_str(data, "tool_name");
    let hook_event = parse::optional_str(data, "hook_event_name");
    let tool_class = tool_name
        .map(|n| bash_tool::classify_tool(Agent::Augment, n))
        .unwrap_or(ToolClass::Skip);

    // context.modelName is only present when metadata.includeUserContext is set.
    let model = data
        .get("context")
        .and_then(|c| c.get("modelName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();

    let context = PresetContext {
        agent_id: AgentId {
            tool: "augment".to_string(),
            id: conversation_id.clone(),
            model,
        },
        external_session_id: conversation_id,
        trace_id: trace_id.to_string(),
        cwd: PathBuf::from(&workspace_root),
        metadata: HashMap::new(),
    };

    build_tool_event(
        hook_event,
        tool_class,
        &context,
        data,
        &workspace_root,
        |d, root| {
            let post = extract_post_file_changes(d, root);
            if post.is_empty() {
                extract_file_paths(d, root)
            } else {
                post
            }
        },
    )
    .map_err(|hook_event| {
        GitAiError::PresetError(format!(
            "Unsupported Augment hook_event_name: {}",
            hook_event.unwrap_or("<missing>")
        ))
    })
}

fn parse_v2(data: &serde_json::Value, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    if let Some(v) = data.get("hook_type")
        && v.as_str().is_none()
    {
        return Err(GitAiError::PresetError(format!(
            "Augment hook_type must be a string, got {}",
            json_type_name(v)
        )));
    }

    let hook_type = parse::optional_str(data, "hook_type");
    let tool_name = parse::optional_str(data, "tool_name");

    // v2 never sends workspace_roots; the hook subprocess inherits the
    // CLI process's own cwd (it is not chdir'd per hook), which IS the
    // workspace root when auggie is launched from the workspace.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = cwd.to_string_lossy().to_string();

    let tool_class = tool_name
        .map(|n| bash_tool::classify_tool(Agent::Augment, n))
        .unwrap_or(ToolClass::Skip);

    // No on-disk session mining (see module docs): a stable cwd-derived
    // hash is the default, but a payload-supplied `conversation_id` (the
    // same field v1 already sends) is honoured verbatim when present, so
    // a future hook-API addition or an extension bridge that forwards the
    // real conversation id is picked up without a git-ai release. Absent
    // or JSON `null` falls back to the hash; present-but-wrong-type or
    // present-but-empty is a `PresetError`, never a silent fallback.
    let conversation_id = match data.get("conversation_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(serde_json::Value::String(_)) => {
            return Err(GitAiError::PresetError(
                "Augment v2 conversation_id must not be empty".to_string(),
            ));
        }
        Some(v) => {
            return Err(GitAiError::PresetError(format!(
                "Augment v2 conversation_id must be a string, got {}",
                json_type_name(v)
            )));
        }
    };
    let session_id =
        conversation_id.unwrap_or_else(|| generate_session_id(&workspace_root, "augment"));
    let model = "unknown".to_string();

    let context = PresetContext {
        agent_id: AgentId {
            tool: "augment".to_string(),
            id: session_id.clone(),
            model,
        },
        external_session_id: session_id,
        trace_id: trace_id.to_string(),
        cwd,
        metadata: HashMap::new(),
    };

    build_tool_event(
        hook_type,
        tool_class,
        &context,
        data,
        &workspace_root,
        extract_file_paths,
    )
    .map_err(|hook_type| {
        GitAiError::PresetError(format!(
            "Unsupported Augment v2 hook_type: {}",
            hook_type.unwrap_or("<missing>")
        ))
    })
}

/// Shared PreToolUse/PostToolUse -> ParsedHookEvent dispatch for both v1 and
/// v2. `event_name` is the already-extracted discriminator value
/// (`hook_event_name` for v1, `hook_type` for v2). `file_path_fn` differs
/// only in v1's file_changes[] preference for PostToolUse.
/// Returns `Err(event_name)` for a genuinely unrecognized discriminator so
/// each caller can format its own actionable error message.
///
/// Known limitation, fixed shell tool_use_id: unlike every other preset
/// with a shell path (claude, codex, pi, cursor, droid, gemini,
/// continue_cli, ... all read a tool_use_id/toolUseId field from the hook
/// payload, falling back to a fixed id only when it's absent), neither the
/// documented v1 payload (docs.augmentcode.com/cli/hooks) nor the v2
/// fixtures this preset was built against carry any per-invocation
/// identifier (tool_use_id/tool_call_id/call_id/etc.) for launch-process /
/// terminal / bash calls. So both bash branches below always emit the
/// fixed "bash" id. daemon::bash_sessions pairs pre/post snapshots by
/// (external_session_id, tool_use_id), so two concurrent shell tool calls
/// within one Augment session share this single slot: the first call's
/// post-hook can diff against the second call's pre-snapshot (or find none
/// at all). If Augment's hook payload ever adds a stable per-call id, wire
/// it in here the same way the other presets do.
fn build_tool_event<'a>(
    event_name: Option<&'a str>,
    tool_class: ToolClass,
    context: &PresetContext,
    data: &serde_json::Value,
    workspace_root: &str,
    file_path_fn: impl Fn(&serde_json::Value, &str) -> Vec<PathBuf>,
) -> Result<Vec<ParsedHookEvent>, Option<&'a str>> {
    let is_bash = tool_class == ToolClass::Bash;
    let is_file_edit = tool_class == ToolClass::FileEdit;

    let event = match event_name {
        Some("PreToolUse") => {
            if is_bash {
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context: context.clone(),
                    // Fixed id: no per-invocation identifier is available.
                    // See the "Known limitation" note on `build_tool_event`.
                    tool_use_id: "bash".to_string(),
                    command: parse::bash_command_from_hook_input(data),
                })
            } else if is_file_edit {
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context: context.clone(),
                    file_paths: file_path_fn(data, workspace_root),
                    dirty_files: None,
                    tool_use_id: None,
                })
            } else {
                // Read-only/inspection tools are an intentional no-op, not
                // an error: the installer's catch-all ".*" matcher fires
                // this hook for every tool call, so most invocations are
                // non-mutating by design. `git-ai checkpoint` exits 0
                // either way, and Augment renders exit-0 stderr as a
                // user-visible warning, so a PresetError here would spam a
                // spurious message on ordinary, successful tool use.
                return Ok(Vec::new());
            }
        }
        Some("PostToolUse") => {
            if is_bash {
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context: context.clone(),
                    // Fixed id: no per-invocation identifier is available.
                    // See the "Known limitation" note on `build_tool_event`.
                    tool_use_id: "bash".to_string(),
                    command: parse::bash_command_from_hook_input(data),
                    // No documented on-disk transcript for either
                    // protocol generation.
                    stream_source: None,
                })
            } else if is_file_edit {
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context: context.clone(),
                    file_paths: file_path_fn(data, workspace_root),
                    dirty_files: None,
                    stream_source: None,
                    tool_use_id: None,
                })
            } else {
                return Ok(Vec::new());
            }
        }
        _ if event_name.is_some_and(is_lifecycle_event) => return Ok(Vec::new()),
        _ => return Err(event_name),
    };

    Ok(vec![event])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    /// A platform-native absolute path for v2 path-extraction tests: on
    /// Windows, `Path::is_absolute()` requires a drive/UNC prefix, so a bare
    /// POSIX-style path is not absolute there and would be (correctly)
    /// re-rooted under the test process's cwd.
    fn native_abs_path(rel: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\{}", rel.replace('/', "\\"))
        } else {
            format!("/{}", rel)
        }
    }

    fn v1_input(event: &str, tool: &str, tool_input: serde_json::Value) -> String {
        json!({
            "hook_event_name": event,
            "conversation_id": "conv-xyz789",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": tool,
            "tool_input": tool_input,
        })
        .to_string()
    }

    fn v2_input(hook_type: &str, tool: &str, tool_input: serde_json::Value) -> String {
        json!({
            "hook_type": hook_type,
            "tool_name": tool,
            "tool_input": tool_input,
        })
        .to_string()
    }

    // ------------------------------------------------------------------
    // v1 tool routing / path extraction (table-driven)
    // ------------------------------------------------------------------

    #[test]
    fn test_v1_mutating_tools_produce_expected_file_paths() {
        let cases: &[(&str, serde_json::Value, Vec<PathBuf>)] = &[
            (
                "save-file",
                json!({"path": "src/main.rs", "content": "fn main() {}"}),
                vec![PathBuf::from("/Users/me/project/src/main.rs")],
            ),
            (
                "str-replace-editor",
                json!({"path": "src/lib.rs", "command": "str_replace"}),
                vec![PathBuf::from("/Users/me/project/src/lib.rs")],
            ),
            (
                "remove-files",
                json!({"file_paths": ["src/dead.rs", "src/old.rs"]}),
                vec![
                    PathBuf::from("/Users/me/project/src/dead.rs"),
                    PathBuf::from("/Users/me/project/src/old.rs"),
                ],
            ),
            (
                "apply_patch",
                json!({"input": "*** Begin Patch\n*** Add File: hello.py\n+pass\n*** End Patch"}),
                vec![PathBuf::from("/Users/me/project/hello.py")],
            ),
            (
                "save-file",
                json!({"path": "/etc/hosts", "content": ""}),
                vec![PathBuf::from("/etc/hosts")], // already absolute, passes through
            ),
        ];
        for (tool, tool_input, expected) in cases {
            let input = v1_input("PostToolUse", tool, tool_input.clone());
            let events = AugmentPreset.parse(&input, "t_test").unwrap();
            match &events[0] {
                ParsedHookEvent::PostFileEdit(e) => {
                    assert_eq!(&e.file_paths, expected, "tool={tool}");
                }
                _ => panic!("Expected PostFileEdit for tool={tool}"),
            }
        }
    }

    #[test]
    fn test_v1_post_tool_use_prefers_file_changes_over_tool_input() {
        for (tool, tool_input) in [
            ("save-file", json!({"path": "src/old.rs"})),
            (
                "apply_patch",
                json!({"input": "*** Begin Patch\n*** Add File: greet2.py\n+pass\n*** End Patch"}),
            ),
        ] {
            let input = json!({
                "hook_event_name": "PostToolUse",
                "conversation_id": "conv-1",
                "workspace_roots": ["/Users/me/project"],
                "tool_name": tool,
                "tool_input": tool_input,
                "file_changes": [
                    {"path": "src/new.rs", "changeType": "create"},
                    {"path": "src/also.rs", "changeType": "modify"},
                ],
            })
            .to_string();
            let events = AugmentPreset.parse(&input, "t_test").unwrap();
            match &events[0] {
                ParsedHookEvent::PostFileEdit(e) => {
                    assert_eq!(
                        e.file_paths,
                        vec![
                            PathBuf::from("/Users/me/project/src/new.rs"),
                            PathBuf::from("/Users/me/project/src/also.rs"),
                        ],
                        "tool={tool}"
                    );
                }
                _ => panic!("Expected PostFileEdit"),
            }
        }
    }

    #[test]
    fn test_v1_bash_tool_pre_and_post() {
        let pre = v1_input(
            "PreToolUse",
            "launch-process",
            json!({"command": "git status"}),
        );
        match &AugmentPreset.parse(&pre, "t_test").unwrap()[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(e.tool_use_id, "bash");
                assert_eq!(e.command.as_deref(), Some("git status"));
            }
            _ => panic!("Expected PreBashCall"),
        }

        let post = v1_input("PostToolUse", "launch-process", json!({"command": "ls"}));
        match &AugmentPreset.parse(&post, "t_test").unwrap()[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.command.as_deref(), Some("ls"));
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_v1_context_and_model_fields() {
        let input = v1_input("PostToolUse", "save-file", json!({"path": "src/main.rs"}));
        let events = AugmentPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(e.context.agent_id.id, "conv-xyz789");
                assert_eq!(e.context.agent_id.model, "unknown");
                assert_eq!(e.context.external_session_id, "conv-xyz789");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/Users/me/project"));
            }
            _ => panic!("Expected PostFileEdit"),
        }

        let with_model = json!({
            "hook_event_name": "PostToolUse",
            "conversation_id": "conv-1",
            "workspace_roots": ["/Users/me/project"],
            "tool_name": "save-file",
            "tool_input": {"path": "src/main.rs"},
            "context": {"modelName": "claude-sonnet-4-5"},
        })
        .to_string();
        match &AugmentPreset.parse(&with_model, "t_test").unwrap()[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    // ------------------------------------------------------------------
    // Skip / error behavior shared by both protocol generations
    // ------------------------------------------------------------------

    #[test]
    fn test_unsupported_tools_and_lifecycle_events_skip_silently() {
        for tool in ["view", "grep-search", "web-fetch"] {
            let input = v1_input("PreToolUse", tool, json!({}));
            assert!(AugmentPreset.parse(&input, "t_test").unwrap().is_empty());
            let input = v1_input("PostToolUse", tool, json!({}));
            assert!(AugmentPreset.parse(&input, "t_test").unwrap().is_empty());
        }
        let read_input = v2_input("PreToolUse", "read", json!({"path": "foo.txt"}));
        assert!(
            AugmentPreset
                .parse(&read_input, "t_test")
                .unwrap()
                .is_empty()
        );
        for event in ["SessionStart", "SessionEnd", "Stop"] {
            let input = json!({
                "hook_event_name": event,
                "conversation_id": "conv-1",
                "workspace_roots": ["/Users/me/project"],
            })
            .to_string();
            assert!(
                AugmentPreset.parse(&input, "t_test").unwrap().is_empty(),
                "expected silent no-op for v1 {event}"
            );
        }
        for hook_type in [
            "SessionStart",
            "SessionEnd",
            "Stop",
            "Notification",
            "PromptSubmit",
        ] {
            let input = json!({"hook_type": hook_type}).to_string();
            assert!(
                AugmentPreset.parse(&input, "t_test").unwrap().is_empty(),
                "expected silent no-op for v2 {hook_type}"
            );
        }
    }

    #[test]
    fn test_malformed_and_missing_field_errors() {
        assert!(AugmentPreset.parse("not valid json", "t_test").is_err());

        // Unknown (non-lifecycle) event name still fails closed.
        let input = json!({
            "hook_event_name": "TotallyUnknownEvent",
            "conversation_id": "conv-1",
            "workspace_roots": ["/Users/me/project"],
        })
        .to_string();
        match AugmentPreset.parse(&input, "t_test") {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("Unsupported Augment hook_event_name"), "{msg}")
            }
            other => panic!("expected PresetError, got {other:?}"),
        }

        // Missing required v1 fields.
        for missing in [
            json!({"hook_event_name": "PostToolUse", "workspace_roots": ["/x"]}),
            json!({"hook_event_name": "PostToolUse", "conversation_id": "c"}),
        ] {
            assert!(AugmentPreset.parse(&missing.to_string(), "t_test").is_err());
        }
    }

    // ------------------------------------------------------------------
    // Autodetection matrix
    // ------------------------------------------------------------------

    #[test]
    fn test_autodetect_matrix() {
        // v1-only key routes to v1.
        let v1 = v1_input("PostToolUse", "save-file", json!({"path": "x"}));
        match &AugmentPreset.parse(&v1, "t_test").unwrap()[0] {
            ParsedHookEvent::PostFileEdit(e) => assert_eq!(e.context.agent_id.id, "conv-xyz789"),
            _ => panic!("Expected PostFileEdit"),
        }

        // v2-only key routes to v2.
        let v2 = v2_input("PostToolUse", "write", json!({"path": "x"}));
        assert!(matches!(
            AugmentPreset.parse(&v2, "t_test").unwrap()[0],
            ParsedHookEvent::PostFileEdit(_)
        ));

        // Both present -> ambiguous error.
        let both = json!({
            "hook_event_name": "PostToolUse",
            "hook_type": "PostToolUse",
            "conversation_id": "c",
            "workspace_roots": ["/x"],
            "tool_name": "write",
            "tool_input": {"path": "x"},
        })
        .to_string();
        match AugmentPreset.parse(&both, "t_test") {
            Err(GitAiError::PresetError(msg)) => assert!(msg.contains("Ambiguous"), "{msg}"),
            other => panic!("expected PresetError, got {other:?}"),
        }

        // Neither present -> unrecognized error.
        let neither = json!({"tool_name": "write", "tool_input": {"path": "x"}}).to_string();
        match AugmentPreset.parse(&neither, "t_test") {
            Err(GitAiError::PresetError(msg)) => assert!(msg.contains("Unrecognized"), "{msg}"),
            other => panic!("expected PresetError, got {other:?}"),
        }

        // A `null` discriminator counts as absent, so v1-shaped-plus-null-v2
        // still routes cleanly to v1 rather than being flagged ambiguous.
        let null_v2 = json!({
            "hook_event_name": "PostToolUse",
            "hook_type": null,
            "conversation_id": "c",
            "workspace_roots": ["/x"],
            "tool_name": "save-file",
            "tool_input": {"path": "x"},
        })
        .to_string();
        assert!(AugmentPreset.parse(&null_v2, "t_test").is_ok());
    }

    #[test]
    fn test_wrong_type_discriminator_after_routing_errors() {
        let input = json!({
            "hook_event_name": 123,
            "conversation_id": "c",
            "workspace_roots": ["/x"],
        })
        .to_string();
        match AugmentPreset.parse(&input, "t_test") {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("hook_event_name must be a string"), "{msg}")
            }
            other => panic!("expected PresetError, got {other:?}"),
        }

        let input = json!({"hook_type": 123}).to_string();
        match AugmentPreset.parse(&input, "t_test") {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("hook_type must be a string"), "{msg}")
            }
            other => panic!("expected PresetError, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // v2-specific behavior
    // ------------------------------------------------------------------

    #[test]
    fn test_v2_write_edit_bash_tool_routing() {
        let path = native_abs_path("tmp/proj/bar2.txt");

        let pre_write = v2_input("PreToolUse", "write", json!({"path": path, "content": "x"}));
        match &AugmentPreset.parse(&pre_write, "t_test123456789a").unwrap()[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "augment");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.file_paths, vec![PathBuf::from(&path)]);
                assert_eq!(e.context.agent_id.model, "unknown");
                assert!(!e.context.agent_id.id.is_empty());
            }
            _ => panic!("Expected PreFileEdit"),
        }

        let post_edit = v2_input(
            "PostToolUse",
            "edit",
            json!({"path": path, "edits": [{"oldText": "a", "newText": "b"}]}),
        );
        match &AugmentPreset.parse(&post_edit, "t_test").unwrap()[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from(&path)]);
                assert!(e.stream_source.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }

        let bash = v2_input("PreToolUse", "bash", json!({"command": "git status"}));
        match &AugmentPreset.parse(&bash, "t_test").unwrap()[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
                assert_eq!(e.command.as_deref(), Some("git status"));
            }
            _ => panic!("Expected PreBashCall"),
        }

        // "terminal" is the documented current v2 shell tool name
        // (docs.augmentcode.com/cli/permissions); "launch-process" is only
        // a legacy alias for v1. It must route identically to "bash".
        let terminal = v2_input("PostToolUse", "terminal", json!({"command": "ls"}));
        match &AugmentPreset.parse(&terminal, "t_test").unwrap()[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
                assert_eq!(e.command.as_deref(), Some("ls"));
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_v2_apply_patch_hypothetical_extracts_path_from_patch_text() {
        // Not part of v2's confirmed default toolset, but classify_tool's
        // shared Agent::Augment arm would still route a hypothetical future
        // "apply_patch" tool_name to FileEdit under v2, whose tool_input has
        // no "path", only a raw patch-format "input" string.
        let input = v2_input(
            "PreToolUse",
            "apply_patch",
            json!({"input": "*** Begin Patch\n*** Add File: hello.py\n+pass\n*** End Patch"}),
        );
        match &AugmentPreset.parse(&input, "t_test").unwrap()[0] {
            ParsedHookEvent::PreFileEdit(e) => assert!(e.file_paths[0].ends_with("hello.py")),
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_v2_session_id_is_stable_and_never_mines_session_files() {
        // No mining fixture is set up for this test's cwd. The MVP fallback
        // must always produce the same stable, non-empty session id and
        // default model regardless of what (if anything) exists on disk.
        let input = v2_input("PreToolUse", "write", json!({"path": "x", "content": "y"}));
        let a = AugmentPreset.parse(&input, "t_test").unwrap();
        let b = AugmentPreset.parse(&input, "t_test").unwrap();
        let id_of = |events: &[ParsedHookEvent]| match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => e.context.agent_id.id.clone(),
            _ => panic!("Expected PreFileEdit"),
        };
        assert_eq!(
            id_of(&a),
            id_of(&b),
            "session id must be stable across calls"
        );
        assert!(!id_of(&a).is_empty());
    }

    fn id_of_first(events: &[ParsedHookEvent]) -> String {
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => e.context.agent_id.id.clone(),
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_v2_conversation_id_used_verbatim_when_present() {
        let input = json!({
            "hook_type": "PreToolUse",
            "conversation_id": "conv-v2-1",
            "tool_name": "write",
            "tool_input": {"path": "x", "content": "y"},
        })
        .to_string();
        match &AugmentPreset.parse(&input, "t_test").unwrap()[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.id, "conv-v2-1");
                assert_eq!(e.context.external_session_id, "conv-v2-1");
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_v2_different_conversation_ids_in_same_cwd_produce_different_ids() {
        let make = |conv_id: &str| {
            json!({
                "hook_type": "PreToolUse",
                "conversation_id": conv_id,
                "tool_name": "write",
                "tool_input": {"path": "x", "content": "y"},
            })
            .to_string()
        };
        let a = AugmentPreset.parse(&make("conv-a"), "t_test").unwrap();
        let b = AugmentPreset.parse(&make("conv-b"), "t_test").unwrap();
        assert_ne!(id_of_first(&a), id_of_first(&b));
    }

    #[test]
    fn test_v2_missing_conversation_id_falls_back_to_generated_session_id() {
        let input = v2_input("PreToolUse", "write", json!({"path": "x", "content": "y"}));
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let expected = generate_session_id(&cwd.to_string_lossy(), "augment");
        assert_eq!(id_of_first(&events), expected);
    }

    #[test]
    fn test_v2_null_conversation_id_falls_back_to_generated_session_id() {
        let input = json!({
            "hook_type": "PreToolUse",
            "conversation_id": null,
            "tool_name": "write",
            "tool_input": {"path": "x", "content": "y"},
        })
        .to_string();
        let events = AugmentPreset.parse(&input, "t_test").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let expected = generate_session_id(&cwd.to_string_lossy(), "augment");
        assert_eq!(id_of_first(&events), expected);
    }

    #[test]
    fn test_v2_non_string_conversation_id_errors() {
        let input = json!({
            "hook_type": "PreToolUse",
            "conversation_id": 42,
            "tool_name": "write",
            "tool_input": {"path": "x", "content": "y"},
        })
        .to_string();
        match AugmentPreset.parse(&input, "t_test") {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("conversation_id must be a string"), "{msg}")
            }
            other => panic!("expected PresetError, got {other:?}"),
        }
    }

    #[test]
    fn test_v2_empty_string_conversation_id_errors() {
        let input = json!({
            "hook_type": "PreToolUse",
            "conversation_id": "",
            "tool_name": "write",
            "tool_input": {"path": "x", "content": "y"},
        })
        .to_string();
        match AugmentPreset.parse(&input, "t_test") {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("conversation_id must not be empty"), "{msg}")
            }
            other => panic!("expected PresetError, got {other:?}"),
        }
    }
}
