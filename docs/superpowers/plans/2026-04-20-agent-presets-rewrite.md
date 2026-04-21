# Agent Presets Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ~7000 lines of duplicated agent preset code with a clean parse-then-orchestrate architecture where presets are pure parsers (~1500-2000 lines total).

**Architecture:** Presets implement a single `AgentPreset` trait with a `parse()` method that returns typed `ParsedHookEvent` enums. A shared orchestrator handles bash snapshots, repo discovery, and checkpoint dispatch. `CheckpointResult` replaces `AgentRunResult` as the serializable type flowing to the daemon.

**Tech Stack:** Rust 2024 edition, serde for serialization, existing bash_tool module unchanged.

---

### Task 1: Create Core Types Module (`presets/mod.rs`)

**Files:**
- Create: `src/commands/checkpoint_agent/presets/mod.rs`
- Modify: `src/commands/checkpoint_agent/mod.rs`

- [ ] **Step 1: Create the presets directory and module file with all core types**

```rust
// src/commands/checkpoint_agent/presets/mod.rs

pub mod parse;

mod agent_v1;
mod ai_tab;
mod amp;
mod claude;
mod codex;
mod continue_cli;
mod cursor;
mod droid;
mod firebender;
mod gemini;
mod github_copilot;
mod opencode;
mod pi;
mod windsurf;

use crate::authorship::transcript::AiTranscript;
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::agent_presets::BashPreHookStrategy;
use crate::error::GitAiError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Common context present for every hook event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetContext {
    pub agent_id: AgentId,
    pub session_id: String,
    pub trace_id: String,
    pub cwd: PathBuf,
    pub metadata: HashMap<String, String>,
}

/// The sole output type from any agent preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParsedHookEvent {
    PreFileEdit(PreFileEdit),
    PostFileEdit(PostFileEdit),
    PreBashCall(PreBashCall),
    PostBashCall(PostBashCall),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub transcript_source: Option<TranscriptSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
    pub strategy: BashPreHookStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
    pub transcript_source: Option<TranscriptSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranscriptSource {
    Path {
        path: PathBuf,
        format: TranscriptFormat,
    },
    Inline(AiTranscript),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptFormat {
    ClaudeJsonl,
    GeminiJson,
    WindsurfJsonl,
    CodexJsonl,
    CursorJsonl,
    DroidJsonl,
    CopilotSessionJson,
    CopilotEventStreamJsonl,
    AmpThreadJson,
    OpenCodeSqlite,
    OpenCodeLegacyJson,
    PiJsonl,
}

/// The single trait all agent presets implement.
pub trait AgentPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError>;
}

pub fn resolve_preset(name: &str) -> Result<Box<dyn AgentPreset>, GitAiError> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudePreset)),
        "codex" => Ok(Box::new(codex::CodexPreset)),
        "gemini" => Ok(Box::new(gemini::GeminiPreset)),
        "windsurf" => Ok(Box::new(windsurf::WindsurfPreset)),
        "continue-cli" => Ok(Box::new(continue_cli::ContinueCliPreset)),
        "cursor" => Ok(Box::new(cursor::CursorPreset)),
        "github-copilot" => Ok(Box::new(github_copilot::GithubCopilotPreset)),
        "amp" => Ok(Box::new(amp::AmpPreset)),
        "ai_tab" => Ok(Box::new(ai_tab::AiTabPreset)),
        "firebender" => Ok(Box::new(firebender::FirebenderPreset)),
        "agent-v1" => Ok(Box::new(agent_v1::AgentV1Preset)),
        "droid" => Ok(Box::new(droid::DroidPreset)),
        "opencode" => Ok(Box::new(opencode::OpenCodePreset)),
        "pi" => Ok(Box::new(pi::PiPreset)),
        _ => Err(GitAiError::PresetError(format!("Unknown preset: {}", name))),
    }
}
```

- [ ] **Step 2: Register the `presets` submodule in `checkpoint_agent/mod.rs`**

Add `pub mod presets;` to `src/commands/checkpoint_agent/mod.rs`.

- [ ] **Step 3: Run `task build` to verify compilation**

Run: `task build`
Expected: Compiles (submodule files don't exist yet but mod declarations will cause errors — we'll fix in next steps). The types module itself should be well-formed.

Note: This step will initially fail because the child module files don't exist. That's expected — we'll create stub files in Step 4.

- [ ] **Step 4: Create stub files for all preset submodules**

Create empty stub files so the build passes:

```rust
// Each of these files (claude.rs, codex.rs, etc.) starts as:
use super::{AgentPreset, ParsedHookEvent};
use crate::error::GitAiError;

pub struct ClaudePreset;  // (name varies per file)

impl AgentPreset for ClaudePreset {
    fn parse(&self, _hook_input: &str, _trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        Err(GitAiError::PresetError("Not yet implemented".to_string()))
    }
}
```

Create stubs for: `claude.rs`, `codex.rs`, `gemini.rs`, `windsurf.rs`, `continue_cli.rs`, `cursor.rs`, `github_copilot.rs`, `amp.rs`, `droid.rs`, `opencode.rs`, `pi.rs`, `ai_tab.rs`, `firebender.rs`, `agent_v1.rs`.

- [ ] **Step 5: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 6: Commit**

```bash
git add src/commands/checkpoint_agent/presets/
git add src/commands/checkpoint_agent/mod.rs
git commit -m "feat: add core types module for agent presets rewrite"
```

---

### Task 2: Create Parse Helpers Module (`presets/parse.rs`)

**Files:**
- Create: `src/commands/checkpoint_agent/presets/parse.rs`

- [ ] **Step 1: Write tests for parse helpers**

```rust
// At the bottom of src/commands/checkpoint_agent/presets/parse.rs

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_required_str_present() {
        let data = json!({"cwd": "/home/user/project"});
        assert_eq!(required_str(&data, "cwd").unwrap(), "/home/user/project");
    }

    #[test]
    fn test_required_str_missing() {
        let data = json!({"other": "value"});
        assert!(required_str(&data, "cwd").is_err());
    }

    #[test]
    fn test_optional_str_present() {
        let data = json!({"tool_name": "Write"});
        assert_eq!(optional_str(&data, "tool_name"), Some("Write"));
    }

    #[test]
    fn test_optional_str_missing() {
        let data = json!({"other": "value"});
        assert_eq!(optional_str(&data, "tool_name"), None);
    }

    #[test]
    fn test_str_or_default_present() {
        let data = json!({"tool_use_id": "abc123"});
        assert_eq!(str_or_default(&data, "tool_use_id", "bash"), "abc123");
    }

    #[test]
    fn test_str_or_default_missing() {
        let data = json!({"other": "value"});
        assert_eq!(str_or_default(&data, "tool_use_id", "bash"), "bash");
    }

    #[test]
    fn test_required_file_stem() {
        let data = json!({"transcript_path": "/home/user/.claude/projects/abc123.jsonl"});
        assert_eq!(required_file_stem(&data, "transcript_path").unwrap(), "abc123");
    }

    #[test]
    fn test_resolve_absolute_already_absolute() {
        let result = resolve_absolute("/home/user/file.txt", "/some/cwd");
        assert_eq!(result, PathBuf::from("/home/user/file.txt"));
    }

    #[test]
    fn test_resolve_absolute_relative() {
        let result = resolve_absolute("src/main.rs", "/home/user/project");
        assert_eq!(result, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn test_file_paths_from_tool_input_single() {
        let data = json!({
            "tool_input": {"file_path": "src/main.rs"}
        });
        let paths = file_paths_from_tool_input(&data, "/home/user/project");
        assert_eq!(paths, vec![PathBuf::from("/home/user/project/src/main.rs")]);
    }

    #[test]
    fn test_file_paths_from_tool_input_missing() {
        let data = json!({"tool_input": {"command": "ls"}});
        let paths = file_paths_from_tool_input(&data, "/home/user/project");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_optional_str_multi_key() {
        let data = json!({"hookEventName": "PreToolUse"});
        let result = optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        assert_eq!(result, Some("PreToolUse"));
    }

    #[test]
    fn test_optional_str_multi_key_missing() {
        let data = json!({"other": "value"});
        let result = optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_dirty_files_from_hook_data() {
        let data = json!({
            "dirty_files": {
                "/home/user/file.txt": "old content"
            }
        });
        let result = dirty_files_from_value(&data, "/home/user");
        assert!(result.is_some());
        let map = result.unwrap();
        assert_eq!(map.get(&PathBuf::from("/home/user/file.txt")).unwrap(), "old content");
    }
}
```

