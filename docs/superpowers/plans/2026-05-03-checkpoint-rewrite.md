# Checkpoint Logic Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the checkpoint ingestion pipeline end-to-end so the CLI subcommand is ~40 lines, all processing lives in the daemon, and one unified request type carries file contents directly.

**Architecture:** Define new `CheckpointRequest` / `CheckpointFileEntry` types in the orchestrator. Rewrite the subcommand and control API to use them. Update daemon ingestion to group files by repo and process. Then sweep dead code: captured checkpoints, dirty_files, unscoped paths, sync fallback, is_pre_commit threading.

**Tech Stack:** Rust 2024 edition, serde for serialization, Unix domain sockets for IPC, insta for snapshot tests.

---

### Task 1: Define new types

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs:15-27`
- Modify: `src/commands/checkpoint.rs:61-66` (PreparedPathRole stays but verify it compiles standalone)

- [ ] **Step 1: Replace CheckpointRequest and add CheckpointFileEntry**

Replace the existing `CheckpointRequest` struct at `src/commands/checkpoint_agent/orchestrator.rs:15-27` with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFileEntry {
    pub path: PathBuf,
    pub content: String,
    pub repo_work_dir: PathBuf,
    pub base_commit_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub trace_id: String,
    pub checkpoint_kind: CheckpointKind,
    pub agent_id: Option<AgentId>,
    pub files: Vec<CheckpointFileEntry>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}
```

Remove the imports for `find_repository_for_file` (will be re-added when orchestrator is rewritten in Task 3).

- [ ] **Step 2: Verify the project does NOT compile**

Run: `task build 2>&1 | head -80`

Expected: Compilation errors everywhere that references the removed fields (`repo_working_dir`, `file_paths`, `dirty_files`, `captured_checkpoint_id`). This is correct — the compiler is now our guide for every callsite that needs updating.

- [ ] **Step 3: Commit the new types**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs
git commit -m "feat: define new CheckpointRequest and CheckpointFileEntry types

Removes repo_working_dir, file_paths, dirty_files, captured_checkpoint_id
from CheckpointRequest. Adds files: Vec<CheckpointFileEntry> with per-file
path, content, repo_work_dir, and base_commit_sha.

Note: This intentionally breaks compilation. Subsequent commits will update
all consumers."
```

---

### Task 2: Simplify control API types

**Files:**
- Modify: `src/daemon/control_api.rs:7-84`

- [ ] **Step 1: Replace CheckpointRun and remove wrapper types**

Replace the `ControlRequest::CheckpointRun` variant and remove `CheckpointRunRequest`, `LiveCheckpointRunRequest`, `CapturedCheckpointRunRequest`, and their impl blocks. The entire file from line 7 to 84 becomes:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        request: Box<CheckpointRequest>,
    },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    #[serde(rename = "telemetry.submit")]
    SubmitTelemetry { envelopes: Vec<TelemetryEnvelope> },
    #[serde(rename = "cas.submit")]
    SubmitCas { records: Vec<CasSyncPayload> },
    #[serde(rename = "wrapper.pre_state")]
    WrapperPreState {
        invocation_id: String,
        repo_working_dir: String,
        repo_context: RepoContext,
    },
    #[serde(rename = "wrapper.post_state")]
    WrapperPostState {
        invocation_id: String,
        repo_working_dir: String,
        repo_context: RepoContext,
    },
    #[serde(rename = "snapshot.watermarks")]
    SnapshotWatermarks { repo_working_dir: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}
```

Remove the `use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;` import if it exists and add it fresh at the top. Remove the `CheckpointRunRequest` enum, its impl block, `LiveCheckpointRunRequest`, and `CapturedCheckpointRunRequest`. Keep `ControlResponse`, `FamilyStatus`, `TelemetryEnvelope`, and `CasSyncPayload` unchanged.

- [ ] **Step 2: Commit**

```bash
git add src/daemon/control_api.rs
git commit -m "refactor: simplify control API - remove CheckpointRunRequest wrapper types

CheckpointRun now carries CheckpointRequest directly. No wait parameter
(always async). Removed LiveCheckpointRunRequest, CapturedCheckpointRunRequest,
and CheckpointRunRequest enum."
```

---

### Task 3: Rewrite orchestrator event handlers

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs`
- Modify: `src/commands/checkpoint_agent/presets/mod.rs:52-81` (remove dirty_files from event types)

The orchestrator handlers need to: resolve repo per file, get HEAD, read file content, and pack into `CheckpointFileEntry`. The bash handlers are done separately in Task 4.

- [ ] **Step 1: Add helper functions to orchestrator**

Add these helpers after the imports in `orchestrator.rs`:

```rust
use std::fs;

fn resolve_repo_and_head(file_path: &Path) -> Result<(PathBuf, String), GitAiError> {
    let repo = find_repository_for_file(&file_path.to_string_lossy(), None)?;
    let work_dir = repo.workdir()?;
    let head = repo.rev_parse_head().unwrap_or_default();
    Ok((work_dir, head))
}

