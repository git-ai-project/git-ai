# Checkpoint Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the checkpoint system end-to-end so the CLI subcommand does ~50 lines of work (gather files, send to daemon) and all processing happens in the daemon.

**Architecture:** New `CheckpointRequest` type carries per-file `CheckpointFile` structs with absolute paths, content, repo info, and base commit. No more dirty_files, captured_checkpoint_id, unscoped checkpoints, disk-based blob capture, or local synchronous fallback. Bash pre-snapshots move from disk files to daemon in-memory state. CLI+daemon update atomically — no backwards compatibility needed.

**Tech Stack:** Rust 2024 edition, serde for serialization, local Unix sockets for daemon communication, existing `discover_repository_in_path_no_git_exec` for repo discovery.

**Spec:** `docs/superpowers/specs/2026-05-03-checkpoint-rewrite-design.md`

---

## File Map

### New/rewritten files:
- `src/commands/checkpoint_agent/orchestrator.rs` — rewrite `CheckpointRequest`, `CheckpointFile`, `BaseCommit` types and all `execute_*` functions
- `src/daemon/control_api.rs` — rewrite `CheckpointRun` variant, add bash session control request variants, remove `CheckpointRunRequest`/`LiveCheckpointRunRequest`/`CapturedCheckpointRunRequest`
- `src/daemon/bash_sessions.rs` — **new file**: in-memory bash session state for the daemon

### Modified files:
- `src/commands/git_ai_handlers.rs` — rewrite `handle_checkpoint` to ~50 lines, update `synthesize_hook_input_from_cli_args`, delete ~600 lines of helpers
- `src/commands/checkpoint_agent/presets/mod.rs` — remove `BashPreHookStrategy`, remove `dirty_files` from `PreFileEdit`/`PostFileEdit`/`KnownHumanEdit`, remove `strategy` from `PreBashCall`
- `src/commands/checkpoint_agent/bash_tool.rs` — remove disk I/O for snapshots, use daemon for pre-snapshot storage/retrieval, remove `checkpoint_context_from_active_bash`/`scan_active_bash_snapshots`/`InflightBashAgentContext` serialization, remove captured checkpoint blob logic
- `src/daemon.rs` — update `ingest_checkpoint_payload`, `apply_checkpoint_side_effect`, `ControlRequest::CheckpointRun` handler, `sync_pre_commit_checkpoint_for_daemon_commit`; add bash session handlers; move author identity + metrics here; remove captured checkpoint path
- `src/daemon/coordinator.rs` — update coordinator to handle bash session state
- `src/commands/checkpoint.rs` — remove `PreparedCheckpointManifest`, `PreparedCheckpointFileSource`, `prepare_captured_checkpoint`, `execute_captured_checkpoint`, `update_captured_checkpoint_agent_context`, `explicit_capture_target_paths`, `cleanup_failed_captured_checkpoint_prepare`, async checkpoint blob directory management

### Deleted files:
- `src/authorship/pre_commit.rs` — dead code, no callers

### Test files to update:
- `tests/integration/` — various test files that use `mock_ai`, `mock_known_human`, `human` presets; bash tool conformance tests

---

## Task 1: Define New Core Types

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs:15-27`
- Modify: `src/commands/checkpoint.rs:61-66` (PreparedPathRole stays, referenced by new types)

- [ ] **Step 1: Replace CheckpointRequest and add CheckpointFile + BaseCommit**

In `src/commands/checkpoint_agent/orchestrator.rs`, replace the existing `CheckpointRequest` struct (lines 15-27) with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BaseCommit {
    Sha(String),
    Initial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: PathBuf,
    pub content: Option<String>,
    pub repo_work_dir: PathBuf,
    pub base_commit: BaseCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}
```

Remove the `find_repository_for_file` import (line 10) since we'll use `discover_repository_in_path_no_git_exec` instead. Add:

```rust
use crate::git::repository::discover_repository_in_path_no_git_exec;
```

- [ ] **Step 2: Verify it compiles (expect many errors downstream)**

Run: `task build 2>&1 | head -80`

Expected: Compilation errors in files that reference the old `CheckpointRequest` fields (`repo_working_dir`, `file_paths`, `dirty_files`, `captured_checkpoint_id`). This is expected — we'll fix them in subsequent tasks. Note the error locations for reference.

- [ ] **Step 3: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs
git commit -m "refactor: define new CheckpointRequest, CheckpointFile, BaseCommit types"
```

---

## Task 2: Remove BashPreHookStrategy and dirty_files from Preset Types

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/mod.rs:23-27, 52-95`
- Modify: All preset files that reference `BashPreHookStrategy` or `dirty_files`

- [ ] **Step 1: Remove BashPreHookStrategy and dirty_files from preset types**

In `src/commands/checkpoint_agent/presets/mod.rs`:

Remove the `BashPreHookStrategy` enum (lines 23-27):
```rust
// DELETE:
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BashPreHookStrategy {
    EmitHumanCheckpoint,
    SnapshotOnly,
}
```

Remove `dirty_files` from `PreFileEdit` (line 56):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
}
```

Remove `dirty_files` from `PostFileEdit` (line 63):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub transcript_source: Option<TranscriptSource>,
}
```

Remove `dirty_files` from `KnownHumanEdit` (line 72):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHumanEdit {
    pub trace_id: String,
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub editor_metadata: HashMap<String, String>,
}
```

Remove `strategy` from `PreBashCall` (line 87):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
}
```

- [ ] **Step 2: Fix all preset files that reference removed fields**