- [ ] **Step 2: Implement parse helpers**

```rust
// src/commands/checkpoint_agent/presets/parse.rs

use crate::error::GitAiError;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn required_str<'a>(data: &'a Value, key: &str) -> Result<&'a str, GitAiError> {
    data.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| GitAiError::PresetError(format!("{} not found in hook_input", key)))
}

pub fn optional_str<'a>(data: &'a Value, key: &str) -> Option<&'a str> {
    data.get(key).and_then(|v| v.as_str())
}

pub fn optional_str_multi<'a>(data: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| data.get(*key).and_then(|v| v.as_str()))
}

pub fn str_or_default<'a>(data: &'a Value, key: &str, default: &'a str) -> &'a str {
    data.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
}

pub fn required_file_stem(data: &Value, path_key: &str) -> Result<String, GitAiError> {
    let path_str = required_str(data, path_key)?;
    Path::new(path_str)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            GitAiError::PresetError(format!("Could not extract file stem from {}", path_key))
        })
}

pub fn resolve_absolute(path: &str, cwd: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    }
}

pub fn file_paths_from_tool_input(data: &Value, cwd: &str) -> Vec<PathBuf> {
    let tool_input = match data.get("tool_input").or_else(|| data.get("toolInput")) {
        Some(ti) => ti,
        None => return vec![],
    };

    // Try single file_path field
    if let Some(path) = tool_input
        .get("file_path")
        .or_else(|| tool_input.get("filepath"))
        .or_else(|| tool_input.get("path"))
        .and_then(|v| v.as_str())
    {
        if !path.is_empty() {
            return vec![resolve_absolute(path, cwd)];
        }
    }

    // Try array fields
    if let Some(arr) = tool_input
        .get("file_paths")
        .or_else(|| tool_input.get("filepaths"))
        .or_else(|| tool_input.get("files"))
        .and_then(|v| v.as_array())
    {
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

    vec![]
}

pub fn dirty_files_from_value(data: &Value, cwd: &str) -> Option<HashMap<PathBuf, String>> {
    let df = data.get("dirty_files")?;
    let obj = df.as_object()?;
    let mut result = HashMap::new();
    for (key, value) in obj {
        if let Some(content) = value.as_str() {
            let path = resolve_absolute(key, cwd);
            result.insert(path, content.to_string());
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

pub fn string_array(data: &Value, key: &str) -> Option<Vec<String>> {
    let arr = data.get(key)?.as_array()?;
    let strings: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.to_string())
        .collect();
    if strings.is_empty() {
        None
    } else {
        Some(strings)
    }
}

pub fn pathbuf_array(data: &Value, key: &str, cwd: &str) -> Vec<PathBuf> {
    string_array(data, key)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| resolve_absolute(&s, cwd))
        .collect()
}
```

- [ ] **Step 3: Run tests to verify**

Run: `task test TEST_FILTER=parse`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/presets/parse.rs
git commit -m "feat: add parse helpers module for agent presets"
```

---

### Task 3: Create Orchestrator Module

**Files:**
- Create: `src/commands/checkpoint_agent/orchestrator.rs`
- Modify: `src/commands/checkpoint_agent/mod.rs`

- [ ] **Step 1: Write the orchestrator with CheckpointResult type**

```rust
// src/commands/checkpoint_agent/orchestrator.rs

use crate::authorship::working_log::{AgentId, CheckpointKind};
use crate::commands::checkpoint::PreparedPathRole;
use crate::commands::checkpoint_agent::agent_presets::BashPreHookStrategy;
use crate::commands::checkpoint_agent::bash_tool::{self, HookEvent};
use crate::commands::checkpoint_agent::presets::{
    ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit, TranscriptSource,
};
use crate::error::GitAiError;
use crate::git::repository::find_repository_for_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Output of the orchestrator. Replaces AgentRunResult as the serializable type
/// flowing to daemon and checkpoint machinery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointResult {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: AgentId,
    pub repo_working_dir: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub path_role: PreparedPathRole,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
    pub captured_checkpoint_id: Option<String>,
}

/// Generate a random trace ID (t_ + 14 hex chars).
pub fn generate_trace_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 7] = rng.random();
    format!("t_{}", hex::encode(bytes))
}

/// Main entry point: resolve preset, parse hook input, execute events.
pub fn execute_preset_checkpoint(
    preset_name: &str,
    hook_input: &str,
) -> Result<Vec<CheckpointResult>, GitAiError> {
    let trace_id = generate_trace_id();
    let preset = super::presets::resolve_preset(preset_name)?;
    let events = preset.parse(hook_input, &trace_id)?;

    events
        .into_iter()
        .map(|event| execute_event(event))
        .collect()
}

fn resolve_repo_working_dir_from_file_paths(file_paths: &[PathBuf]) -> Result<PathBuf, GitAiError> {
    let first_path = file_paths.first().ok_or_else(|| {
        GitAiError::PresetError("No file paths provided for repo discovery".to_string())
    })?;
    let repo = find_repository_for_file(&first_path.to_string_lossy(), None)?;
    repo.workdir()
        .map(|p| PathBuf::from(p))
        .map_err(|e| GitAiError::Generic(format!("Failed to get workdir: {}", e)))
}

fn resolve_repo_working_dir_from_cwd(cwd: &Path) -> Result<PathBuf, GitAiError> {
    let repo = find_repository_for_file(&cwd.to_string_lossy(), None)?;
    repo.workdir()
        .map(|p| PathBuf::from(p))
        .map_err(|e| GitAiError::Generic(format!("Failed to get workdir: {}", e)))
}

fn execute_event(event: ParsedHookEvent) -> Result<CheckpointResult, GitAiError> {
    match event {
        ParsedHookEvent::PreFileEdit(e) => execute_pre_file_edit(e),
        ParsedHookEvent::PostFileEdit(e) => execute_post_file_edit(e),
        ParsedHookEvent::PreBashCall(e) => execute_pre_bash_call(e),
        ParsedHookEvent::PostBashCall(e) => execute_post_bash_call(e),
    }
}

fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = if !e.file_paths.is_empty() {
        resolve_repo_working_dir_from_file_paths(&e.file_paths)?
    } else {
        resolve_repo_working_dir_from_cwd(&e.context.cwd)?
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths: e.file_paths,
        path_role: PreparedPathRole::WillEdit,
        dirty_files: e.dirty_files,
        transcript_source: None,
        metadata: e.context.metadata,
        captured_checkpoint_id: None,
    })
}

fn execute_post_file_edit(e: PostFileEdit) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = if !e.file_paths.is_empty() {
        resolve_repo_working_dir_from_file_paths(&e.file_paths)?
    } else {
        resolve_repo_working_dir_from_cwd(&e.context.cwd)?
    };

    let checkpoint_kind = if e.context.agent_id.tool == "ai_tab" {
        CheckpointKind::AiTab
    } else {
        CheckpointKind::AiAgent
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths: e.file_paths,
        path_role: PreparedPathRole::Edited,
        dirty_files: e.dirty_files,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
        captured_checkpoint_id: None,
    })
}