fn build_file_entries(file_paths: &[PathBuf]) -> Result<Vec<CheckpointFileEntry>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(vec![]);
    }
    // Cache repo lookups — files in the same repo share work_dir and head
    let mut repo_cache: HashMap<PathBuf, (PathBuf, String)> = HashMap::new();
    let mut entries = Vec::with_capacity(file_paths.len());

    for path in file_paths {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }
        let repo = find_repository_for_file(&path.to_string_lossy(), None)?;
        let work_dir = repo.workdir()?;
        let (repo_work_dir, base_commit_sha) = repo_cache
            .entry(work_dir.clone())
            .or_insert_with(|| {
                let head = repo.rev_parse_head().unwrap_or_default();
                (work_dir, head)
            })
            .clone();

        let content = fs::read_to_string(path).unwrap_or_default();
        entries.push(CheckpointFileEntry {
            path: path.clone(),
            content,
            repo_work_dir,
            base_commit_sha,
        });
    }
    Ok(entries)
}

fn build_file_entries_with_content(
    files_with_content: &[(PathBuf, String)],
) -> Result<Vec<CheckpointFileEntry>, GitAiError> {
    if files_with_content.is_empty() {
        return Ok(vec![]);
    }
    let mut repo_cache: HashMap<PathBuf, (PathBuf, String)> = HashMap::new();
    let mut entries = Vec::with_capacity(files_with_content.len());

    for (path, content) in files_with_content {
        if !path.is_absolute() {
            return Err(GitAiError::PresetError(format!(
                "file path must be absolute: {}",
                path.display()
            )));
        }
        let repo = find_repository_for_file(&path.to_string_lossy(), None)?;
        let work_dir = repo.workdir()?;
        let (repo_work_dir, base_commit_sha) = repo_cache
            .entry(work_dir.clone())
            .or_insert_with(|| {
                let head = repo.rev_parse_head().unwrap_or_default();
                (work_dir, head)
            })
            .clone();

        entries.push(CheckpointFileEntry {
            path: path.clone(),
            content: content.clone(),
            repo_work_dir,
            base_commit_sha,
        });
    }
    Ok(entries)
}
```

- [ ] **Step 2: Remove dirty_files from preset event types**

In `src/commands/checkpoint_agent/presets/mod.rs`, remove `dirty_files` from `PreFileEdit`, `PostFileEdit`, and `KnownHumanEdit`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub transcript_source: Option<TranscriptSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHumanEdit {
    pub trace_id: String,
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub editor_metadata: HashMap<String, String>,
}
```

This will cause compilation errors in individual preset files that set `dirty_files`. Each preset file that constructs these event types needs to have the `dirty_files` field removed from the struct literal. Grep for `dirty_files` in `src/commands/checkpoint_agent/presets/` and remove the field from each constructor site.

Run: `grep -rn "dirty_files" src/commands/checkpoint_agent/presets/ --include="*.rs"`

Fix each hit by removing the `dirty_files: ...` field from the struct literal.

- [ ] **Step 3: Rewrite non-bash event handlers**

Replace `execute_pre_file_edit`, `execute_post_file_edit`, `execute_known_human_edit`, and `execute_untracked_edit` in `orchestrator.rs`. Remove `resolve_repo_working_dir_from_file_paths` and `resolve_repo_working_dir_from_cwd` — they're no longer needed.

```rust
fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
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

fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
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

fn execute_known_human_edit(e: KnownHumanEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
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

fn execute_untracked_edit(e: UntrackedEdit) -> Result<CheckpointRequest, GitAiError> {
    let files = build_file_entries(&e.file_paths)?;
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

- [ ] **Step 4: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs src/commands/checkpoint_agent/presets/
git commit -m "refactor: rewrite orchestrator to build CheckpointFileEntry with content

Each event handler now resolves repo, reads HEAD, reads file content, and
packs into CheckpointFileEntry. Removed dirty_files from preset event types."
```

---