For each preset file, find and remove:
- `strategy: BashPreHookStrategy::EmitHumanCheckpoint` or `strategy: BashPreHookStrategy::SnapshotOnly` from `PreBashCall` construction
- `dirty_files: ...` from `PreFileEdit`, `PostFileEdit`, `KnownHumanEdit` construction
- `use ... BashPreHookStrategy` from imports
- Any test assertions on `e.strategy` or `e.dirty_files`

Files to update (search for `BashPreHookStrategy` and `dirty_files`):
- `src/commands/checkpoint_agent/presets/claude.rs`
- `src/commands/checkpoint_agent/presets/codex.rs`
- `src/commands/checkpoint_agent/presets/gemini.rs`
- `src/commands/checkpoint_agent/presets/windsurf.rs`
- `src/commands/checkpoint_agent/presets/continue_cli.rs`
- `src/commands/checkpoint_agent/presets/amp.rs`
- `src/commands/checkpoint_agent/presets/firebender.rs`
- `src/commands/checkpoint_agent/presets/droid.rs`
- `src/commands/checkpoint_agent/presets/opencode.rs`
- `src/commands/checkpoint_agent/presets/github_copilot.rs`
- `src/commands/checkpoint_agent/presets/pi.rs`
- `src/commands/checkpoint_agent/presets/mock_ai.rs`
- `src/commands/checkpoint_agent/presets/mock_known_human.rs`
- `src/commands/checkpoint_agent/presets/human.rs`
- `src/commands/checkpoint_agent/presets/known_human.rs`
- `src/commands/checkpoint_agent/presets/agent_v1.rs`
- `src/commands/checkpoint_agent/presets/ai_tab.rs`
- `src/commands/checkpoint_agent/presets/cursor.rs`
- `src/commands/checkpoint_agent/presets/parse.rs` (contains `dirty_files_from_value` helper)

For the `parse.rs` helper `dirty_files_from_value`, remove it entirely — it's no longer needed.

- [ ] **Step 3: Verify preset unit tests still compile**

Run: `task build 2>&1 | head -80`

Expect errors only from orchestrator and downstream (not from preset files themselves).

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/presets/
git commit -m "refactor: remove BashPreHookStrategy, dirty_files from preset types"
```

---

## Task 3: Rewrite Orchestrator execute_* Functions

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs`

The orchestrator functions must now: resolve repo per file via `discover_repository_in_path_no_git_exec`, get base_commit per repo via `repo.head().target()`, read file content from disk, and build `CheckpointFile` structs.

- [ ] **Step 1: Add helper function for resolving repo and base_commit per file**

Add at the top of `orchestrator.rs` (after imports):

```rust
use std::collections::HashMap as StdHashMap;
use std::fs;

fn resolve_file_context(path: &Path) -> Result<(PathBuf, BaseCommit), GitAiError> {
    let repo = discover_repository_in_path_no_git_exec(path)?;
    let repo_work_dir = repo.workdir()?;
    let base_commit = match repo.head() {
        Ok(head) => match head.target() {
            Ok(sha) => BaseCommit::Sha(sha),
            Err(_) => BaseCommit::Initial,
        },
        Err(_) => BaseCommit::Initial,
    };
    Ok((repo_work_dir, base_commit))
}

fn build_checkpoint_files(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFile>, GitAiError> {
    // Cache repo lookups — files in the same repo share repo_work_dir and base_commit
    let mut repo_cache: StdHashMap<PathBuf, (PathBuf, BaseCommit)> = StdHashMap::new();

    file_paths
        .iter()
        .map(|path| {
            if !path.is_absolute() {
                return Err(GitAiError::PresetError(format!(
                    "file path must be absolute: {}",
                    path.display()
                )));
            }

            // Walk up to find which cached repo_work_dir this file falls under,
            // or resolve a new one
            let (repo_work_dir, base_commit) = {
                let mut found = None;
                for (cached_dir, cached) in &repo_cache {
                    if path.starts_with(cached_dir) {
                        found = Some(cached.clone());
                        break;
                    }
                }
                match found {
                    Some(cached) => cached,
                    None => {
                        let resolved = resolve_file_context(path)?;
                        repo_cache.insert(resolved.0.clone(), resolved.clone());
                        resolved
                    }
                }
            };

            let content = fs::read_to_string(path).ok();

            Ok(CheckpointFile {
                path: path.clone(),
                content,
                repo_work_dir,
                base_commit,
            })
        })
        .collect()
}
```

- [ ] **Step 2: Rewrite execute_pre_file_edit**

Replace the existing `execute_pre_file_edit` function:

```rust
fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;

    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files,
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: e.context.metadata,
    })
}
```

- [ ] **Step 3: Rewrite execute_post_file_edit**

```rust
fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<CheckpointRequest, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;

    let checkpoint_kind = match preset_name {
        "ai_tab" => CheckpointKind::AiTab,
        _ => CheckpointKind::AiAgent,
    };

    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind,
        agent_id: Some(e.context.agent_id),
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
    })
}
```

- [ ] **Step 4: Rewrite execute_known_human_edit**

```rust
fn execute_known_human_edit(e: KnownHumanEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;

    Ok(CheckpointRequest {
        trace_id: e.trace_id,
        checkpoint_kind: CheckpointKind::KnownHuman,
        agent_id: None,
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: None,
        metadata: e.editor_metadata,
    })
}
```

- [ ] **Step 5: Rewrite execute_untracked_edit**

```rust
fn execute_untracked_edit(e: UntrackedEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_checkpoint_files(&e.file_paths)?;

    Ok(CheckpointRequest {
        trace_id: e.trace_id,
        checkpoint_kind: CheckpointKind::Human,
        agent_id: None,
        files,
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: HashMap::new(),
    })
}
```