fn execute_pre_bash_call(e: PreBashCall) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = resolve_repo_working_dir_from_cwd(&e.context.cwd)?;

    let captured_checkpoint_id = {
        let is_bash = true; // PreBashCall is always a bash tool
        match super::agent_presets::prepare_agent_bash_pre_hook(
            is_bash,
            Some(&e.context.cwd.to_string_lossy()),
            &e.context.session_id,
            &e.tool_use_id,
            &e.context.agent_id,
            Some(&e.context.metadata),
            e.strategy,
        )? {
            super::agent_presets::BashPreHookResult::EmitHumanCheckpoint {
                captured_checkpoint_id,
            } => captured_checkpoint_id,
            super::agent_presets::BashPreHookResult::SkipCheckpoint {
                captured_checkpoint_id,
            } => captured_checkpoint_id,
        }
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths: vec![],
        path_role: PreparedPathRole::WillEdit,
        dirty_files: None,
        transcript_source: None,
        metadata: e.context.metadata,
        captured_checkpoint_id,
    })
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointResult, GitAiError> {
    let repo_working_dir = resolve_repo_working_dir_from_cwd(&e.context.cwd)?;

    let bash_result = bash_tool::handle_bash_tool(
        HookEvent::PostToolUse,
        &e.context.cwd,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let (file_paths, captured_checkpoint_id) = match &bash_result {
        Ok(result) => {
            let paths = match &result.action {
                bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                    paths.iter().map(|p| PathBuf::from(p)).collect()
                }
                bash_tool::BashCheckpointAction::NoChanges => vec![],
                bash_tool::BashCheckpointAction::Fallback => vec![],
                bash_tool::BashCheckpointAction::TakePreSnapshot => vec![],
            };
            let cap_id = result
                .captured_checkpoint
                .as_ref()
                .map(|info| info.capture_id.clone());
            (paths, cap_id)
        }
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            (vec![], None)
        }
    };

    Ok(CheckpointResult {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: e.context.agent_id,
        repo_working_dir,
        file_paths,
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
        captured_checkpoint_id,
    })
}
```

- [ ] **Step 2: Register `orchestrator` module in `checkpoint_agent/mod.rs`**

Add `pub mod orchestrator;` to `src/commands/checkpoint_agent/mod.rs`.

- [ ] **Step 3: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs
git add src/commands/checkpoint_agent/mod.rs
git commit -m "feat: add orchestrator module for agent presets"
```

---

### Task 4: Create Transcript Readers Module

**Files:**
- Create: `src/commands/checkpoint_agent/transcript_readers.rs`
- Modify: `src/commands/checkpoint_agent/mod.rs`

- [ ] **Step 1: Create transcript readers module that re-exports existing reader functions**

Move (copy for now, delete later) all `transcript_and_model_from_*` functions from the existing presets into a single module with a dispatch function. The individual reader functions are unchanged — they just get a unified entry point.

```rust
// src/commands/checkpoint_agent/transcript_readers.rs

use crate::authorship::transcript::AiTranscript;
use crate::commands::checkpoint_agent::presets::{TranscriptFormat, TranscriptSource};
use crate::error::GitAiError;
use std::path::Path;

/// Read a transcript from the given source, returning the parsed transcript
/// and optionally the model name extracted from transcript content.
pub fn read_transcript(source: &TranscriptSource) -> Result<(AiTranscript, Option<String>), GitAiError> {
    match source {
        TranscriptSource::Path { path, format } => read_from_path(path, *format),
        TranscriptSource::Inline(transcript) => Ok((transcript.clone(), None)),
    }
}

fn read_from_path(path: &Path, format: TranscriptFormat) -> Result<(AiTranscript, Option<String>), GitAiError> {
    match format {
        TranscriptFormat::ClaudeJsonl => read_claude_jsonl(path),
        TranscriptFormat::GeminiJson => read_gemini_json(path),
        TranscriptFormat::WindsurfJsonl => read_windsurf_jsonl(path),
        TranscriptFormat::CodexJsonl => read_codex_jsonl(path),
        TranscriptFormat::CursorJsonl => read_cursor_jsonl(path),
        TranscriptFormat::DroidJsonl => read_droid_jsonl(path),
        TranscriptFormat::CopilotSessionJson => read_copilot_session_json(path),
        TranscriptFormat::CopilotEventStreamJsonl => read_copilot_event_stream_jsonl(path),
        TranscriptFormat::AmpThreadJson => read_amp_thread_json(path),
        TranscriptFormat::OpenCodeSqlite => read_opencode_sqlite(path),
        TranscriptFormat::OpenCodeLegacyJson => read_opencode_legacy_json(path),
        TranscriptFormat::PiJsonl => read_pi_jsonl(path),
    }
}

// Each function below is copied from its respective old preset implementation.
// The function bodies are unchanged — only their call site has moved.
```

Then copy each `transcript_and_model_from_*` function body from the existing code into this module, renaming to match the pattern `read_<format>`. The function implementations are NOT shown here because they are 100+ lines each and are being moved verbatim from the existing files:

- `ClaudePreset::transcript_and_model_from_claude_code_jsonl` → `read_claude_jsonl`
- `GeminiPreset::transcript_and_model_from_gemini_json` → `read_gemini_json`
- `WindsurfPreset::transcript_and_model_from_windsurf_jsonl` → `read_windsurf_jsonl`
- `CodexPreset::transcript_and_model_from_codex_rollout_jsonl` → `read_codex_jsonl`
- `CursorPreset::transcript_and_model_from_cursor_jsonl` → `read_cursor_jsonl`
- `DroidPreset::transcript_and_model_from_droid_jsonl` → `read_droid_jsonl`
- `GithubCopilotPreset::transcript_and_model_from_copilot_session_json` → `read_copilot_session_json`
- `GithubCopilotPreset::transcript_and_model_from_copilot_event_stream_jsonl` → `read_copilot_event_stream_jsonl`
- `AmpPreset::transcript_and_model_from_thread_path` → `read_amp_thread_json`
- `OpenCodePreset::transcript_and_model_from_sqlite` → `read_opencode_sqlite`
- `OpenCodePreset::transcript_and_model_from_legacy_storage` → `read_opencode_legacy_json`
- `PiPreset::transcript_and_model_from_pi_session` → `read_pi_jsonl`

- [ ] **Step 2: Register module in `checkpoint_agent/mod.rs`**

Add `pub mod transcript_readers;` to `src/commands/checkpoint_agent/mod.rs`.

- [ ] **Step 3: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation (functions are copied verbatim, imports may need adjustment).

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/transcript_readers.rs
git add src/commands/checkpoint_agent/mod.rs
git commit -m "feat: add transcript readers module with format dispatch"
```

---

### Task 5: Implement Claude Preset (Reference Implementation)

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/claude.rs`

- [ ] **Step 1: Write unit tests for Claude preset parsing**

```rust
// At the bottom of src/commands/checkpoint_agent/presets/claude.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    fn make_claude_hook_input(event: &str, tool: &str) -> String {
        json!({
            "transcript_path": "/home/user/.claude/projects/abc123.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": event,
            "tool_name": tool,
            "session_id": "sess-1",
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string()
    }

    #[test]
    fn test_claude_pre_file_edit() {
        let input = make_claude_hook_input("PreToolUse", "Write");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.context.session_id, "sess-1");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/main.rs")]);
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_claude_post_file_edit() {
        let input = make_claude_hook_input("PostToolUse", "Write");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.file_paths, vec![PathBuf::from("/home/user/project/src/main.rs")]);
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource::Path { format: TranscriptFormat::ClaudeJsonl, .. })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_claude_pre_bash_call() {
        let input = make_claude_hook_input("PreToolUse", "Bash");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_claude_post_bash_call() {
        let input = make_claude_hook_input("PostToolUse", "Bash");
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "claude");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_claude_session_id_from_filename() {
        let input = json!({
            "transcript_path": "/home/user/.claude/projects/cb947e5b-246e-4253-a953-631f7e464c6b.jsonl",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "src/main.rs"}
        }).to_string();
        let events = ClaudePreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.session_id, "cb947e5b-246e-4253-a953-631f7e464c6b");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `task test TEST_FILTER=test_claude_pre_file_edit`
Expected: FAIL (stub implementation returns error).

- [ ] **Step 3: Implement Claude preset parser**

```rust
// src/commands/checkpoint_agent/presets/claude.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ClaudePreset;

