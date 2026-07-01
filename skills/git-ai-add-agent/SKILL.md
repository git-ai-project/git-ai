---
name: git-ai-add-agent
description: "Add support for a new AI coding agent to git-ai. Use when you need to integrate a new agent (like a new IDE, coding assistant, or AI tool) so that git-ai can track its file edits and attribute them correctly."
argument-hint: "[name of the new AI agent to add support for]"
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
---

# Adding a New AI Agent Preset to git-ai

This guide covers all the code changes needed to integrate a new AI coding agent.

## Architecture Overview

Each agent integration has two layers:
1. **AgentCheckpointPreset** (`src/commands/checkpoint_agent/`) — parses the hook input JSON the agent sends and returns the edited file paths, transcript, and model info
2. **HookInstaller** (`src/mdm/agents/`) — detects whether the agent is installed, reads/writes its config file to install/uninstall git-ai hooks

## Step 1: Create the Preset

Create `src/commands/checkpoint_agent/<agent>_preset.rs` (for complex agents) or add to `agent_presets.rs` (for simpler ones).

### Understand the Agent's Hook Input Format

Git-ai receives JSON via stdin when the agent fires its hook. The shape varies per agent. Study `src/commands/checkpoint_agent/amp_preset.rs` and `src/commands/checkpoint_agent/pi_preset.rs` for reference.

**Common patterns:**

```rust
#[derive(Deserialize)]
struct MyAgentHookInput {
    hook_event_name: String,   // "PreToolUse" | "PostToolUse" (or agent-specific names)
    session_id: String,
    cwd: String,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<serde_json::Value>,
    // agent-specific fields...
}
```

### Implement the Preset

```rust
pub struct MyAgentPreset;

impl AgentCheckpointPreset for MyAgentPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let input: MyAgentHookInput = serde_json::from_str(
            flags.hook_input.as_deref().unwrap_or("")
        ).map_err(|e| GitAiError::PresetError(format!("Failed to parse hook input: {}", e)))?;

        let is_pre_edit = input.hook_event_name == "PreToolUse";
        let is_post_edit = input.hook_event_name == "PostToolUse";

        // Extract file paths from the tool_input
        let file_paths = extract_file_paths_from_tool_input(&input.tool_input);

        if is_pre_edit {
            // Check if this is a bash/shell tool (use bash_tool fast path)
            if is_bash_tool(&input.tool_name) {
                let strategy = prepare_agent_bash_pre_hook(
                    &input.session_id,
                    input.tool_use_id.as_deref(),
                    SupportedAgent::MyAgent,
                    &input.tool_name.as_deref().unwrap_or(""),
                    &flags,
                )?;
                match strategy {
                    BashPreHookStrategy::EmitHumanCheckpoint => {
                        return Ok(AgentRunResult {
                            agent_id: make_agent_id("my_agent", &input.session_id, ""),
                            checkpoint_kind: CheckpointKind::Human,
                            ..Default::default()
                        });
                    }
                    BashPreHookStrategy::SkipCheckpoint => {
                        return Err(GitAiError::PresetError("skip".to_string()));
                    }
                }
            }

            // File edit: pre-edit is always a Human (untracked) checkpoint
            return Ok(AgentRunResult {
                agent_id: make_agent_id("my_agent", &input.session_id, ""),
                checkpoint_kind: CheckpointKind::Human,
                will_edit_filepaths: Some(file_paths),
                repo_working_dir: Some(input.cwd.clone()),
                ..Default::default()
            });
        }

        if is_post_edit {
            // Load transcript and model from your agent's storage
            let (transcript, model) = load_my_agent_transcript(&input.session_id)?;

            return Ok(AgentRunResult {
                agent_id: make_agent_id("my_agent", &input.session_id, &model),
                checkpoint_kind: CheckpointKind::AiAgent,
                edited_filepaths: Some(file_paths),
                transcript: Some(transcript),
                repo_working_dir: Some(input.cwd.clone()),
                ..Default::default()
            });
        }

        Err(GitAiError::PresetError(format!("Unknown event: {}", input.hook_event_name)))
    }
}

fn make_agent_id(tool: &str, session_id: &str, model: &str) -> AgentId {
    AgentId {
        tool: tool.to_string(),
        id: session_id.to_string(),
        model: model.to_string(),
    }
}
```

### Bash Tool Handling