- [ ] **Step 6: Stub out bash functions temporarily**

The bash functions will be rewritten in Task 8 when we tackle bash sessions. For now, make them compile with the new types:

```rust
fn execute_pre_bash_call(_e: PreBashCall) -> Result<Option<CheckpointRequest>, GitAiError> {
    // TODO: Task 8 will rewrite this to send BashSessionStart to daemon
    Ok(None)
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    // TODO: Task 8 will rewrite this to query daemon for pre-snapshot + do stat diff
    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(e.context.agent_id),
        files: vec![],
        path_role: PreparedPathRole::Edited,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
    })
}
```

- [ ] **Step 7: Remove old helper functions**

Delete `resolve_repo_working_dir_from_file_paths` and `resolve_repo_working_dir_from_cwd` — they're replaced by the per-file resolution in `build_checkpoint_files`.

- [ ] **Step 8: Verify compilation**

Run: `task build 2>&1 | head -80`

Expect errors only from downstream consumers of `CheckpointRequest` (git_ai_handlers.rs, daemon.rs, checkpoint.rs), not from orchestrator.rs itself.

- [ ] **Step 9: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs
git commit -m "refactor: rewrite orchestrator execute_* functions for new CheckpointRequest type"
```

---

## Task 4: Update Control API Types

**Files:**
- Modify: `src/daemon/control_api.rs:9-84`

- [ ] **Step 1: Rewrite CheckpointRun and remove old types**

Replace the existing `CheckpointRun` variant and all associated types in `src/daemon/control_api.rs`:

Remove `CheckpointRunRequest`, `LiveCheckpointRunRequest`, `CapturedCheckpointRunRequest` (lines 39-84).

Update the `CheckpointRun` variant (line 10-14) to:

```rust
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        request: Box<CheckpointRequest>,
    },
```

- [ ] **Step 2: Add bash session control request variants**

Add these new variants to the `ControlRequest` enum:

```rust
    #[serde(rename = "bash_session.start")]
    BashSessionStart {
        repo_work_dir: String,
        session_id: String,
        tool_use_id: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: StatSnapshot,
    },
    #[serde(rename = "bash_session.end")]
    BashSessionEnd {
        session_id: String,
        tool_use_id: String,
    },
    #[serde(rename = "bash_session.query")]
    BashSessionQuery {
        repo_work_dir: String,
    },
    #[serde(rename = "bash_snapshot.query")]
    BashSnapshotQuery {
        session_id: String,
        tool_use_id: String,
    },
```

Add the necessary imports at the top:

```rust
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::StatSnapshot;
use std::collections::HashMap;
```

- [ ] **Step 3: Add BashSessionQueryResponse type**

Add after `ControlResponse`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashSessionQueryResponse {
    pub active: bool,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashSnapshotQueryResponse {
    pub found: bool,
    pub stat_snapshot: Option<StatSnapshot>,
}
```

- [ ] **Step 4: Remove the is_pre_commit and repo_working_dir methods**

Delete the `impl CheckpointRunRequest` block (lines 46-60) — these methods are on the deleted type.

- [ ] **Step 5: Verify compilation of control_api.rs**

Run: `task build 2>&1 | head -80`

Expect errors from daemon.rs where it references the old `CheckpointRunRequest` type. This is expected.

- [ ] **Step 6: Commit**

```bash
git add src/daemon/control_api.rs
git commit -m "refactor: rewrite control API types for checkpoint rewrite"
```

---

## Task 5: Rewrite handle_checkpoint CLI Subcommand

**Files:**
- Modify: `src/commands/git_ai_handlers.rs:294-870+`

- [ ] **Step 1: Update synthesize_hook_input_from_cli_args to resolve absolute paths**

In `src/commands/git_ai_handlers.rs`, find `synthesize_hook_input_from_cli_args` (line 1719). Update the `"human" | "mock_ai" | "mock_known_human"` arm to resolve relative paths to absolute:

```rust
fn synthesize_hook_input_from_cli_args(preset_name: &str, remaining_args: &[String]) -> String {
    match preset_name {
        "human" | "mock_ai" | "mock_known_human" => {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let paths: Vec<String> = remaining_args
                .iter()
                .filter(|a| !a.starts_with("--"))
                .map(|s| {
                    let p = std::path::Path::new(s.as_str());
                    if p.is_absolute() {
                        s.clone()
                    } else {
                        cwd.join(p).to_string_lossy().to_string()
                    }
                })
                .collect();
            serde_json::json!({
                "file_paths": paths,
                "cwd": cwd.to_string_lossy(),
            })
            .to_string()
        }
        "known_human" => {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mut editor = "unknown".to_string();
            let mut editor_version = "unknown".to_string();
            let mut extension_version = "unknown".to_string();
            let mut files: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < remaining_args.len() {
                match remaining_args[i].as_str() {
                    "--editor" if i + 1 < remaining_args.len() => {
                        editor = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--editor-version" if i + 1 < remaining_args.len() => {
                        editor_version = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--extension-version" if i + 1 < remaining_args.len() => {
                        extension_version = remaining_args[i + 1].clone();
                        i += 2;
                    }
                    "--" => {
                        files.extend(remaining_args[i + 1..].iter().map(|s| {
                            let p = std::path::Path::new(s.as_str());
                            if p.is_absolute() {
                                s.clone()
                            } else {
                                cwd.join(p).to_string_lossy().to_string()
                            }
                        }));
                        break;
                    }
                    arg if !arg.starts_with("--") => {
                        let p = std::path::Path::new(arg);
                        if p.is_absolute() {
                            files.push(arg.to_string());
                        } else {
                            files.push(cwd.join(p).to_string_lossy().to_string());
                        }
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            serde_json::json!({
                "editor": editor,
                "editor_version": editor_version,
                "extension_version": extension_version,
                "cwd": cwd.to_string_lossy(),
                "edited_filepaths": files,
            })
            .to_string()
        }
        _ => String::new(),
    }
}
```