impl ClaudePreset {
    fn is_vscode_copilot_hook_payload(data: &serde_json::Value) -> bool {
        // Check if transcript_path looks like a copilot path
        if let Some(path) = parse::optional_str(data, "transcript_path") {
            let lower = path.to_lowercase();
            (lower.contains("github copilot") || lower.contains("github.copilot"))
                && !lower.contains(".claude")
        } else {
            // No transcript_path but has VS Code-specific fields
            data.get("extensionId").is_some()
        }
    }

    fn is_cursor_hook_payload(data: &serde_json::Value) -> bool {
        data.get("cursor_version").is_some()
            || parse::optional_str(data, "transcript_path")
                .map(|p| p.contains(".cursor"))
                .unwrap_or(false)
    }
}

impl AgentPreset for ClaudePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        if Self::is_vscode_copilot_hook_payload(&data) {
            return Err(GitAiError::PresetError(
                "Skipping VS Code hook payload in Claude preset; use github-copilot hooks."
                    .to_string(),
            ));
        }
        if Self::is_cursor_hook_payload(&data) {
            return Err(GitAiError::PresetError(
                "Skipping Cursor hook payload in Claude preset; use cursor hooks.".to_string(),
            ));
        }

        let cwd = parse::required_str(&data, "cwd")?;
        let transcript_path = parse::required_str(&data, "transcript_path")?;

        let session_id = parse::optional_str(&data, "session_id")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                parse::required_file_stem(&data, "transcript_path")
                    .unwrap_or_else(|_| "unknown".to_string())
            });

        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default(&data, "tool_use_id", "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Claude, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "claude".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource::Path {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::ClaudeJsonl,
        });

        let event = match (hook_event, is_bash) {
            (Some("PreToolUse"), true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            (Some("PreToolUse"), false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
            }),
            (_, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            }),
            (_, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
                transcript_source,
            }),
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `task test TEST_FILTER=test_claude`
Expected: All Claude preset tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/presets/claude.rs
git commit -m "feat: implement Claude preset as pure parser"
```

---

### Task 6: Implement Remaining Simple Presets (Gemini, ContinueCli, Codex, Windsurf)

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/gemini.rs`
- Modify: `src/commands/checkpoint_agent/presets/continue_cli.rs`
- Modify: `src/commands/checkpoint_agent/presets/codex.rs`
- Modify: `src/commands/checkpoint_agent/presets/windsurf.rs`

These presets follow the same pattern as Claude with minor field name differences.

- [ ] **Step 1: Implement Gemini preset**

```rust
// src/commands/checkpoint_agent/presets/gemini.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct GeminiPreset;

impl AgentPreset for GeminiPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let transcript_path = parse::required_str(&data, "transcript_path")?;
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default(&data, "tool_use_id", "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Gemini, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "gemini".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource::Path {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::GeminiJson,
        });

        // Gemini uses "BeforeTool" instead of "PreToolUse"
        let is_pre = matches!(hook_event, Some("BeforeTool") | Some("PreToolUse"));

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            (true, false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
            }),
            (false, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            }),
            (false, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
                transcript_source,
            }),
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 2: Implement ContinueCli preset**

```rust
// src/commands/checkpoint_agent/presets/continue_cli.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct ContinueCliPreset;

impl AgentPreset for ContinueCliPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let transcript_path = parse::required_str(&data, "transcript_path")?;
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_use_id = parse::str_or_default(&data, "tool_use_id", "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::ContinueCli, n) == ToolClass::Bash)
            .unwrap_or(false);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "continue-cli".to_string(),
                id: session_id.clone(),
                model: parse::optional_str(&data, "model")
                    .unwrap_or("unknown")
                    .to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]),
        };

        let transcript_source = Some(TranscriptSource::Path {
            path: PathBuf::from(transcript_path),
            format: TranscriptFormat::ClaudeJsonl, // Continue uses Claude-compatible JSONL
        });

        let is_pre = hook_event == Some("PreToolUse");

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::EmitHumanCheckpoint,
            }),
            (true, false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
            }),
            (false, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            }),
            (false, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: parse::file_paths_from_tool_input(&data, cwd),
                dirty_files: None,
                transcript_source,
            }),
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 3: Implement Codex preset**

```rust
// src/commands/checkpoint_agent/presets/codex.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PreBashCall, PresetContext,
    TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CodexPreset;

impl CodexPreset {
    fn session_id_from_hook_data(data: &serde_json::Value) -> Result<String, GitAiError> {
        parse::optional_str_multi(data, &["session_id", "thread_id"])
            .map(|s| s.to_string())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "session_id or thread_id not found in hook_input".to_string(),
                )
            })
    }
}

impl AgentPreset for CodexPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let cwd = parse::required_str(&data, "cwd")?;
        let session_id = Self::session_id_from_hook_data(&data)?;
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let tool_use_id = parse::str_or_default(&data, "tool_use_id", "bash");

        let is_bash = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Codex, n) == ToolClass::Bash)
            .unwrap_or(false);

        // Codex only supports bash tool checkpoints
        if !is_bash {
            return Err(GitAiError::PresetError(format!(
                "Codex preset only supports bash tools, got: {:?}",
                tool_name
            )));
        }

        let transcript_path = parse::optional_str(&data, "transcript_path");

        let mut metadata = HashMap::new();
        if let Some(tp) = transcript_path {
            metadata.insert("transcript_path".to_string(), tp.to_string());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CodexJsonl,
        });

        let event = if hook_event == Some("PreToolUse") {
            // Codex uses SnapshotOnly for pre-hook (no Human checkpoint emitted)
            ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                strategy: BashPreHookStrategy::SnapshotOnly,
            })
        } else {
            ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            })
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 4: Implement Windsurf preset**

```rust
// src/commands/checkpoint_agent/presets/windsurf.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct WindsurfPreset;

impl AgentPreset for WindsurfPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let trajectory_id = parse::required_str(&data, "trajectory_id")?.to_string();
        let agent_action = parse::optional_str(&data, "agent_action_name");

        // cwd can be at top level or nested in tool_info
        let tool_info = data.get("tool_info");
        let cwd = tool_info
            .and_then(|ti| ti.get("cwd"))
            .and_then(|v| v.as_str())
            .or_else(|| parse::optional_str(&data, "cwd"))
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        let model = parse::optional_str(&data, "model_name")
            .unwrap_or("unknown")
            .to_string();

        // Transcript path: from tool_info or derived from trajectory_id
        let transcript_path = tool_info
            .and_then(|ti| ti.get("transcript_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
                format!(
                    "{}/.windsurf/transcripts/{}.jsonl",
                    home.display(),
                    trajectory_id
                )
            });

        let context = PresetContext {
            agent_id: AgentId {
                tool: "windsurf".to_string(),
                id: trajectory_id.clone(),
                model,
            },
            session_id: trajectory_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("transcript_path".to_string(), transcript_path.clone())]),
        };

        let transcript_source = Some(TranscriptSource::Path {
            path: PathBuf::from(&transcript_path),
            format: TranscriptFormat::WindsurfJsonl,
        });

        // Windsurf uses "pre_run_command" / "post_run_command" for bash
        let is_bash = matches!(agent_action, Some("pre_run_command") | Some("post_run_command"));
        let is_pre = matches!(agent_action, Some("pre_run_command"));

        let event = if is_bash {
            if is_pre {
                ParsedHookEvent::PreBashCall(PreBashCall {
                    context,
                    tool_use_id: "bash".to_string(),
                    strategy: BashPreHookStrategy::EmitHumanCheckpoint,
                })
            } else {
                ParsedHookEvent::PostBashCall(PostBashCall {
                    context,
                    tool_use_id: "bash".to_string(),
                    transcript_source,
                })
            }
        } else {
            // File edit tools
            let file_path = tool_info
                .and_then(|ti| ti.get("file_path"))
                .and_then(|v| v.as_str())
                .map(|p| vec![parse::resolve_absolute(p, cwd)])
                .unwrap_or_default();

            if is_pre || agent_action.is_none() {
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context,
                    file_paths: file_path,
                    dirty_files: None,
                    transcript_source,
                })
            } else {
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context,
                    file_paths: file_path,
                    dirty_files: None,
                    transcript_source,
                })
            }
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 5: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 6: Commit**