If the agent has a bash/shell execution tool (most do), use the shared bash_tool path. Add your agent to `classify_tool` in `bash_tool.rs`:

```rust
// In src/commands/checkpoint_agent/bash_tool.rs classify_tool():
SupportedAgent::MyAgent => {
    if ["bash", "shell", "run_command"].contains(&tool_name_lower.as_str()) {
        ToolClass::Bash
    } else if ["edit_file", "write_file", "apply_patch"].contains(&tool_name_lower.as_str()) {
        ToolClass::FileEdit
    } else {
        ToolClass::Skip
    }
}
```

And add to the `SupportedAgent` enum in `bash_tool.rs`.

### Agent Metadata for Transcript Re-fetching

Include `agent_metadata` with the transcript path so post-commit can re-fetch the transcript:

```rust
let mut agent_metadata = HashMap::new();
agent_metadata.insert("transcript_path".to_string(), transcript_path.to_string_lossy().to_string());
// Include test override path for test isolation:
if let Ok(test_path) = std::env::var("GIT_AI_MY_AGENT_STORAGE_PATH") {
    agent_metadata.insert("__test_storage_path".to_string(), test_path);
}
```

### Transcript Loading

Load the transcript lazily and return `PromptUpdateResult::Updated(transcript, model)`. Register a transcript updater in `src/authorship/prompt_utils.rs`:

```rust
"my_agent" => {
    my_agent_preset::transcript_and_model_from_storage(
        agent_metadata.get("transcript_path").map(String::as_str),
        &agent_id.id,
        agent_metadata.get("__test_storage_path").map(String::as_str),
    )
}
```

## Step 2: Register the Preset

In `src/commands/checkpoint_agent/agent_presets.rs`, add to the preset lookup:

```rust
pub fn get_preset_for_agent(name: &str) -> Option<Box<dyn AgentCheckpointPreset>> {
    match name {
        // ... existing presets ...
        "my_agent" | "my-agent" => Some(Box::new(MyAgentPreset)),
        _ => None,
    }
}
```

Also add to `src/commands/checkpoint_agent/bash_tool.rs` `is_known_preset_for_bash_tool` if the agent uses bash tool detection.

## Step 3: Create the MDM Installer

Create `src/mdm/agents/my_agent.rs`:

```rust
use crate::mdm::hook_installer::{HookInstaller, HookInstallerParams, HookCheckResult};

pub struct MyAgentInstaller;

impl HookInstaller for MyAgentInstaller {
    fn name(&self) -> &str { "My Agent" }
    fn id(&self) -> &str { "my_agent" }
    fn process_names(&self) -> Vec<&str> { vec!["my-agent", "myagent"] }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        // Check if the agent is installed (binary, config dir, etc.)
        let config_path = resolve_my_agent_config_path()?;

        let tool_installed = config_path.exists() || binary_exists("my-agent");
        if !tool_installed {
            return Ok(HookCheckResult::not_installed());
        }

        // Check if git-ai hooks are already in the config
        let hooks_installed = check_my_agent_hooks_installed(&config_path, params)?;
        Ok(HookCheckResult { tool_installed, hooks_installed, hooks_up_to_date: hooks_installed })
    }

    fn install_hooks(&self, params: &HookInstallerParams) -> Result<Option<String>, GitAiError> {
        // Write git-ai hook entries to the agent's config file
        // Return Some(unified_diff_string) for user preview, or None on no-op
        let binary_path = params.binary_path.display().to_string();
        let pre_hook_cmd = format!("{} checkpoint my-agent --hook-input stdin", binary_path);
        let post_hook_cmd = format!("{} checkpoint my-agent --hook-input stdin", binary_path);
        // ... write to config file ...
    }

    fn uninstall_hooks(&self, params: &HookInstallerParams) -> Result<Option<String>, GitAiError> {
        // Remove git-ai entries from the agent's config
    }
}
```

Register in `src/mdm/agents/mod.rs`:

```rust
pub fn get_all_installers() -> Vec<Box<dyn HookInstaller>> {
    vec![
        // ... existing installers ...
        Box::new(my_agent::MyAgentInstaller),
    ]
}
```

And add the module declaration in `src/mdm/agents/mod.rs`:

```rust
pub mod my_agent;
```

## Step 4: Add Test Support

### Integration Tests