- [ ] **Step 2: Rewrite handle_checkpoint**

Replace the entire `handle_checkpoint` function (lines 294-870+) with:

```rust
fn handle_checkpoint(args: &[String]) {
    let mut hook_input = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
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
                        if buffer.trim().is_empty() {
                            eprintln!("No hook input provided (via --hook-input or stdin).");
                            std::process::exit(0);
                        }
                        hook_input = Some(strip_utf8_bom(buffer));
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
            _ => { i += 1; }
        }
    }

    if args.is_empty()
        || crate::commands::checkpoint_agent::presets::resolve_preset(args[0].as_str()).is_err()
    {
        eprintln!("Usage: git-ai checkpoint <preset> [--hook-input <json|stdin>] [files...]");
        std::process::exit(0);
    }

    let effective_hook_input = hook_input
        .unwrap_or_else(|| synthesize_hook_input_from_cli_args(args[0].as_str(), &args[1..]));

    let requests = match crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint(
        args[0].as_str(),
        &effective_hook_input,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} preset error: {}", args[0], e);
            std::process::exit(0);
        }
    };

    if requests.is_empty() {
        std::process::exit(0);
    }

    // Validate: all file paths must be absolute
    for request in &requests {
        for file in &request.files {
            if !file.path.is_absolute() {
                eprintln!("Error: file path must be absolute: {}", file.path.display());
                std::process::exit(1);
            }
        }
    }

    let is_test = std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some();
    let daemon_timeout = if cfg!(windows) || is_test {
        std::time::Duration::from_secs(10)
    } else {
        std::time::Duration::from_secs(5)
    };

    let daemon_config = if is_test
        && (std::env::var_os("GIT_AI_DAEMON_HOME").is_some()
            || std::env::var_os("GIT_AI_DAEMON_CONTROL_SOCKET").is_some())
    {
        crate::daemon::DaemonConfig::from_env_or_default_paths().map_err(|e| e.to_string())
    } else {
        crate::commands::daemon::ensure_daemon_running(daemon_timeout)
    };

    let config = match daemon_config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Daemon unavailable: {}", e);
            std::process::exit(1);
        }
    };

    for request in requests {
        let control_request = ControlRequest::CheckpointRun {
            request: Box::new(request),
        };
        if let Err(e) = crate::daemon::send_control_request(
            &config.control_socket_path,
            &control_request,
        ) {
            eprintln!("Failed to send checkpoint to daemon: {}", e);
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 3: Delete dead helper functions**

Delete these functions from `git_ai_handlers.rs` (search for `fn` to find exact locations):
- `run_checkpoint_via_daemon_or_local` (~line 886)
- `checkpoint_request_has_explicit_capture_scope` (~line 1127)
- `get_all_files_for_mock_ai` (~line 1702)
- `estimate_checkpoint_file_count` (search for it)
- `log_daemon_checkpoint_delegate_failure` (search for it)
- `cleanup_captured_checkpoint_after_delegate_failure` (search for it)
- `log_performance_for_checkpoint` (search for it)
- `CheckpointDispatchOutcome` struct (search for it)
- `checkpoint_kind_to_str` helper (search for it — may still be needed elsewhere, check references first)

Also remove the `group_files_by_repository` function if it's only used by the checkpoint path (grep for all call sites first).

Remove all now-unused imports.

- [ ] **Step 4: Verify compilation of git_ai_handlers.rs**

Run: `task build 2>&1 | head -80`

Expect errors from daemon.rs (still referencing old types). Not from this file.

- [ ] **Step 5: Commit**

```bash
git add src/commands/git_ai_handlers.rs
git commit -m "refactor: rewrite handle_checkpoint to ~50-line daemon-only dispatch"
```

---

## Task 6: Create Daemon Bash Session State

**Files:**
- Create: `src/daemon/bash_sessions.rs`
- Modify: `src/daemon/mod.rs` (add module declaration)

- [ ] **Step 1: Create bash_sessions.rs**

```rust
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::StatSnapshot;
use std::collections::HashMap;
use std::time::Instant;

pub struct BashSession {
    pub repo_work_dir: String,
    pub agent_id: AgentId,
    pub metadata: HashMap<String, String>,
    pub stat_snapshot: StatSnapshot,
    pub started_at: Instant,
}

#[derive(Default)]
pub struct BashSessionState {
    sessions: HashMap<(String, String), BashSession>,
}