```bash
git add src/commands/checkpoint_agent/presets/gemini.rs
git add src/commands/checkpoint_agent/presets/continue_cli.rs
git add src/commands/checkpoint_agent/presets/codex.rs
git add src/commands/checkpoint_agent/presets/windsurf.rs
git commit -m "feat: implement Gemini, ContinueCli, Codex, and Windsurf presets"
```

---

### Task 7: Implement Complex Presets (Cursor, GithubCopilot, Droid)

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/cursor.rs`
- Modify: `src/commands/checkpoint_agent/presets/github_copilot.rs`
- Modify: `src/commands/checkpoint_agent/presets/droid.rs`

- [ ] **Step 1: Implement Cursor preset**

Key differences: uses `workspace_roots` array, only checkpoints file-mutating tools (Write, Delete, StrReplace), `conversation_id` for session.

```rust
// src/commands/checkpoint_agent/presets/cursor.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CursorPreset;

impl CursorPreset {
    const FILE_EDIT_TOOLS: &'static [&'static str] = &["Write", "Delete", "StrReplace"];
}

impl AgentPreset for CursorPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Skip legacy events
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        if matches!(hook_event, Some("beforeSubmitPrompt") | Some("afterFileEdit")) {
            return Err(GitAiError::PresetError(
                "Legacy Cursor event, skipping".to_string(),
            ));
        }

        let conversation_id = parse::required_str(&data, "conversation_id")?.to_string();
        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);

        // Only checkpoint file-mutating tools
        let is_file_edit_tool = tool_name
            .map(|n| Self::FILE_EDIT_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(n)))
            .unwrap_or(false);

        if !is_file_edit_tool {
            return Err(GitAiError::PresetError(format!(
                "Cursor preset only checkpoints file-edit tools, got: {:?}. Skipping.",
                tool_name
            )));
        }

        let workspace_roots: Vec<String> = data
            .get("workspace_roots")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let cwd = workspace_roots
            .first()
            .map(|s| s.as_str())
            .unwrap_or(".");

        let model = parse::optional_str(&data, "model")
            .unwrap_or("unknown")
            .to_string();
        let transcript_path = parse::optional_str(&data, "transcript_path");

        let mut metadata = HashMap::new();
        if let Some(tp) = transcript_path {
            metadata.insert("transcript_path".to_string(), tp.to_string());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: conversation_id.clone(),
                model,
            },
            session_id: conversation_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        let file_paths = parse::file_paths_from_tool_input(&data, cwd);

        let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CursorJsonl,
        });

        let is_pre = matches!(hook_event, Some("preToolUse") | Some("PreToolUse"));

        let event = if is_pre {
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
            })
        } else {
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: None,
                transcript_source,
            })
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 2: Implement GithubCopilot preset**

Key differences: Two dispatch paths (legacy extension vs VS Code native), `create_file` special case, `dirty_files` extraction, `SnapshotOnly` for bash.

```rust
// src/commands/checkpoint_agent/presets/github_copilot.rs

use super::parse;
use super::{
    AgentPreset, BashPreHookStrategy, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall,
    PreFileEdit, PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct GithubCopilotPreset;

impl GithubCopilotPreset {
    pub fn transcript_path_from_hook_data(data: &serde_json::Value) -> Option<&str> {
        parse::optional_str_multi(data, &["chat_session_path", "chatSessionPath"])
    }

    pub fn looks_like_claude_transcript_path(path: &str) -> bool {
        path.contains(".claude")
    }

    fn is_legacy_extension_payload(data: &serde_json::Value) -> bool {
        let event = parse::optional_str(data, "hook_event_name");
        matches!(event, Some("before_edit") | Some("after_edit"))
    }
}

impl AgentPreset for GithubCopilotPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        if Self::is_legacy_extension_payload(&data) {
            return self.parse_legacy(&data, trace_id);
        }

        self.parse_vscode_native(&data, trace_id)
    }
}

impl GithubCopilotPreset {
    fn parse_legacy(
        &self,
        data: &serde_json::Value,
        trace_id: &str,
    ) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_event = parse::required_str(data, "hook_event_name")?;
        let cwd = parse::optional_str_multi(data, &["workspace_folder", "workspaceFolder"])
            .ok_or_else(|| {
                GitAiError::PresetError("workspace_folder not found in hook_input".to_string())
            })?;

        let session_id = parse::optional_str_multi(
            data,
            &["chat_session_id", "session_id", "chatSessionId", "sessionId"],
        )
        .unwrap_or("unknown")
        .to_string();

        let transcript_path = Self::transcript_path_from_hook_data(data);

        let mut metadata = HashMap::new();
        if let Some(tp) = transcript_path {
            metadata.insert("chat_session_path".to_string(), tp.to_string());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "github-copilot".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        let event = if hook_event == "before_edit" {
            let file_paths = parse::pathbuf_array(data, "will_edit_filepaths", cwd);
            if file_paths.is_empty() {
                return Err(GitAiError::PresetError(
                    "No will_edit_filepaths in before_edit event".to_string(),
                ));
            }
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: parse::dirty_files_from_value(data, cwd),
            })
        } else {
            // after_edit
            let file_paths = parse::pathbuf_array(data, "edited_filepaths", cwd);
            let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
                path: PathBuf::from(tp),
                format: TranscriptFormat::CopilotSessionJson,
            });
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: parse::dirty_files_from_value(data, cwd),
                transcript_source,
            })
        };

        Ok(vec![event])
    }

    fn parse_vscode_native(
        &self,
        data: &serde_json::Value,
        trace_id: &str,
    ) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_event = parse::optional_str_multi(data, &["hook_event_name", "hookEventName"])
            .unwrap_or("PostToolUse");
        let cwd = parse::optional_str_multi(data, &["cwd", "workspace_folder", "workspaceFolder"])
            .ok_or_else(|| {
                GitAiError::PresetError("cwd not found in hook_input".to_string())
            })?;

        let session_id = parse::optional_str_multi(
            data,
            &["chat_session_id", "session_id", "chatSessionId", "sessionId"],
        )
        .unwrap_or("unknown")
        .to_string();

        let tool_name = parse::optional_str_multi(data, &["tool_name", "toolName"])
            .unwrap_or("");
        let tool_use_id = parse::str_or_default(data, "tool_use_id", "tool");

        let is_bash = bash_tool::classify_tool(Agent::GithubCopilot, tool_name) == ToolClass::Bash;

        let transcript_path = Self::transcript_path_from_hook_data(data);
        let mut metadata = HashMap::new();
        if let Some(tp) = transcript_path {
            metadata.insert("chat_session_path".to_string(), tp.to_string());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "github-copilot".to_string(),
                id: session_id.clone(),
                model: "unknown".to_string(),
            },
            session_id: session_id.clone(),
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        let file_paths = parse::file_paths_from_tool_input(data, cwd);
        let dirty_files = parse::dirty_files_from_value(data, cwd);

        if hook_event == "PreToolUse" {
            if is_bash {
                return Ok(vec![ParsedHookEvent::PreBashCall(PreBashCall {
                    context,
                    tool_use_id: tool_use_id.to_string(),
                    strategy: BashPreHookStrategy::SnapshotOnly,
                })]);
            }

            // create_file: synthesize empty dirty_files
            if tool_name.eq_ignore_ascii_case("create_file") {
                if file_paths.is_empty() {
                    return Err(GitAiError::PresetError(
                        "No file path found in create_file PreToolUse tool_input".to_string(),
                    ));
                }
                let empty_dirty = Some(
                    file_paths.iter().map(|p| (p.clone(), String::new())).collect(),
                );
                return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context,
                    file_paths,
                    dirty_files: empty_dirty,
                })]);
            }

            if file_paths.is_empty() {
                return Err(GitAiError::PresetError(format!(
                    "No editable file paths found in VS Code hook input (tool_name: {}). Skipping.",
                    tool_name
                )));
            }

            return Ok(vec![ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files,
            })]);
        }

        // PostToolUse
        if is_bash {
            let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
                path: PathBuf::from(tp),
                format: TranscriptFormat::CopilotSessionJson,
            });
            return Ok(vec![ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id.to_string(),
                transcript_source,
            })]);
        }

        let transcript_source = transcript_path.map(|tp| TranscriptSource::Path {
            path: PathBuf::from(tp),
            format: TranscriptFormat::CopilotSessionJson,
        });

        Ok(vec![ParsedHookEvent::PostFileEdit(PostFileEdit {
            context,
            file_paths,
            dirty_files,
            transcript_source,
        })])
    }
}
```