### Task 4: Rewrite bash checkpoint orchestrator handlers

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs` (execute_pre_bash_call, execute_post_bash_call)
- Modify: `src/commands/checkpoint_agent/bash_tool.rs` (update BashToolResult, remove CapturedCheckpointInfo)

The bash handlers are separate because they use the stat snapshot system. The watermark/stat logic is unchanged — only the output packaging changes.

- [ ] **Step 1: Update BashToolResult to remove captured_checkpoint**

In `src/commands/checkpoint_agent/bash_tool.rs`, find `BashToolResult` (line ~273) and `CapturedCheckpointInfo` (line ~287). Remove `CapturedCheckpointInfo` entirely. Change `BashToolResult` to:

```rust
pub struct BashToolResult {
    pub action: BashCheckpointAction,
}
```

Then find and fix all places that construct `BashToolResult` — remove the `captured_checkpoint` field. Grep:

Run: `grep -n "captured_checkpoint" src/commands/checkpoint_agent/bash_tool.rs`

At each construction site, remove the `captured_checkpoint` field. At each read site (`.captured_checkpoint`), remove the access. The `handle_bash_pre_tool_use_with_context` function currently returns a `BashToolResult` with a `captured_checkpoint` — change it to return `BashToolResult { action: BashCheckpointAction::TakePreSnapshot }` (or whatever it returns now, just without the captured checkpoint field).

- [ ] **Step 2: Rewrite execute_pre_bash_call**

The pre-bash handler needs to: take the stat snapshot (unchanged), then find already-dirty files and read their contents to emit a Human checkpoint.

```rust
fn execute_pre_bash_call(e: PreBashCall) -> Result<Option<CheckpointRequest>, GitAiError> {
    let repo = find_repository_for_file(&e.context.cwd.to_string_lossy(), None)?;
    let repo_working_dir = repo.workdir()?;

    // Take the stat snapshot for later diffing — this is unchanged
    if let Err(error) = bash_tool::handle_bash_pre_tool_use_with_context(
        &repo_working_dir,
        &e.context.session_id,
        &e.tool_use_id,
        &e.context.agent_id,
        Some(&e.context.metadata),
    ) {
        tracing::debug!(
            "Bash pre-hook snapshot failed for {} session {}: {}",
            e.context.agent_id.tool,
            e.context.session_id,
            error
        );
    }

    match e.strategy {
        BashPreHookStrategy::EmitHumanCheckpoint => {
            // Find dirty files and read their contents for the Human checkpoint
            let dirty_paths = repo.get_staged_and_unstaged_filenames().unwrap_or_default();
            if dirty_paths.is_empty() {
                return Ok(None);
            }
            let abs_paths: Vec<PathBuf> = dirty_paths
                .into_iter()
                .map(|p| {
                    let pb = PathBuf::from(&p);
                    if pb.is_absolute() { pb } else { repo_working_dir.join(pb) }
                })
                .collect();
            let files = build_file_entries(&abs_paths)?;
            if files.is_empty() {
                return Ok(None);
            }
            Ok(Some(CheckpointRequest {
                trace_id: e.context.trace_id,
                checkpoint_kind: CheckpointKind::Human,
                agent_id: None,
                files,
                path_role: PreparedPathRole::WillEdit,
                transcript_source: None,
                metadata: e.context.metadata,
            }))
        }
        BashPreHookStrategy::SnapshotOnly => Ok(None),
    }
}
```

- [ ] **Step 3: Rewrite execute_post_bash_call**

The post-bash handler diffs stat snapshots (unchanged), then reads content for changed files.

```rust
fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    let repo = find_repository_for_file(&e.context.cwd.to_string_lossy(), None)?;
    let repo_working_dir = repo.workdir()?;

    let bash_result = bash_tool::handle_bash_tool(
        HookEvent::PostToolUse,
        &repo_working_dir,
        &e.context.session_id,
        &e.tool_use_id,
    );

    let file_paths: Vec<PathBuf> = match &bash_result {
        Ok(result) => match &result.action {
            bash_tool::BashCheckpointAction::Checkpoint(paths) => paths
                .iter()
                .map(|p| {
                    let pb = PathBuf::from(p);
                    if pb.is_absolute() { pb } else { repo_working_dir.join(pb) }
                })
                .collect(),
            _ => vec![],
        },
        Err(err) => {
            tracing::debug!("Bash tool post-hook error: {}", err);
            vec![]
        }
    };

    let files = build_file_entries(&file_paths)?;

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

- [ ] **Step 4: Remove checkpoint_context_from_active_bash**

This function (bash_tool.rs line ~372) was only used by the unscoped pre-commit path in `handle_checkpoint` and by `sync_pre_commit_checkpoint_for_daemon_commit`. The pre-commit path is being removed. The daemon commit replay will be updated in Task 7 to not need this. Remove the function. Also remove `InflightBashAgentContext::into_checkpoint_request()` since it constructs the old type.

Grep for callers first to make sure:

Run: `grep -rn "checkpoint_context_from_active_bash" src/ --include="*.rs"`

If the daemon still references it, leave a `todo!()` stub that Task 7 will replace.

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs src/commands/checkpoint_agent/bash_tool.rs
git commit -m "refactor: rewrite bash checkpoint handlers to use unified file entries