impl BashSessionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_session(
        &mut self,
        session_id: String,
        tool_use_id: String,
        repo_work_dir: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: StatSnapshot,
    ) {
        self.sessions.insert(
            (session_id, tool_use_id),
            BashSession {
                repo_work_dir,
                agent_id,
                metadata,
                stat_snapshot,
                started_at: Instant::now(),
            },
        );
    }

    pub fn end_session(&mut self, session_id: &str, tool_use_id: &str) -> Option<BashSession> {
        self.sessions.remove(&(session_id.to_string(), tool_use_id.to_string()))
    }

    pub fn query_active_for_repo(&self, repo_work_dir: &str) -> Option<&BashSession> {
        self.sessions
            .values()
            .find(|s| s.repo_work_dir == repo_work_dir)
    }

    pub fn get_snapshot(&self, session_id: &str, tool_use_id: &str) -> Option<&StatSnapshot> {
        self.sessions
            .get(&(session_id.to_string(), tool_use_id.to_string()))
            .map(|s| &s.stat_snapshot)
    }
}
```

- [ ] **Step 2: Add module declaration**

In `src/daemon/mod.rs`, add:
```rust
pub mod bash_sessions;
```

- [ ] **Step 3: Verify it compiles**

Run: `task build 2>&1 | head -20`

Check that `bash_sessions.rs` compiles. The `StatSnapshot` import requires that type to be `pub` — check and fix if needed.

- [ ] **Step 4: Commit**

```bash
git add src/daemon/bash_sessions.rs src/daemon/mod.rs
git commit -m "feat: add in-memory bash session state for daemon"
```

---

## Task 7: Update Daemon Checkpoint Handling

**Files:**
- Modify: `src/daemon.rs` (the large daemon file)

This is the biggest task. The daemon must:
1. Accept the new `CheckpointRequest` type (no more Live/Captured distinction)
2. Handle the 4 new bash session control requests
3. Move author identity resolution and metrics production to daemon side
4. Update `ingest_checkpoint_payload` (remove `wait` parameter, remove trace-ingest wait logic)
5. Update `apply_checkpoint_side_effect` (single code path, no branching)
6. Update `sync_pre_commit_checkpoint_for_daemon_commit`
7. Update watermark computation after checkpoint

- [ ] **Step 1: Add bash session state to the daemon coordinator**

Find where the daemon coordinator struct is defined (it holds the daemon's runtime state). Add a `BashSessionState` field:

```rust
use crate::daemon::bash_sessions::BashSessionState;
use std::sync::Mutex;

// In the coordinator/daemon struct:
bash_sessions: Mutex<BashSessionState>,
```

Initialize it in the constructor:
```rust
bash_sessions: Mutex::new(BashSessionState::new()),
```

- [ ] **Step 2: Handle new bash session control requests**

In the daemon's `handle_control_request` match (where `ControlRequest` variants are dispatched), add handlers for the 4 new variants:

```rust
ControlRequest::BashSessionStart {
    repo_work_dir,
    session_id,
    tool_use_id,
    agent_id,
    metadata,
    stat_snapshot,
} => {
    let mut state = self.bash_sessions.lock().unwrap();
    state.start_session(session_id, tool_use_id, repo_work_dir, agent_id, metadata, stat_snapshot);
    Ok(ControlResponse::ok(None, None))
}

ControlRequest::BashSessionEnd {
    session_id,
    tool_use_id,
} => {
    let mut state = self.bash_sessions.lock().unwrap();
    state.end_session(&session_id, &tool_use_id);
    Ok(ControlResponse::ok(None, None))
}

ControlRequest::BashSessionQuery { repo_work_dir } => {
    let state = self.bash_sessions.lock().unwrap();
    let response = match state.query_active_for_repo(&repo_work_dir) {
        Some(session) => {
            let data = serde_json::to_value(BashSessionQueryResponse {
                active: true,
                agent_id: Some(session.agent_id.clone()),
                session_id: None, // stored in the key, not needed in response
                tool_use_id: None,
                metadata: Some(session.metadata.clone()),
            }).ok();
            ControlResponse::ok(None, data)
        }
        None => {
            let data = serde_json::to_value(BashSessionQueryResponse {
                active: false,
                agent_id: None,
                session_id: None,
                tool_use_id: None,
                metadata: None,
            }).ok();
            ControlResponse::ok(None, data)
        }
    };
    Ok(response)
}