- [ ] **Step 3: Implement Droid preset**

Key differences: ApplyPatch parsing, multiple field name variants (camelCase), transcript path discovery from cwd.

```rust
// src/commands/checkpoint_agent/presets/droid.rs
// (Implementation follows same pattern - extract fields, classify tool, route to variant)
// Key specifics:
// - session_id: session_id || sessionId || "droid-{timestamp}"
// - is_bash: all tools are bash in Droid
// - transcript_path: transcript_path || transcriptPath || "{cwd}/.droid/{session_id}.jsonl"
// - Special: ApplyPatch tool parses "*** Update File:" lines from tool_input
// - format: TranscriptFormat::DroidJsonl
```

The Droid implementation should follow the same structure as Claude/Gemini, with:
- `session_id` fallback chain
- All tools treated as bash
- ApplyPatch file path extraction via regex on tool_input text

- [ ] **Step 4: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/presets/cursor.rs
git add src/commands/checkpoint_agent/presets/github_copilot.rs
git add src/commands/checkpoint_agent/presets/droid.rs
git commit -m "feat: implement Cursor, GithubCopilot, and Droid presets"
```

---

### Task 8: Implement Remaining Presets (Amp, OpenCode, Pi, AiTab, Firebender, AgentV1)

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/amp.rs`
- Modify: `src/commands/checkpoint_agent/presets/opencode.rs`
- Modify: `src/commands/checkpoint_agent/presets/pi.rs`
- Modify: `src/commands/checkpoint_agent/presets/ai_tab.rs`
- Modify: `src/commands/checkpoint_agent/presets/firebender.rs`
- Modify: `src/commands/checkpoint_agent/presets/agent_v1.rs`

- [ ] **Step 1: Implement AiTab preset (simplest)**

```rust
// src/commands/checkpoint_agent/presets/ai_tab.rs

use super::parse;
use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext};
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AiTabPreset;

impl AgentPreset for AiTabPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let hook_event = parse::required_str(&data, "hook_event_name")?;
        let tool = parse::required_str(&data, "tool")?.to_string();
        let model = parse::required_str(&data, "model")?.to_string();

        let cwd = parse::optional_str(&data, "repo_working_dir").unwrap_or(".");

        let completion_id = parse::optional_str(&data, "completion_id")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                format!("{}", ts)
            });
        let session_id = format!("ai_tab-{}", completion_id);

        let context = PresetContext {
            agent_id: AgentId {
                tool: "ai_tab".to_string(),
                id: session_id.clone(),
                model,
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::from([("tool".to_string(), tool)]),
        };

        let dirty_files = parse::dirty_files_from_value(&data, cwd);

        let event = if hook_event == "before_edit" {
            let file_paths = parse::pathbuf_array(&data, "will_edit_filepaths", cwd);
            ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files,
            })
        } else {
            // after_edit → AiTab checkpoint kind (handled by orchestrator via agent_id.tool)
            let file_paths = parse::pathbuf_array(&data, "edited_filepaths", cwd);
            ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files,
                transcript_source: None,
            })
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 2: Implement AgentV1 preset**

```rust
// src/commands/checkpoint_agent/presets/agent_v1.rs

use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext, TranscriptSource};
use crate::authorship::transcript::AiTranscript;
use crate::authorship::working_log::AgentId;
use crate::error::GitAiError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct AgentV1Preset;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentV1Payload {
    Human {
        repo_working_dir: String,
        will_edit_filepaths: Option<Vec<String>>,
        dirty_files: Option<HashMap<String, String>>,
    },
    AiAgent {
        repo_working_dir: String,
        edited_filepaths: Option<Vec<String>>,
        transcript: AiTranscript,
        agent_name: String,
        model: String,
        conversation_id: String,
        dirty_files: Option<HashMap<String, String>>,
    },
}