Pre-bash reads dirty file contents for Human checkpoint. Post-bash reads
changed file contents after stat diff. Removed CapturedCheckpointInfo and
captured_checkpoint from BashToolResult."
```

---

### Task 5: Rewrite handle_checkpoint subcommand

**Files:**
- Modify: `src/commands/git_ai_handlers.rs:294-877` (handle_checkpoint)
- Modify: `src/commands/git_ai_handlers.rs:886-1202` (remove run_checkpoint_via_daemon_or_local and helpers)
- Modify: `src/commands/git_ai_handlers.rs:1719-1785` (update synthesize_hook_input_from_cli_args)

- [ ] **Step 1: Update synthesize_hook_input_from_cli_args to absolutize paths**

In the `"human" | "mock_ai" | "mock_known_human"` arm, convert relative paths to absolute using CWD:

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
                    let p = std::path::Path::new(s);
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
                            let p = std::path::Path::new(s);
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

Replace the entire `handle_checkpoint` function (lines 294-877) with:

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
                        if !buffer.trim().is_empty() {
                            hook_input = Some(strip_utf8_bom(buffer));
                        } else {
                            eprintln!("No hook input provided (via --hook-input or stdin).");
                            std::process::exit(0);
                        }
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
            _ => {
                i += 1;
            }
        }
    }

    if args.is_empty()
        || crate::commands::checkpoint_agent::presets::resolve_preset(args[0].as_str()).is_err()
    {
        eprintln!("Error: checkpoint requires a valid preset name as the first argument");
        std::process::exit(0);
    }

    let preset_name = args[0].as_str();
    let effective_hook_input = hook_input
        .unwrap_or_else(|| synthesize_hook_input_from_cli_args(preset_name, &args[1..]));

    let checkpoint_requests = match crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint(
        preset_name,
        &effective_hook_input,
    ) {
        Ok(results) => results,
        Err(e) => {
            eprintln!("{} preset error: {}", preset_name, e);
            std::process::exit(0);
        }
    };

    if checkpoint_requests.is_empty() {
        std::process::exit(0);
    }

    // Emit AgentUsage metric for AI checkpoints
    for request in &checkpoint_requests {
        if request.checkpoint_kind.is_ai()
            && let Some(ref agent_id) = request.agent_id
            && commands::checkpoint::should_emit_agent_usage(agent_id)
        {
            let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);
            let session_id = crate::authorship::authorship_log_serialization::generate_session_id(
                &agent_id.id,
                &agent_id.tool,
            );
            let attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                .session_id(session_id)
                .tool(&agent_id.tool)
                .model(&agent_id.model)
                .prompt_id(prompt_id)
                .external_prompt_id(&agent_id.id)
                .custom_attributes_map(crate::config::Config::fresh().custom_attributes());
            let values = crate::metrics::AgentUsageValues::new();
            crate::metrics::record(values, attrs);
        }
    }

    // Send to daemon
    let checkpoint_start = std::time::Instant::now();
    let is_test = std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some();
    let checkpoint_daemon_timeout = if cfg!(windows) || is_test {
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
        crate::commands::daemon::ensure_daemon_running(checkpoint_daemon_timeout)
    };

    let daemon_config = match daemon_config {
        Ok(config) => config,
        Err(e) => {
            eprintln!("[git-ai] daemon startup failed: {}; checkpoint dropped", e);
            std::process::exit(0);
        }
    };

    for request in checkpoint_requests {
        let control_request = ControlRequest::CheckpointRun {
            request: Box::new(request),
        };
        match send_control_request(&daemon_config.control_socket_path, &control_request) {
            Ok(response) if response.ok => {}
            Ok(response) => {
                let msg = response.error.unwrap_or_else(|| "unknown error".to_string());
                eprintln!("[git-ai] checkpoint rejected by daemon: {}", msg);
            }
            Err(e) => {
                eprintln!("[git-ai] checkpoint send failed: {}", e);
            }
        }
    }

    let elapsed = checkpoint_start.elapsed();
    eprintln!("Checkpoint dispatched in {:?}", elapsed);
}
```

- [ ] **Step 3: Delete dead functions**

Remove these functions from `git_ai_handlers.rs`:
- `run_checkpoint_via_daemon_or_local` (lines ~886-1125)
- `CheckpointDispatchOutcome` struct (lines ~880-883)
- `checkpoint_request_has_explicit_capture_scope` (lines ~1127-1155)
- `cleanup_captured_checkpoint_after_delegate_failure` (lines ~1157-1178)
- `log_daemon_checkpoint_delegate_failure` (lines ~1180-1202)
- `checkpoint_kind_to_str` (lines ~1204+)
- `get_all_files_for_mock_ai` (lines ~1702-1715)
- `estimate_checkpoint_file_count` (grep for it and remove)

Also remove imports that are no longer needed: `find_repository_in_path`, `group_files_by_repository`, `Repository`, `CheckpointKind` (if no longer used), `PreparedPathRole`, etc. Let the compiler guide you.

- [ ] **Step 4: Commit**

```bash
git add src/commands/git_ai_handlers.rs
git commit -m "refactor: rewrite handle_checkpoint to ~60 lines

Parse args, run preset, send to daemon. No repo detection, no sync fallback,
no captured checkpoint ceremony, no multi-repo grouping. Removed
run_checkpoint_via_daemon_or_local and all helper functions."
```

---

### Task 6: Update daemon checkpoint ingestion

**Files:**
- Modify: `src/daemon.rs` (handle_control_request, ingest_checkpoint_payload, apply_checkpoint_side_effect, FamilySequencerEntry, drain_ready_family_sequencer_entries_locked)
- Modify: `src/daemon/test_sync.rs`

- [ ] **Step 1: Update handle_control_request**

The `CheckpointRun` arm now receives `CheckpointRequest` directly. Update `handle_control_request` (line ~7269):

```rust
ControlRequest::CheckpointRun { request } => {
    // Extract transcript notification before processing checkpoint
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
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "failed to ensure session exists"
            );
        }

        worker
            .notify_checkpoint(
                session_id,
                agent_type,
                trace_id,
                transcript_source.path.clone(),
            )
            .await;
    }

    self.ingest_checkpoint_payload(*request).await
}
```