Create `tests/integration/my_agent.rs` with the standard pattern:

```rust
#[test]
fn test_my_agent_basic_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test.rs");

    // Simulate pre-edit (human) checkpoint
    std::fs::write(&file_path, "fn old_code() {}\n").unwrap();
    let hook_input_pre = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-123",
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "edit_file",
        "tool_input": { "file_path": "test.rs" }
    });
    repo.git_ai_with_stdin(
        &["checkpoint", "my-agent", "--hook-input", "stdin"],
        hook_input_pre.to_string()
    ).unwrap();

    // Simulate post-edit (AI) checkpoint
    std::fs::write(&file_path, "fn old_code() {}\nfn ai_code() {}\n").unwrap();
    let hook_input_post = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-123",
        "cwd": repo.path().to_str().unwrap(),
        "tool_name": "edit_file",
        "tool_input": { "file_path": "test.rs" }
    });
    repo.git_ai_with_stdin(
        &["checkpoint", "my-agent", "--hook-input", "stdin"],
        hook_input_post.to_string()
    ).unwrap();

    repo.stage_all_and_commit("test commit").unwrap();
    let mut file = repo.filename("test.rs");
    file.assert_committed_lines(lines![
        "fn old_code() {}".unattributed_human(),
        "fn ai_code() {}".ai(),
    ]);
}
```

Register in `tests/integration/main.rs`:
```rust
mod my_agent;
```

### Test Isolation for Storage Paths

Use `GIT_AI_MY_AGENT_STORAGE_PATH` pattern so parallel tests don't conflict on real agent storage:

```rust
repo.patch_git_ai_config(|_patch| {});  // forces isolated HOME
std::env::set_var("GIT_AI_MY_AGENT_STORAGE_PATH", temp_dir.path());
```

Or better, accept it in `agent_metadata` and pass it as `__test_storage_path`.

## Step 5: Add to the Recognized Preset Names

In `tests/integration/repos/test_repo.rs`, add to `is_known_checkpoint_preset`:

```rust
fn is_known_checkpoint_preset(name: &str) -> bool {
    matches!(name,
        // ... existing ...
        | "my-agent" | "my_agent"
    )
}
```

This ensures `normalize_test_git_ai_checkpoint_args` correctly identifies your preset name and inserts `--` before file paths.

## Naming Conventions

| Field | Convention | Example |
|---|---|---|
| Preset name | kebab-case | `"my-agent"` |
| `AgentId.tool` | snake_case | `"my_agent"` |
| Installer `id()` | snake_case | `"my_agent"` |
| Env var override | `GIT_AI_<AGENT>_STORAGE_PATH` | `GIT_AI_MY_AGENT_STORAGE_PATH` |
| Test metadata key | `"__test_storage_path"` | standard key name |

## Checklist

- [ ] `src/commands/checkpoint_agent/<agent>_preset.rs` — preset implementation
- [ ] Register in `agent_presets.rs` `get_preset_for_agent()`
- [ ] Add to `bash_tool.rs` `SupportedAgent` enum and `classify_tool()` (if has bash/shell tool)
- [ ] Register transcript updater in `prompt_utils.rs`
- [ ] `src/mdm/agents/<agent>.rs` — hook installer
- [ ] Register in `src/mdm/agents/mod.rs` `get_all_installers()`
- [ ] `tests/integration/my_agent.rs` — integration tests
- [ ] Register in `tests/integration/main.rs`
- [ ] Add to `is_known_checkpoint_preset` in `test_repo.rs`
- [ ] Run `task test TEST_FILTER=my_agent` to verify
- [ ] Run `task lint && task fmt` before committing

## Reference Implementations

Study these before writing your own:
- **Simple file-edit only**: `src/commands/checkpoint_agent/pi_preset.rs` (Pi has explicit semantic event names, a good clean example)
- **With bash tool**: `src/commands/checkpoint_agent/amp_preset.rs` (AmpPreset has the full pre/post + bash flow)
- **SQLite-backed transcript**: `src/commands/checkpoint_agent/opencode_preset.rs` (reads from SQLite with filesystem fallback)
- **JS plugin installer**: `src/mdm/agents/amp.rs` (embeds a TypeScript plugin file at compile time)
- **IDE settings installer**: `src/mdm/agents/cursor.rs` (checks binary, dotfiles, and IDE settings paths)