ControlRequest::BashSnapshotQuery {
    session_id,
    tool_use_id,
} => {
    let state = self.bash_sessions.lock().unwrap();
    let response = match state.get_snapshot(&session_id, &tool_use_id) {
        Some(snapshot) => {
            let data = serde_json::to_value(BashSnapshotQueryResponse {
                found: true,
                stat_snapshot: Some(snapshot.clone()),
            }).ok();
            ControlResponse::ok(None, data)
        }
        None => {
            let data = serde_json::to_value(BashSnapshotQueryResponse {
                found: false,
                stat_snapshot: None,
            }).ok();
            ControlResponse::ok(None, data)
        }
    };
    Ok(response)
}
```

- [ ] **Step 3: Update the CheckpointRun handler**

Replace the existing `ControlRequest::CheckpointRun` match arm (around line 7271). The new version is simpler — no Live/Captured branching, no `wait`:

```rust
ControlRequest::CheckpointRun { request } => {
    // Extract transcript notification before processing
    if let Some(worker) = &self.transcript_worker
        && let Some(transcript_source) = &request.transcript_source
    {
        let session_id = transcript_source.session_id.clone();
        let agent_type = request
            .agent_id
            .as_ref()
            .map(|aid| aid.tool.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let trace_id = request.trace_id.clone();

        if let Some(db) = &self.transcripts_db
            && let Err(e) = Self::ensure_session_exists(
                db,
                &session_id,
                &agent_type,
                transcript_source,
                request.agent_id.as_ref(),
            )
        {
            tracing::warn!(session_id = %session_id, error = %e, "failed to ensure session exists");
        }

        worker
            .notify_checkpoint(session_id, agent_type, trace_id, transcript_source.path.clone())
            .await;
    }

    self.ingest_checkpoint_payload(*request).await
}
```

- [ ] **Step 4: Rewrite ingest_checkpoint_payload**

Replace the existing function. No more `wait` parameter, no Live/Captured branching:

```rust
async fn ingest_checkpoint_payload(
    &self,
    request: CheckpointRequest,
) -> Result<ControlResponse, GitAiError> {
    if request.files.is_empty() {
        return Ok(ControlResponse::ok(None, None));
    }

    // Group files by repo_work_dir to resolve families
    let mut files_by_repo: HashMap<PathBuf, Vec<&CheckpointFile>> = HashMap::new();
    for file in &request.files {
        files_by_repo
            .entry(file.repo_work_dir.clone())
            .or_default()
            .push(file);
    }

    // Submit to each family's sequencer
    for (repo_work_dir, _files) in &files_by_repo {
        let family = self.backend.resolve_family(repo_work_dir)?;
        self.append_checkpoint_to_family_sequencer(&family.0, request.clone(), None)
            .await?;
    }

    Ok(ControlResponse::ok(None, None))
}
```

Note: the exact family sequencer API may differ — adapt to existing `append_checkpoint_to_family_sequencer` signature. The key change is: no `wait` parameter, no `CheckpointRunRequest` wrapper.

- [ ] **Step 5: Rewrite apply_checkpoint_side_effect**

Replace the existing function. Single code path, resolves author here:

```rust
fn apply_checkpoint_side_effect(request: CheckpointRequest) -> Result<(), GitAiError> {
    if request.files.is_empty() {
        return Ok(());
    }

    // Use the first file's repo_work_dir for repo lookup
    let repo_work_dir = &request.files[0].repo_work_dir;
    let repo = find_repository_in_path(&repo_work_dir.to_string_lossy())?;
    let author = repo.git_author_identity().formatted_or_unknown();

    // Call checkpoint processing with the new request
    crate::commands::checkpoint::run(
        &repo,
        &author,
        request.checkpoint_kind,
        true,
        Some(request),
        false,
    )?;
    Ok(())
}
```

Note: `checkpoint::run` still takes the old signature at this point. It will need updating too (see Task 9). For now, adapt the call to match whatever intermediate state the code is in — the goal is to get the daemon dispatching correctly.

- [ ] **Step 6: Update watermark computation after checkpoint**

In `drain_ready_family_sequencer_entries_locked`, the watermark update currently computes `per_file` watermarks from file paths and `per_worktree` watermarks for full Human checkpoints. Update the watermark computation to extract file paths from the new `request.files` vec:

```rust
// Replace file path extraction from the old CheckpointRunRequest with:
let checkpoint_file_paths: Vec<String> = request
    .files
    .iter()
    .map(|f| f.path.to_string_lossy().to_string())
    .collect();
```

The `per_worktree` watermark update for full Human checkpoints should be removed — there are no more unscoped checkpoints, so per_worktree watermarks are never set from this path. Only per_file watermarks get updated.

- [ ] **Step 7: Update sync_pre_commit_checkpoint_for_daemon_commit**

This function (around line 2500) needs to:
1. Remove the `has_active_bash_inflight` disk check — replace with daemon bash session query
2. Build `CheckpointFile` structs from the committed diff snapshot instead of using old types

Since this function runs inside the daemon, it can query `self.bash_sessions` directly (no socket call needed). Update the bash context detection to use the in-memory state.

- [ ] **Step 8: Remove the extract_checkpoint_request helper**

Search for `fn extract_checkpoint_request` — this extracted the `CheckpointRequest` from `CheckpointRunRequest::Live`. No longer needed since we receive `CheckpointRequest` directly.

- [ ] **Step 9: Verify compilation**

Run: `task build 2>&1 | head -80`

At this point, the daemon should compile. Remaining errors should be in `checkpoint.rs` (Task 9) and `bash_tool.rs` (Task 8).

- [ ] **Step 10: Commit**

```bash
git add src/daemon.rs src/daemon/coordinator.rs
git commit -m "refactor: update daemon for new checkpoint types, add bash session handlers"
```

---

## Task 8: Rewrite Bash Tool for Daemon-Based Snapshots

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs`
- Modify: `src/commands/checkpoint_agent/orchestrator.rs` (replace bash stubs from Task 3)

- [ ] **Step 1: Remove disk-based snapshot I/O functions**

In `bash_tool.rs`, delete:
- `save_snapshot` function
- `load_and_consume_snapshot` function
- `snapshot_cache_dir` function
- `cleanup_stale_snapshots` function
- `cache_entry_is_fresh` function
- `checkpoint_context_from_active_bash` function
- `scan_active_bash_snapshots` function
- `has_active_bash_inflight` function
- `ActiveBashSnapshotScan` struct
- `InflightBashAgentContext.into_checkpoint_request` method (but keep the struct fields for now since `StatSnapshot` embeds it)
- `attempt_pre_hook_capture` function
- `attempt_post_hook_capture` function
- `CapturedCheckpointInfo` struct
- `BashToolResult.captured_checkpoint` field

- [ ] **Step 2: Remove InflightBashAgentContext from StatSnapshot**

The `inflight_agent_context` field on `StatSnapshot` was used for disk-based bash session detection. Remove it:

In the `StatSnapshot` struct, remove:
```rust
    // DELETE:
    pub inflight_agent_context: Option<InflightBashAgentContext>,
```

Remove `InflightBashAgentContext` struct entirely — the daemon's `BashSession` now holds this data.

- [ ] **Step 3: Rewrite handle_bash_pre_tool_use_with_context**

The pre-hook now sends the stat snapshot to the daemon instead of writing to disk:

```rust
pub fn handle_bash_pre_tool_use_with_context(
    repo_root: &Path,
    session_id: &str,
    tool_use_id: &str,
    agent_id: &AgentId,
    agent_metadata: Option<&HashMap<String, String>>,
) -> Result<BashToolResult, GitAiError> {
    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // Query daemon watermarks for efficient filtering
    let wm = query_daemon_watermarks(&repo_working_dir).ok();

    // Take filesystem snapshot
    let snap = snapshot(repo_root, session_id, tool_use_id, wm.as_ref())?;

    // Send snapshot to daemon for storage
    let daemon_config = crate::daemon::DaemonConfig::from_env_or_default_paths()
        .map_err(|e| GitAiError::Generic(e))?;

    let request = ControlRequest::BashSessionStart {
        repo_work_dir: repo_working_dir,
        session_id: session_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
        agent_id: agent_id.clone(),
        metadata: agent_metadata.cloned().unwrap_or_default(),
        stat_snapshot: snap,
    };

    crate::daemon::send_control_request(&daemon_config.control_socket_path, &request)?;

    Ok(BashToolResult {
        action: BashCheckpointAction::TakePreSnapshot,
    })
}
```

- [ ] **Step 4: Rewrite handle_bash_tool PostToolUse path**

The post-hook now queries the daemon for the pre-snapshot, does the stat diff, reads changed files inline:

```rust
// In the PostToolUse arm of handle_bash_tool:
HookEvent::PostToolUse => {
    let repo_working_dir = repo_root.to_string_lossy().to_string();

    // Query daemon for pre-snapshot
    let daemon_config = crate::daemon::DaemonConfig::from_env_or_default_paths()
        .map_err(|e| GitAiError::Generic(e))?;

    let query = ControlRequest::BashSnapshotQuery {
        session_id: session_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
    };

    let response = crate::daemon::send_control_request(
        &daemon_config.control_socket_path,
        &query,
    )?;

    let pre_snapshot = if let Some(data) = response.data {
        let snap_response: BashSnapshotQueryResponse =
            serde_json::from_value(data).map_err(GitAiError::JsonError)?;
        snap_response.stat_snapshot
    } else {
        None
    };

    let Some(pre) = pre_snapshot else {
        return Ok(BashToolResult {
            action: BashCheckpointAction::Fallback,
        });
    };

    // Take post-snapshot with same watermarks as pre for consistent filtering
    let post_wm = /* reconstruct DaemonWatermarks from pre.effective_worktree_wm and pre.per_file_wm */;
    let post = snapshot(repo_root, session_id, tool_use_id, post_wm.as_ref())?;

    // Diff pre vs post
    let diff_result = diff(&pre, &post);
    let changed_paths = diff_result.all_changed_paths();

    if changed_paths.is_empty() {
        // Signal end of bash session
        let end_request = ControlRequest::BashSessionEnd {
            session_id: session_id.to_string(),
            tool_use_id: tool_use_id.to_string(),
        };
        let _ = crate::daemon::send_control_request(
            &daemon_config.control_socket_path,
            &end_request,
        );

        return Ok(BashToolResult {
            action: BashCheckpointAction::NoChanges,
        });
    }

    // Signal end of bash session
    let end_request = ControlRequest::BashSessionEnd {
        session_id: session_id.to_string(),
        tool_use_id: tool_use_id.to_string(),
    };
    let _ = crate::daemon::send_control_request(
        &daemon_config.control_socket_path,
        &end_request,
    );

    Ok(BashToolResult {
        action: BashCheckpointAction::Checkpoint(changed_paths),
    })
}
```

- [ ] **Step 5: Update orchestrator bash functions**

Replace the stubs from Task 3 in `orchestrator.rs`:

```rust
fn execute_pre_bash_call(e: PreBashCall) -> Result<Option<CheckpointRequest>, GitAiError> {
    let repo_work_dir = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?
        .workdir()?;

    match bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        Ok(_) => Ok(None), // Pre-bash never emits a checkpoint anymore
        Err(error) => {
            tracing::debug!(
                "Bash pre-hook snapshot failed for {} session {}: {}",
                e.context.agent_id.tool,
                e.context.session_id,
                error
            );
            Ok(None)
        }
    }
}

fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    let repo = discover_repository_in_path_no_git_exec(e.context.cwd.as_path())?;
    let repo_work_dir = repo.workdir()?;

    let bash_result = bash_tool::handle_bash_tool(
        bash_tool::HookEvent::PostToolUse,
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => match &result.action {
            bash_tool::BashCheckpointAction::Checkpoint(paths) => {
                paths.iter().map(|p| repo_work_dir.join(p)).collect()
            }
            _ => vec![],
        },
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            vec![]
        }
    };

    let files = build_checkpoint_files(&file_paths)?;

    Ok(CheckpointRequest {
        trace_id: e.context.trace_id,
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(e.context.agent_id),
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: e.transcript_source,
        metadata: e.context.metadata,
    })
}
```

- [ ] **Step 6: Verify compilation**

Run: `task build 2>&1 | head -80`

- [ ] **Step 7: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs src/commands/checkpoint_agent/orchestrator.rs
git commit -m "refactor: move bash snapshots from disk to daemon memory"
```

---

## Task 9: Update checkpoint.rs Processing

**Files:**
- Modify: `src/commands/checkpoint.rs`

The `checkpoint::run()` function is called from the daemon's `apply_checkpoint_side_effect`. It needs to work with the new `CheckpointRequest` type. The key changes:
- It receives file contents in `request.files[].content` instead of reading from disk or dirty_files
- `base_commit` comes from `request.files[].base_commit` instead of `resolve_base_commit()`
- No more `captured_checkpoint_id` branching
- Remove `is_pre_commit` parameter entirely — with `pre_commit.rs` dead and the daemon never setting it to true, all branches guarded by `is_pre_commit` are dead code. Delete the parameter from `run()`, `run_with_base_commit_override_with_policy()`, `resolve_live_checkpoint_execution()`, `execute_resolved_checkpoint()`, and every function that threads it through. Remove all `if is_pre_commit` branches and the code they guard.

- [ ] **Step 1: Delete captured checkpoint functions**

Remove from `checkpoint.rs`:
- `prepare_captured_checkpoint`
- `execute_captured_checkpoint`
- `update_captured_checkpoint_agent_context`
- `explicit_capture_target_paths`
- `cleanup_failed_captured_checkpoint_prepare`
- `async_checkpoint_internal_dir`
- `async_checkpoint_storage_dir`
- `async_checkpoint_capture_dir`
- `async_checkpoint_manifest_path`
- `PreparedCheckpointManifest` struct
- `PreparedCheckpointFile` struct
- `PreparedCheckpointFileSource` enum

- [ ] **Step 2: Update checkpoint::run signature and internals**

The function should accept the new `CheckpointRequest` and use file contents from `request.files` instead of reading from disk. The exact refactoring depends on how deeply `dirty_files` is woven into the execution — follow the data flow:

1. Where `dirty_files` is set on working_log → use `request.files[].content` instead
2. Where `resolve_base_commit` is called → use `request.files[0].base_commit` 
3. Where individual file content is read from disk → check `request.files` first

This is a careful surgery task — trace every reference to `dirty_files`, `captured_checkpoint_id`, and `is_pre_commit` and update or remove.

- [ ] **Step 3: Verify compilation**

Run: `task build 2>&1 | head -80`

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint.rs
git commit -m "refactor: update checkpoint processing for new types, remove captured checkpoint code"
```

---

## Task 10: Delete Dead Code

**Files:**
- Delete: `src/authorship/pre_commit.rs`
- Modify: `src/authorship/mod.rs` (remove `pub mod pre_commit;`)
- Clean up remaining references

- [ ] **Step 1: Delete pre_commit.rs and its module declaration**

```bash
rm src/authorship/pre_commit.rs
```

In `src/authorship/mod.rs`, remove:
```rust
pub mod pre_commit;
```

- [ ] **Step 2: Search for and remove any remaining references to deleted types**

```bash
# Search for remaining references to old types
grep -rn "CapturedCheckpointRunRequest\|LiveCheckpointRunRequest\|CheckpointRunRequest\|captured_checkpoint_id\|dirty_files\|BashPreHookStrategy\|PreparedCheckpointManifest\|InflightBashAgentContext\|checkpoint_context_from_active_bash\|scan_active_bash_snapshots\|has_active_bash_inflight\|async_checkpoint_blob\|async-checkpoint-blob" src/ --include="*.rs"
```

Fix any remaining references.

- [ ] **Step 3: Clean up unused imports across all modified files**

Run: `task build 2>&1` and fix all "unused import" warnings.

- [ ] **Step 4: Verify full compilation**

Run: `task build`

Expected: Clean compilation with no errors. Warnings about unused code are OK at this stage.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: delete dead checkpoint code — pre_commit module, captured checkpoint types, disk snapshot I/O"
```

---

## Task 11: Run and Fix Lint/Format

**Files:**
- Various (auto-fixed by formatter)

- [ ] **Step 1: Run formatter**

Run: `task fmt`

- [ ] **Step 2: Run linter**

Run: `task lint`

Fix any lint errors.

- [ ] **Step 3: Commit if changes**

```bash
git add -A
git commit -m "style: fix lint and format issues from checkpoint rewrite"
```

---

## Task 12: Run Tests and Fix Failures

**Files:**
- `tests/integration/` — various test files

- [ ] **Step 1: Run the full test suite**

Run: `task test`

Collect all failures. Categorize each failure:
1. **Type mismatch** — test constructs old `CheckpointRequest` directly → update to new types
2. **Missing function** — test calls deleted function → remove or rewrite test
3. **Behavioral change** — test relied on unscoped checkpoint or captured checkpoint behavior → evaluate whether the test was correct or was a band-aid (per user guidance about bash mtime tests)
4. **Genuine regression** — new code has a bug → fix

- [ ] **Step 2: Fix test failures iteratively**

For each failing test:
- Read the test to understand what it's testing
- If it's testing deleted functionality (captured checkpoints, unscoped checkpoints, disk snapshots), delete the test
- If it's testing attribution correctness, update to use new types and verify the attribution logic is preserved
- If it's a bash test with mtime issues, evaluate whether it was a poorly-configured test

Run: `task test` after each batch of fixes.

- [ ] **Step 3: Run specific test categories to verify**

```bash
task test TEST_FILTER=checkpoint
task test TEST_FILTER=bash
task test TEST_FILTER=attribution
task test TEST_FILTER=blame
```

- [ ] **Step 4: Commit fixes**

```bash
git add -A
git commit -m "test: update integration tests for checkpoint rewrite"
```

---

## Task 13: Final Verification

- [ ] **Step 1: Full test suite**

Run: `task test`

Expected: All tests pass.

- [ ] **Step 2: Lint and format**

Run: `task lint && task fmt`

Expected: Clean.

- [ ] **Step 3: Build release mode**

Run: `task build`

Expected: Clean compilation.

- [ ] **Step 4: Verify no remaining references to old types**

```bash
grep -rn "dirty_files\|captured_checkpoint_id\|CapturedCheckpointRunRequest\|LiveCheckpointRunRequest\|BashPreHookStrategy\|PreparedCheckpointManifest\|InflightBashAgentContext" src/ --include="*.rs"
```

Expected: No results (or only in comments explaining why something was removed).

- [ ] **Step 5: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "refactor: final cleanup for checkpoint rewrite"
```