impl AgentPreset for AgentV1Preset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let payload: AgentV1Payload = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid agent-v1 JSON: {}", e)))?;

        let event = match payload {
            AgentV1Payload::Human {
                repo_working_dir,
                will_edit_filepaths,
                dirty_files,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = will_edit_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
                let dirty = dirty_files.map(|df| {
                    df.into_iter().map(|(k, v)| (PathBuf::from(k), v)).collect()
                });
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: "human".to_string(),
                            id: "human".to_string(),
                            model: "human".to_string(),
                        },
                        session_id: "human".to_string(),
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    dirty_files: dirty,
                })
            }
            AgentV1Payload::AiAgent {
                repo_working_dir,
                edited_filepaths,
                transcript,
                agent_name,
                model,
                conversation_id,
                dirty_files,
            } => {
                let cwd = PathBuf::from(&repo_working_dir);
                let file_paths = edited_filepaths
                    .unwrap_or_default()
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
                let dirty = dirty_files.map(|df| {
                    df.into_iter().map(|(k, v)| (PathBuf::from(k), v)).collect()
                });
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context: PresetContext {
                        agent_id: AgentId {
                            tool: agent_name,
                            id: conversation_id.clone(),
                            model,
                        },
                        session_id: conversation_id,
                        trace_id: trace_id.to_string(),
                        cwd,
                        metadata: HashMap::new(),
                    },
                    file_paths,
                    dirty_files: dirty,
                    transcript_source: Some(TranscriptSource::Inline(transcript)),
                })
            }
        };

        Ok(vec![event])
    }
}
```

- [ ] **Step 3: Implement Firebender preset**

Key: Parses patch text for file paths, uses workspace_roots, no transcript, bash tool support.

```rust
// src/commands/checkpoint_agent/presets/firebender.rs
// Pattern: extract hook_event_name, model, workspace_roots, tool_name
// Bash classification via bash_tool::classify_tool(Agent::Firebender, name)
// File path extraction from: file_path, target_file, relative_workspace_path fields,
// plus patch text parsing for "*** Update File:", "*** Add File:" lines
// No transcript
// dirty_files from hook data
```

- [ ] **Step 4: Implement Amp preset**

Key: Thread path discovery (env var, platform-specific paths), `tool_use_id` in metadata.

```rust
// src/commands/checkpoint_agent/presets/amp.rs
// Pattern: extract hook_event_name, tool_use_id, thread_id, cwd
// Bash classification via bash_tool::classify_tool(Agent::Amp, name)
// transcript_path discovery: field → env var → platform path → search by tool_use_id
// format: TranscriptFormat::AmpThreadJson
// Stores tool_use_id and thread_id in metadata
```

- [ ] **Step 5: Implement OpenCode preset**

Key: Dual storage backends (SQLite + legacy JSON), env var override for storage path.

```rust
// src/commands/checkpoint_agent/presets/opencode.rs
// Pattern: extract hook_event_name, session_id, cwd, tool_name, tool_use_id
// Bash classification via bash_tool::classify_tool(Agent::OpenCode, name)
// Transcript path: env var || platform-specific opencode data path
// Tries SQLite first, falls back to legacy JSON
// format: TranscriptFormat::OpenCodeSqlite (or OpenCodeLegacyJson)
// File paths extracted from tool_input with multiple key variants
```

- [ ] **Step 6: Implement Pi preset**

Key: Four event types (before_edit, after_edit, before_command, after_command), strips model prefix.

```rust
// src/commands/checkpoint_agent/presets/pi.rs
// Pattern: Four events map directly:
//   before_edit → PreFileEdit (with will_edit_filepaths)
//   after_edit → PostFileEdit (with edited_filepaths, transcript)
//   before_command → PreBashCall
//   after_command → PostBashCall
// session_id: required field
// model: strips provider prefix ("anthropic/claude-opus" → "claude-opus")
// transcript: session_path field, format PiJsonl
// dirty_files from hook data
```

- [ ] **Step 7: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 8: Commit**

```bash
git add src/commands/checkpoint_agent/presets/
git commit -m "feat: implement all remaining preset parsers"
```

---

### Task 9: Wire Up Dispatch — Replace Old Preset Calls

**Files:**
- Modify: `src/commands/git_ai_handlers.rs`

- [ ] **Step 1: Replace the 14-arm preset dispatch with orchestrator call**

In `src/commands/git_ai_handlers.rs`, replace the match block at lines ~414-634 (where each preset is individually matched and `.run()` is called) with:

```rust
// Replace the entire match block for agent presets with:
match preset_name {
    "mock_ai" | "mock_known_human" | "human" => {
        // These test mock presets stay as-is (they don't go through AgentPreset)
        // ... existing mock handling code unchanged ...
    }
    _ => {
        match crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint(
            preset_name,
            hook_input.as_deref().unwrap_or(""),
        ) {
            Ok(results) => {
                if let Some(first) = results.first() {
                    repository_working_dir =
                        first.repo_working_dir.to_string_lossy().to_string();
                }
                // Convert CheckpointResult to AgentRunResult for downstream compatibility
                // (temporary bridge until Task 10 removes AgentRunResult)
                if let Some(result) = results.into_iter().next() {
                    agent_run_result = Some(result.into_agent_run_result());
                }
            }
            Err(e) => {
                eprintln!("{} preset error: {}", preset_name, e);
                std::process::exit(0);
            }
        }
    }
}
```

- [ ] **Step 2: Add `into_agent_run_result()` bridge method on CheckpointResult**

In `src/commands/checkpoint_agent/orchestrator.rs`, add:

```rust
impl CheckpointResult {
    /// Temporary bridge: convert to AgentRunResult for downstream code that hasn't
    /// been updated yet. This will be removed once checkpoint.rs and daemon.rs
    /// consume CheckpointResult directly.
    pub fn into_agent_run_result(self) -> crate::commands::checkpoint_agent::agent_presets::AgentRunResult {
        use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;

        let (edited_filepaths, will_edit_filepaths) = match self.path_role {
            PreparedPathRole::Edited => (
                Some(self.file_paths.iter().map(|p| p.to_string_lossy().to_string()).collect()),
                None,
            ),
            PreparedPathRole::WillEdit => (
                None,
                Some(self.file_paths.iter().map(|p| p.to_string_lossy().to_string()).collect()),
            ),
        };

        let dirty_files = self.dirty_files.map(|df| {
            df.into_iter()
                .map(|(k, v)| (k.to_string_lossy().to_string(), v))
                .collect()
        });

        // Resolve transcript eagerly for the bridge (downstream expects Option<AiTranscript>)
        let transcript = self.transcript_source.as_ref().and_then(|src| {
            match crate::commands::checkpoint_agent::transcript_readers::read_transcript(src) {
                Ok((transcript, _model)) => Some(transcript),
                Err(e) => {
                    eprintln!("[Warning] Failed to read transcript: {}", e);
                    None
                }
            }
        });

        AgentRunResult {
            agent_id: self.agent_id,
            agent_metadata: Some(self.metadata),
            checkpoint_kind: self.checkpoint_kind,
            transcript,
            repo_working_dir: Some(self.repo_working_dir.to_string_lossy().to_string()),
            edited_filepaths,
            will_edit_filepaths,
            dirty_files,
            captured_checkpoint_id: self.captured_checkpoint_id,
        }
    }
}
```

- [ ] **Step 3: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 4: Run the full test suite**

Run: `task test`
Expected: All existing tests pass (behavior unchanged, only internal routing changed).

- [ ] **Step 5: Commit**

```bash
git add src/commands/git_ai_handlers.rs
git add src/commands/checkpoint_agent/orchestrator.rs
git commit -m "feat: wire up new preset dispatch via orchestrator"
```

---

### Task 10: Update Daemon and Checkpoint to Use CheckpointResult Directly

**Files:**
- Modify: `src/daemon/control_api.rs`
- Modify: `src/daemon.rs`
- Modify: `src/commands/checkpoint.rs`

- [ ] **Step 1: Update `LiveCheckpointRunRequest` to use `CheckpointResult`**

In `src/daemon/control_api.rs`, replace the `agent_run_result` field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveCheckpointRunRequest {
    #[serde(default)]
    pub repo_working_dir: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default)]
    pub is_pre_commit: Option<bool>,
    #[serde(default)]
    pub checkpoint_result: Option<CheckpointResult>,  // was agent_run_result
}
```

- [ ] **Step 2: Update `checkpoint::run()` signature to accept `CheckpointResult`**

Change the function signature and all internal references:

```rust
pub fn run(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_result: Option<CheckpointResult>,
    is_pre_commit: bool,
) -> Result<(usize, usize, usize), GitAiError> {
```

Update internal field access patterns:
- `result.edited_filepaths` / `result.will_edit_filepaths` → `result.file_paths` + `result.path_role`
- `result.dirty_files` (HashMap<String, String>) → `result.dirty_files` (HashMap<PathBuf, String>)
- `result.transcript` → resolve via `transcript_readers::read_transcript(&result.transcript_source)`
- `result.agent_metadata` → `result.metadata`
- `result.repo_working_dir` → `result.repo_working_dir` (PathBuf, always present)

- [ ] **Step 3: Update `explicit_capture_target_paths()` for CheckpointResult**

```rust
pub fn explicit_capture_target_paths(
    kind: CheckpointKind,
    checkpoint_result: Option<&CheckpointResult>,
) -> Option<(PreparedPathRole, Vec<String>)> {
    let result = checkpoint_result?;
    if result.file_paths.is_empty() {
        return None;
    }
    let paths: Vec<String> = result.file_paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| !p.trim().is_empty())
        .collect();
    if paths.is_empty() {
        None
    } else {
        Some((result.path_role.clone(), paths))
    }
}
```

- [ ] **Step 4: Update daemon's `build_human_replay_agent_result` to build CheckpointResult**

In `src/daemon.rs`, rename to `build_human_replay_checkpoint_result`:

```rust
fn build_human_replay_checkpoint_result(
    files: Vec<String>,
    dirty_files: HashMap<String, String>,
    repo_working_dir: &str,
) -> CheckpointResult {
    CheckpointResult {
        trace_id: crate::commands::checkpoint_agent::orchestrator::generate_trace_id(),
        checkpoint_kind: CheckpointKind::Human,
        agent_id: AgentId {
            tool: "daemon".to_string(),
            id: "daemon-commit-replay".to_string(),
            model: "daemon".to_string(),
        },
        repo_working_dir: PathBuf::from(repo_working_dir),
        file_paths: files.into_iter().map(PathBuf::from).collect(),
        path_role: PreparedPathRole::WillEdit,
        dirty_files: Some(dirty_files.into_iter().map(|(k, v)| (PathBuf::from(k), v)).collect()),
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    }
}
```

- [ ] **Step 5: Update all callers of checkpoint::run() and LiveCheckpointRunRequest**

Search for all references and update. Key call sites:
- `git_ai_handlers.rs`: `run_checkpoint_via_daemon_or_local()` passes `CheckpointResult` instead of `AgentRunResult`
- `daemon.rs`: `apply_checkpoint_side_effect()` extracts `checkpoint_result` from request
- `daemon.rs`: bash-in-flight commit handling constructs `CheckpointResult` directly

- [ ] **Step 6: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 7: Run full test suite**

Run: `task test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/daemon/control_api.rs src/daemon.rs src/commands/checkpoint.rs src/commands/git_ai_handlers.rs
git commit -m "feat: replace AgentRunResult with CheckpointResult in checkpoint and daemon"
```

---

### Task 11: Delete Old Preset Code

**Files:**
- Delete contents of: `src/commands/checkpoint_agent/agent_presets.rs` (keep only shared types still needed)
- Delete: `src/commands/checkpoint_agent/amp_preset.rs`
- Delete: `src/commands/checkpoint_agent/opencode_preset.rs`
- Delete: `src/commands/checkpoint_agent/pi_preset.rs`
- Delete: `src/commands/checkpoint_agent/agent_v1_preset.rs`
- Modify: `src/commands/checkpoint_agent/mod.rs`

- [ ] **Step 1: Determine what to keep from `agent_presets.rs`**

The file currently contains:
- `AgentCheckpointFlags` — can be deleted (replaced by raw `&str` hook_input)
- `AgentRunResult` — can be deleted (replaced by `CheckpointResult`)
- `BashPreHookStrategy` — KEEP (used by orchestrator and bash_tool)
- `BashPreHookResult` — KEEP (used by orchestrator)
- `prepare_agent_bash_pre_hook` — KEEP (called by orchestrator)
- `AgentCheckpointPreset` trait — DELETE
- All `impl AgentCheckpointPreset for XPreset` — DELETE
- All preset struct definitions — DELETE (moved to presets/)
- All `transcript_and_model_from_*` functions — DELETE (moved to transcript_readers.rs)
- All helper methods (is_vscode_copilot_hook_payload, etc.) — DELETE (moved to presets/)

- [ ] **Step 2: Gut `agent_presets.rs` — keep only shared bash pre-hook infrastructure**

```rust
// src/commands/checkpoint_agent/agent_presets.rs
// Only keep:

use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool;
use crate::error::GitAiError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BashPreHookStrategy {
    EmitHumanCheckpoint,
    SnapshotOnly,
}

pub enum BashPreHookResult {
    EmitHumanCheckpoint {
        captured_checkpoint_id: Option<String>,
    },
    SkipCheckpoint {
        captured_checkpoint_id: Option<String>,
    },
}

impl BashPreHookResult {
    pub fn captured_checkpoint_id(self) -> Option<String> {
        match self {
            Self::EmitHumanCheckpoint { captured_checkpoint_id }
            | Self::SkipCheckpoint { captured_checkpoint_id } => captured_checkpoint_id,
        }
    }
}

pub fn prepare_agent_bash_pre_hook(
    is_bash_tool: bool,
    repo_working_dir: Option<&str>,
    session_id: &str,
    tool_use_id: &str,
    agent_id: &AgentId,
    agent_metadata: Option<&HashMap<String, String>>,
    strategy: BashPreHookStrategy,
) -> Result<BashPreHookResult, GitAiError> {
    // ... existing implementation unchanged ...
}
```

- [ ] **Step 3: Delete old preset files**

```bash
rm src/commands/checkpoint_agent/amp_preset.rs
rm src/commands/checkpoint_agent/opencode_preset.rs
rm src/commands/checkpoint_agent/pi_preset.rs
rm src/commands/checkpoint_agent/agent_v1_preset.rs
```

- [ ] **Step 4: Update `mod.rs` to remove deleted modules**

```rust
// src/commands/checkpoint_agent/mod.rs
pub mod agent_presets;  // now only contains bash pre-hook infrastructure
pub mod bash_tool;
pub mod orchestrator;
pub mod presets;
pub mod transcript_readers;
```

- [ ] **Step 5: Remove all remaining references to deleted types**

Search for any remaining `AgentRunResult`, `AgentCheckpointFlags`, `AgentCheckpointPreset` references and remove or update them. Key places:
- `src/commands/git_ai_handlers.rs` imports
- `src/daemon.rs` imports
- `src/git/test_utils/mod.rs` (test helpers)
- `src/authorship/pre_commit.rs`

- [ ] **Step 6: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 7: Run full test suite**

Run: `task test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: delete old agent preset code (~5000 lines removed)"
```

---

### Task 12: Update Test Infrastructure

**Files:**
- Modify: `src/git/test_utils/mod.rs`

- [ ] **Step 1: Update test helpers that construct AgentRunResult**

The test utilities at `src/git/test_utils/mod.rs` construct `AgentRunResult` for mock checkpoints. Update these to construct `CheckpointResult` instead:

- `build_scoped_human_agent_run_result` → `build_scoped_human_checkpoint_result`
- Test helper at line ~1593 that builds mock results

```rust
fn build_scoped_human_checkpoint_result(&self) -> Result<Option<CheckpointResult>, GitAiError> {
    // ... translate existing logic to use CheckpointResult fields
}
```

- [ ] **Step 2: Run full test suite**

Run: `task test`
Expected: All tests pass including integration tests.

- [ ] **Step 3: Commit**

```bash
git add src/git/test_utils/mod.rs
git commit -m "test: update test helpers to use CheckpointResult"
```

---

### Task 13: Final Verification and Lint

**Files:** (none new)

- [ ] **Step 1: Run the full test suite in both modes**

Run: `task test`
Run: `task test:wrapper-daemon`
Expected: All tests pass in both modes.

- [ ] **Step 2: Run lint and format**

Run: `task lint`
Run: `task format`
Expected: No warnings or errors.

- [ ] **Step 3: Verify integration tests exercise the new code path**

Run: `task test TEST_FILTER=checkpoint`
Expected: All checkpoint integration tests pass (they invoke via CLI → new dispatch → orchestrator → checkpoint machinery).

- [ ] **Step 4: Run `cargo insta review` if any snapshots changed**

Run: `cargo insta review`
Expected: Review and accept any snapshot changes that reflect the refactoring (e.g., different field names in debug output).

- [ ] **Step 5: Commit any snapshot or format changes**

```bash
git add -A
git commit -m "chore: update snapshots and formatting for presets rewrite"
```

---

## Summary

| Task | Description | Estimated Lines |
|------|-------------|----------------|
| 1 | Core types module | ~150 |
| 2 | Parse helpers | ~150 |
| 3 | Orchestrator | ~200 |
| 4 | Transcript readers (moved) | ~800 (moved, not new) |
| 5 | Claude preset (reference) | ~100 |
| 6 | Simple presets (4) | ~300 |
| 7 | Complex presets (3) | ~350 |
| 8 | Remaining presets (6) | ~400 |
| 9 | Wire up dispatch | ~50 |
| 10 | Update daemon/checkpoint | ~200 |
| 11 | Delete old code | -5000 |
| 12 | Update test infra | ~50 |
| 13 | Final verification | 0 |

**Net result:** ~1700 lines of clean new code, ~5000+ lines deleted.