Remove `extract_checkpoint_request` — it's no longer needed.

- [ ] **Step 2: Rewrite ingest_checkpoint_payload**

The function now takes `CheckpointRequest` directly, groups files by `repo_work_dir`, and ingests per-repo:

```rust
async fn ingest_checkpoint_payload(
    &self,
    request: CheckpointRequest,
) -> Result<ControlResponse, GitAiError> {
    if request.files.is_empty() {
        return Ok(ControlResponse::ok(None, None));
    }

    // Group files by repo_work_dir
    let mut by_repo: HashMap<String, Vec<&CheckpointFileEntry>> = HashMap::new();
    for file in &request.files {
        let key = file.repo_work_dir.to_string_lossy().to_string();
        by_repo.entry(key).or_default().push(file);
    }

    for (repo_working_dir, _files) in &by_repo {
        if repo_working_dir.trim().is_empty() {
            return Err(GitAiError::Generic(
                "checkpoint request has file with empty repo_work_dir".to_string(),
            ));
        }
        let family = self.backend.resolve_family(Path::new(repo_working_dir))?;

        self.append_checkpoint_to_family_sequencer(&family.0, request.clone(), None)
            .await?;
    }

    Ok(ControlResponse::ok(None, None))
}
```

Note: The sequencer now takes `CheckpointRequest` instead of `CheckpointRunRequest`. This requires updating `FamilySequencerEntry` and `append_checkpoint_to_family_sequencer`.

- [ ] **Step 3: Update FamilySequencerEntry and sequencer functions**

Change `FamilySequencerEntry::Checkpoint` to use `CheckpointRequest`:

```rust
enum FamilySequencerEntry {
    PendingRoot,
    ReadyCommand(Box<crate::daemon::domain::NormalizedCommand>),
    Checkpoint {
        request: Box<CheckpointRequest>,
        respond_to: Option<oneshot::Sender<Result<u64, GitAiError>>>,
    },
    Canceled,
}
```

Update `append_checkpoint_to_family_sequencer` signature:

```rust
async fn append_checkpoint_to_family_sequencer(
    &self,
    family: &str,
    request: CheckpointRequest,
    respond_to: Option<oneshot::Sender<Result<u64, GitAiError>>>,
) -> Result<(), GitAiError> {
```

- [ ] **Step 4: Update drain_ready_family_sequencer_entries_locked checkpoint arm**

In the `FamilySequencerEntry::Checkpoint` processing block (line ~5645), update to work with the new `CheckpointRequest` type. The key changes:

- Extract `repo_work_dir` from files (first file's repo_work_dir, or group as needed)
- Extract `checkpoint_kind` directly from request
- Extract file paths directly from `request.files`
- Remove all `CheckpointRunRequest::Live`/`Captured` match arms
- Remove `captured_checkpoint_id` cleanup
- The `is_live_human_checkpoint` check becomes `request.checkpoint_kind == CheckpointKind::Human`
- Update `checkpoint_kind_str` to use `request.checkpoint_kind.to_str()`

```rust
FamilySequencerEntry::Checkpoint {
    request,
    respond_to,
} => {
    let repo_wd = request.files.first()
        .map(|f| f.repo_work_dir.to_string_lossy().to_string())
        .unwrap_or_default();
    let checkpoint_file_paths: Vec<String> = request.files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    let is_human_checkpoint = request.checkpoint_kind == CheckpointKind::Human;
    let should_log_completion =
        crate::daemon::test_sync::tracks_checkpoint_request_for_test_sync(&request);
    let checkpoint_kind_str = request.checkpoint_kind.to_str().to_string();
    tracing::info!(kind = %checkpoint_kind_str, repo = %repo_wd, "checkpoint start");
    let checkpoint_start = std::time::Instant::now();

    let checkpoint_result = {
        let future = async {
            let ack = self
                .coordinator
                .apply_checkpoint(Path::new(&repo_wd))
                .await;
            match ack {
                Ok(ack) => apply_checkpoint_side_effect(*request).map(|_| ack.seq),
                Err(error) => Err(error),
            }
        };
        let caught = std::panic::AssertUnwindSafe(future);
        futures::FutureExt::catch_unwind(caught).await
    };
    // ... rest of the checkpoint completion logging stays similar,
    // just remove captured_checkpoint_id cleanup block and update variable names
```

- [ ] **Step 5: Rewrite apply_checkpoint_side_effect**

```rust
fn apply_checkpoint_side_effect(request: CheckpointRequest) -> Result<(), GitAiError> {
    let repo_work_dir = request.files.first()
        .map(|f| &f.repo_work_dir)
        .ok_or_else(|| GitAiError::Generic("checkpoint request has no files".to_string()))?;
    let repo = find_repository_in_path(&repo_work_dir.to_string_lossy())?;
    let author = repo.git_author_identity().formatted_or_unknown();

    crate::commands::checkpoint::run(
        &repo,
        &author,
        request.checkpoint_kind,
        true,
        Some(request),
    )?;
    Ok(())
}
```

This requires updating `checkpoint::run` to accept the new `CheckpointRequest` type — that's Task 8.

- [ ] **Step 6: Update test_sync.rs**

In `src/daemon/test_sync.rs`, update `tracks_checkpoint_request_for_test_sync` to take `&CheckpointRequest`:

```rust
pub fn tracks_checkpoint_request_for_test_sync(
    request: &crate::commands::checkpoint_agent::orchestrator::CheckpointRequest,
) -> bool {
    true
}
```

Since `is_pre_commit` is gone, all checkpoint requests are tracked for test sync.

- [ ] **Step 7: Update checkpoint_control_response_timeout**

Since there's no `wait` field anymore, simplify the timeout function. All checkpoint requests use the short timeout in production, long timeout in CI/test:

```rust
fn checkpoint_control_response_timeout(
    request: &ControlRequest,
    use_ci_or_test_budget: bool,
) -> Duration {
    match request {
        ControlRequest::CheckpointRun { .. } if use_ci_or_test_budget => {
            DAEMON_CHECKPOINT_RESPONSE_TIMEOUT
        }
        ControlRequest::CheckpointRun { .. } => DAEMON_CONTROL_RESPONSE_TIMEOUT,
        ControlRequest::SnapshotWatermarks { .. } => Duration::from_millis(500),
        _ => DAEMON_CONTROL_RESPONSE_TIMEOUT,
    }
}
```

- [ ] **Step 8: Commit**

```bash
git add src/daemon.rs src/daemon/control_api.rs src/daemon/test_sync.rs
git commit -m "refactor: update daemon to process CheckpointRequest directly

Removed CheckpointRunRequest indirection. ingest_checkpoint_payload groups
files by repo. apply_checkpoint_side_effect resolves author from repo.
Removed wait logic, captured checkpoint cleanup, is_pre_commit checks."
```

---

### Task 7: Update daemon commit replay

**Files:**
- Modify: `src/daemon.rs` (sync_pre_commit_checkpoint_for_daemon_commit, build_human_replay_checkpoint_request)

- [ ] **Step 1: Rewrite sync_pre_commit_checkpoint_for_daemon_commit**

This function (line ~2500) needs to construct a `CheckpointRequest` with the new type. It already has file content from `committed_file_snapshot_between_commits` and knows the base_commit and repo:

```rust
fn sync_pre_commit_checkpoint_for_daemon_commit(
    repo: &Repository,
    rewrite_event: &RewriteLogEvent,
    author: &str,
    carryover_snapshot: Option<&HashMap<String, String>>,
) -> Result<(), GitAiError> {
    let Some((base_commit, target_commit)) =
        commit_replay_context_from_rewrite_event(rewrite_event)
    else {
        return Ok(());
    };
    if base_commit.trim().is_empty() || target_commit.trim().is_empty() {
        return Ok(());
    }

    let repo_workdir = repo
        .workdir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let repo_root = std::path::Path::new(&repo_workdir);

    let committed_diff_base = if base_commit == "initial" {
        None
    } else {
        Some(base_commit.as_str())
    };

    let dirty_files = if let Some(snapshot) = carryover_snapshot {
        let mut dirty = snapshot.clone();
        if crate::commands::checkpoint_agent::bash_tool::has_active_bash_inflight(repo_root)
            && let Ok(full_diff) =
                committed_file_snapshot_between_commits(repo, committed_diff_base, &target_commit)
        {
            for (path, content) in full_diff {
                dirty.entry(path).or_insert(content);
            }
        }
        dirty
    } else {
        committed_file_snapshot_between_commits(repo, committed_diff_base, &target_commit)?
    };

    if dirty_files.is_empty() {
        return Ok(());
    }

    // Check working log for AI history — skip if no AI edits and no initial attributions
    let working_log = repo.storage.working_log_for_base_commit(&base_commit)?;
    let has_ai_edits = working_log
        .all_ai_touched_files()
        .map(|files| !files.is_empty())
        .unwrap_or(false);
    let has_initial_attributions = !working_log.read_initial_attributions().files.is_empty();

    // Determine checkpoint kind — check for active bash in-flight
    let (checkpoint_kind, agent_id) =
        match crate::commands::checkpoint_agent::bash_tool::checkpoint_kind_from_active_bash(
            repo_root,
        ) {
            Some(kind) => (kind, None),
            None => {
                if !has_ai_edits && !has_initial_attributions {
                    return Ok(());
                }
                (CheckpointKind::Human, None)
            }
        };

    // Build file entries from dirty_files snapshot
    let repo_work_dir = PathBuf::from(&repo_workdir);
    let files: Vec<CheckpointFileEntry> = dirty_files
        .into_iter()
        .map(|(path, content)| CheckpointFileEntry {
            path: repo_work_dir.join(&path),
            content,
            repo_work_dir: repo_work_dir.clone(),
            base_commit_sha: base_commit.clone(),
        })
        .collect();

    let request = CheckpointRequest {
        trace_id: crate::authorship::authorship_log_serialization::generate_trace_id(),
        checkpoint_kind,
        agent_id,
        files,
        path_role: PreparedPathRole::Edited,
        transcript_source: None,
        metadata: HashMap::new(),
    };

    crate::commands::checkpoint::run(
        repo,
        author,
        checkpoint_kind,
        true,
        Some(request),
    )?;

    Ok(())
}
```

Note: This replaces `checkpoint_context_from_active_bash` (which returned old CheckpointRequest) with a simpler `checkpoint_kind_from_active_bash` that just returns the CheckpointKind. If that function doesn't exist yet, either create a simpler variant in bash_tool.rs, or inline the check (look for any non-stale snapshot file in `.git/ai/bash_snapshots/` → AI kind, else Human).

- [ ] **Step 2: Remove build_human_replay_checkpoint_request**

This function constructed the old CheckpointRequest type. It's no longer needed. Grep and remove:

Run: `grep -rn "build_human_replay_checkpoint_request" src/daemon.rs`

- [ ] **Step 3: Commit**

```bash
git add src/daemon.rs src/commands/checkpoint_agent/bash_tool.rs
git commit -m "refactor: update daemon commit replay to use unified CheckpointRequest

sync_pre_commit_checkpoint_for_daemon_commit now builds CheckpointFileEntry
from committed file snapshots. Pre-commit optimizations moved to pre-filtering.
Removed build_human_replay_checkpoint_request."
```

---

### Task 8: Update checkpoint processing (checkpoint.rs)

**Files:**
- Modify: `src/commands/checkpoint.rs`

This is the largest task. The checkpoint processing functions need to accept the new `CheckpointRequest` where file content is in `request.files[].content` instead of `dirty_files`, and there's no `file_paths` / `repo_working_dir` at the top level.

- [ ] **Step 1: Update the public run function signature**

Remove `is_pre_commit` parameter. The `run` function takes the new `CheckpointRequest`:

```rust
pub fn run(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
) -> Result<(usize, usize, usize), GitAiError> {
```

Remove `run_with_base_commit_override` and `run_with_base_commit_override_with_policy` — the base commit is now per-file in the request. The `run` function extracts the base_commit from the first file entry (all files in a single-repo request share the same base_commit).

- [ ] **Step 2: Rework file content access**

The key change: instead of `dirty_files` being set on the working log and `read_current_file_content` checking dirty_files → filesystem, the content is already in `CheckpointRequest.files[].content`. 

In `save_current_file_states` and wherever file content is read during checkpoint processing, use the content from the request's `files` vec. Build a `HashMap<String, String>` mapping relative path → content from the request files, and set it as dirty_files on the working log (this is the path of least resistance — it reuses the existing `read_current_file_content` infrastructure):

```rust
// At the start of run(), after getting the working_log:
if let Some(ref request) = checkpoint_request {
    let content_map: HashMap<String, String> = request.files.iter()
        .filter_map(|f| {
            let rel_path = f.path.strip_prefix(&f.repo_work_dir)
                .ok()
                .map(|p| normalize_to_posix(&p.to_string_lossy()))?;
            Some((rel_path, f.content.clone()))
        })
        .collect();
    if !content_map.is_empty() {
        working_log.set_dirty_files(Some(content_map));
    }
}
```

This is a transitional approach — it feeds the new type into the existing processing without rewriting the entire attribution engine. The `dirty_files` field on `PersistedWorkingLog` stays for now as an internal implementation detail (it's no longer on the request type, which is what matters).

**Note:** The spec calls for removing `dirty_files` from the working log entirely. This plan keeps it as an internal plumbing detail inside `PersistedWorkingLog` to avoid rewriting the attribution engine (which reads content via `read_current_file_content`). The field is removed from all external-facing types (`CheckpointRequest`, preset events). A follow-up can eliminate the internal usage if desired.

- [ ] **Step 3: Extract file paths from request.files**

Wherever the code accesses `checkpoint_request.file_paths`, change to extract paths from `checkpoint_request.files`:

```rust
// Old:
let file_paths = &checkpoint_request.file_paths;
// New:
let file_paths: Vec<PathBuf> = checkpoint_request.files.iter().map(|f| f.path.clone()).collect();
```

- [ ] **Step 4: Remove is_pre_commit threading**

Remove `is_pre_commit` from all internal function signatures in checkpoint.rs:
- `resolve_live_checkpoint_execution`
- `execute_resolved_checkpoint`
- `get_all_tracked_files`
- `build_checkpoint_entry_for_file`
- `resolve_explicit_path_execution`

In each function, remove all `if is_pre_commit` branches. The optimizations they provided (skip human-only files, preserve unchanged paths) are now handled by pre-filtering in the daemon commit replay (Task 7).

- [ ] **Step 5: Remove captured checkpoint functions**

Delete these functions entirely:
- `prepare_captured_checkpoint` (line ~1029)
- `update_captured_checkpoint_agent_context` (line ~1140)
- `load_captured_checkpoint_manifest` (line ~1175)
- `validate_captured_checkpoint_manifest_repo` (line ~1189)
- `execute_captured_checkpoint` (line ~1211)
- `async_checkpoint_internal_dir` (line ~231)
- `async_checkpoint_storage_dir` (line ~243)
- `async_checkpoint_capture_dir` (line ~247)
- `async_checkpoint_manifest_path` (line ~251)
- `cleanup_failed_captured_checkpoint_prepare` (line ~256)
- `new_async_checkpoint_capture_id` (line ~274)
- `delete_captured_checkpoint` (line ~282)
- `prune_stale_captured_checkpoints` (line ~290)

Delete these types:
- `PreparedCheckpointManifest` (line ~81)
- `PreparedCheckpointCapture` (line ~96)
- `PreparedCheckpointFileSource` (line ~68)
- `PreparedCheckpointFile` (line ~75)
- `BaseOverrideResolutionPolicy` (line ~112)

- [ ] **Step 6: Remove explicit_capture_target_paths and resolve_implicit_path_execution**

Delete `explicit_capture_target_paths` (line ~194) — all checkpoints are scoped now.

Delete `resolve_implicit_path_execution` (the function that handled unscoped checkpoints by scanning git status). All paths come from the request's `files` vec.

- [ ] **Step 7: Commit**

```bash
git add src/commands/checkpoint.rs
git commit -m "refactor: update checkpoint processing for new request type

Content comes from request.files[].content. Removed is_pre_commit threading,
captured checkpoint functions, BaseOverrideResolutionPolicy, unscoped checkpoint
support."
```

---

### Task 9: Delete dead code and files

**Files:**
- Delete: `src/authorship/pre_commit.rs`
- Modify: `src/authorship/mod.rs` (remove `pub mod pre_commit;`)

- [ ] **Step 1: Delete authorship/pre_commit.rs**

```bash
rm src/authorship/pre_commit.rs
```

Edit `src/authorship/mod.rs` to remove the `pub mod pre_commit;` line.

- [ ] **Step 2: Clean up any remaining dead imports and functions**

Run: `task build 2>&1 | head -100`

Fix any remaining compilation errors — likely unused imports, dead code warnings, or mismatched types. Work through them systematically.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "chore: delete dead code from checkpoint rewrite

Removed authorship/pre_commit.rs, unused imports, and dead functions
identified by compiler."
```

---

### Task 10: Fix compilation and run tests

**Files:**
- Various (whatever the compiler complains about)

- [ ] **Step 1: Build and fix all compilation errors**

Run: `task build 2>&1`

Work through every error. Common patterns:
- Old field names referenced somewhere (grep for `file_paths`, `dirty_files`, `captured_checkpoint_id`, `repo_working_dir` in checkpoint contexts)
- Missing imports for new types
- Type mismatches where old `CheckpointRunRequest` was expected
- Removed functions still called somewhere

Keep iterating until `task build` succeeds.

- [ ] **Step 2: Run lint and format**

Run: `task fmt && task lint`

Fix any issues.

- [ ] **Step 3: Run the test suite**

Run: `task test`

Expect some test failures — especially in:
- `tests/integration/checkpoint_unit.rs` (references old types and functions like `explicit_capture_target_paths`)
- `tests/integration/checkpoint_explicit_paths.rs`
- `tests/integration/agent_v1.rs` (references `dirty_files`)
- Any tests that use `prepare_captured_checkpoint` or `execute_captured_checkpoint`

- [ ] **Step 4: Fix failing tests**

For each failing test:
- If it tests removed functionality (captured checkpoints, unscoped checkpoints, explicit_capture_target_paths): delete the test
- If it tests checkpoint behavior that still exists but the API changed: update the test to use the new types
- If it's a snapshot test that changed: run `cargo insta review` and accept if the new output is correct

Key tests to update:
- Tests that construct `CheckpointRequest` manually need to use `files: vec![CheckpointFileEntry { ... }]` instead of `file_paths` + `dirty_files`
- Tests using `TestRepo::git_ai(&["checkpoint", ...])` should still work since the binary interface (preset + file args) is unchanged
- Tests that call `checkpoint::run()` directly need updated signatures

- [ ] **Step 5: Run full test suite again**

Run: `task test`

All tests should pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: update tests for checkpoint rewrite

Fixed compilation errors, updated test assertions for new types,
removed tests for deleted functionality."
```

---

### Task 11: Integration smoke test

**Files:**
- None (manual testing)

- [ ] **Step 1: Install debug build**

Run: `task dev`

- [ ] **Step 2: Test mock_ai scoped checkpoint**

In a test git repo:
```bash
echo "test" > /tmp/test-repo/test.txt
git-ai checkpoint mock_ai /tmp/test-repo/test.txt
```

Expected: "Checkpoint dispatched in <time>" message, no errors.

- [ ] **Step 3: Test mock_known_human checkpoint**

```bash
git-ai checkpoint mock_known_human /tmp/test-repo/test.txt
```

Expected: "Checkpoint dispatched in <time>" message, no errors.

- [ ] **Step 4: Verify attribution after commit**

```bash
cd /tmp/test-repo
git add -A && git commit -m "test"
git-ai blame test.txt
```

Expected: Attribution shows correctly for the file.

- [ ] **Step 5: Run full test suite one final time**

Run: `task test`

All tests should pass.

- [ ] **Step 6: Run lint and format**

Run: `task fmt && task lint`

No issues.
