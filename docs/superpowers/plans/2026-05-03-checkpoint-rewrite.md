# Checkpoint Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the checkpoint system end-to-end so the CLI command is a thin ~50-line dispatcher and all processing happens daemon-side.

**Architecture:** New `CheckpointRequest` with per-file `CheckpointFile` structs carrying content + repo + base_commit. Bash snapshots stored in daemon memory instead of filesystem. No unscoped checkpoints, no captured checkpoint blobs, no sync fallback, no dirty_files. Absolute paths enforced at ingestion.

**Tech Stack:** Rust 2024 edition, serde for serialization, Unix domain sockets for daemon communication.

**Spec:** `docs/superpowers/specs/2026-05-03-checkpoint-rewrite-design.md`

---

## File Map

**New files:** None. All changes are modifications to existing files.

**Core type changes:**
- `src/commands/checkpoint_agent/orchestrator.rs` — `CheckpointRequest` and `CheckpointFile` types, all `execute_*` handlers
- `src/commands/checkpoint_agent/presets/mod.rs` — Remove `BashPreHookStrategy`, `dirty_files` from event structs
- `src/daemon/control_api.rs` — Flatten `CheckpointRun`, add `BashBeginInvocation`/`BashCompleteInvocation`

**CLI rewrite:**
- `src/commands/git_ai_handlers.rs` — Rewrite `handle_checkpoint`, delete ~800 lines

**Daemon adaptation:**
- `src/daemon.rs` — Adapt checkpoint handling, add bash invocation store, move author resolution + metrics here
- `src/daemon/family_actor.rs` — Add bash invocation storage/retrieval messages
- `src/daemon/domain.rs` — Add `BashInvocation` to `FamilyState`

**Checkpoint engine:**
- `src/commands/checkpoint.rs` — Simplify `run()` signature, remove captured checkpoint code, dirty_files branches, is_pre_commit threading

**Bash tool:**
- `src/commands/checkpoint_agent/bash_tool.rs` — Replace filesystem snapshot storage with daemon messages, delete captured checkpoint helpers

**Preset updates:**
- `src/commands/checkpoint_agent/presets/agent_v1.rs` — Remove dirty_files
- `src/commands/checkpoint_agent/presets/known_human.rs` — Remove dirty_files
- `src/commands/checkpoint_agent/presets/codex.rs` — Remove `BashPreHookStrategy`
- All other presets — Remove `dirty_files: None` field

**Deletions:**
- `src/authorship/pre_commit.rs` — Delete entire file

**Test updates:**
- `tests/integration/agent_v1.rs` — Remove dirty_files test cases, update remaining
- `tests/integration/codex.rs` — Update bash checkpoint tests
- `tests/integration/bash_tool_conformance.rs` — Update snapshot storage tests
- `tests/integration/checkpoint_explicit_paths.rs` — Update to new API

---

## Task 1: New Core Types

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs:15-27`
- Modify: `src/daemon/control_api.rs:7-85`
- Modify: `src/daemon/domain.rs:257-271`
- Modify: `src/daemon/family_actor.rs:11-21`

This task defines the new types that everything else builds on. Temporarily keep the old types aliased so the codebase compiles, then subsequent tasks will migrate consumers.

- [ ] **Step 1: Define new `CheckpointFile` and rewrite `CheckpointRequest` in orchestrator.rs**

Replace the existing `CheckpointRequest` (lines 15-27) with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
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
    pub files: Vec<CheckpointFile>,
    pub path_role: PreparedPathRole,
    pub transcript_source: Option<TranscriptSource>,
    pub metadata: HashMap<String, String>,
}
```

Remove these imports that are no longer needed: nothing yet — just the type change for now.

- [ ] **Step 2: Flatten control API types in control_api.rs**

Replace `CheckpointRunRequest` enum (lines 39-84) and `LiveCheckpointRunRequest`/`CapturedCheckpointRunRequest` with a direct reference. The `CheckpointRun` variant becomes:

```rust
#[serde(rename = "checkpoint.run")]
CheckpointRun {
    request: Box<CheckpointRequest>,
},
```

Add new bash invocation messages to the `ControlRequest` enum:

```rust
#[serde(rename = "bash.begin_invocation")]
BashBeginInvocation {
    repo_working_dir: String,
    invocation_id: String,
    agent_context: InflightBashAgentContext,
    stat_snapshot: BashStatSnapshot,
},
#[serde(rename = "bash.complete_invocation")]
BashCompleteInvocation {
    repo_working_dir: String,
    invocation_id: String,
},
```

Delete `CheckpointRunRequest`, `LiveCheckpointRunRequest`, `CapturedCheckpointRunRequest` types entirely. Delete `impl CheckpointRunRequest` block (lines 46-60).

Note: `BashStatSnapshot` is a new serializable subset of `StatSnapshot` — just the `entries`, `effective_worktree_wm`, and `per_file_wm` fields. The `taken_at: Instant` and `invocation_key` fields are not serializable/needed over the wire. Define it in `control_api.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashStatSnapshot {
    pub entries: HashMap<PathBuf, StatEntry>,
    pub repo_root: PathBuf,
    #[serde(default)]
    pub effective_worktree_wm: Option<u128>,
    #[serde(default)]
    pub per_file_wm: HashMap<String, u128>,
}
```

- [ ] **Step 3: Add `BashInvocation` to daemon domain types**

In `src/daemon/domain.rs`, add after `WatermarkState`:

```rust
#[derive(Debug, Clone)]
pub struct BashInvocation {
    pub agent_context: crate::commands::checkpoint_agent::bash_tool::InflightBashAgentContext,
    pub stat_snapshot: crate::daemon::control_api::BashStatSnapshot,
    pub stored_at: std::time::Instant,
}
```

Add to `FamilyState` (line 270, after `watermarks`):

```rust
#[serde(skip)]
pub bash_invocations: HashMap<String, BashInvocation>,
```

- [ ] **Step 4: Add bash invocation messages to `FamilyMsg`**

In `src/daemon/family_actor.rs`, add to the `FamilyMsg` enum:

```rust
StoreBashInvocation(String, BashInvocation),
ConsumeBashInvocation(String, oneshot::Sender<Result<Option<BashInvocation>, GitAiError>>),
GetActiveBashContext(oneshot::Sender<Option<crate::commands::checkpoint_agent::bash_tool::InflightBashAgentContext>>),
EvictStaleBashInvocations,
```

Add handlers in the actor loop:

```rust
FamilyMsg::StoreBashInvocation(id, invocation) => {
    state.bash_invocations.insert(id, invocation);
}
FamilyMsg::ConsumeBashInvocation(id, respond_to) => {
    let _ = respond_to.send(Ok(state.bash_invocations.remove(&id)));
}
FamilyMsg::GetActiveBashContext(respond_to) => {
    let now = std::time::Instant::now();
    let active = state.bash_invocations.values()
        .filter(|inv| now.duration_since(inv.stored_at).as_secs() < 300)
        .max_by_key(|inv| inv.stored_at)
        .map(|inv| inv.agent_context.clone());
    let _ = respond_to.send(active);
}
FamilyMsg::EvictStaleBashInvocations => {
    let now = std::time::Instant::now();
    state.bash_invocations.retain(|_, inv| {
        now.duration_since(inv.stored_at).as_secs() < 300
    });
}
```

Add corresponding methods to `FamilyActorHandle`:

```rust
pub async fn store_bash_invocation(&self, id: String, invocation: BashInvocation) {
    let _ = self.tx.send(FamilyMsg::StoreBashInvocation(id, invocation)).await;
}

pub async fn consume_bash_invocation(&self, id: String) -> Result<Option<BashInvocation>, GitAiError> {
    let (tx, rx) = oneshot::channel();
    self.tx.send(FamilyMsg::ConsumeBashInvocation(id, tx)).await
        .map_err(|_| GitAiError::Generic("family actor consume bash send failed".to_string()))?;
    rx.await
        .map_err(|_| GitAiError::Generic("family actor consume bash receive failed".to_string()))?
}

pub async fn get_active_bash_context(&self) -> Option<InflightBashAgentContext> {
    let (tx, rx) = oneshot::channel();
    let _ = self.tx.send(FamilyMsg::GetActiveBashContext(tx)).await;
    rx.await.ok().flatten()
}

pub async fn evict_stale_bash_invocations(&self) {
    let _ = self.tx.send(FamilyMsg::EvictStaleBashInvocations).await;
}
```

- [ ] **Step 5: Build and fix compilation errors**

Run: `task build`

Expected: Compilation errors from every consumer of the old `CheckpointRequest` fields (`file_paths`, `dirty_files`, `captured_checkpoint_id`, `repo_working_dir`) and every consumer of `CheckpointRunRequest`/`LiveCheckpointRunRequest`. This is expected — the subsequent tasks fix each consumer. For now, just verify the new type definitions themselves compile cleanly by temporarily adding `#[allow(dead_code)]` if needed, or by commenting out broken downstream code with `todo!()` markers.

Actually — don't do that. Just build and note what breaks. The plan proceeds in dependency order to fix each.

- [ ] **Step 6: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs src/daemon/control_api.rs src/daemon/domain.rs src/daemon/family_actor.rs
git commit -m "feat: define new CheckpointRequest/CheckpointFile types and bash invocation daemon storage"
```

---

## Task 2: Remove `dirty_files` from Preset Event Types

**Files:**
- Modify: `src/commands/checkpoint_agent/presets/mod.rs:53-95`
- Modify: `src/commands/checkpoint_agent/presets/agent_v1.rs`
- Modify: `src/commands/checkpoint_agent/presets/known_human.rs`
- Modify: `src/commands/checkpoint_agent/presets/claude.rs`
- Modify: `src/commands/checkpoint_agent/presets/codex.rs`
- Modify: `src/commands/checkpoint_agent/presets/continue_cli.rs`
- Modify: `src/commands/checkpoint_agent/presets/gemini.rs`
- Modify: `src/commands/checkpoint_agent/presets/amp.rs`

- [ ] **Step 1: Remove `dirty_files` from `PreFileEdit`, `PostFileEdit`, `KnownHumanEdit`**

In `src/commands/checkpoint_agent/presets/mod.rs`:

Remove `pub dirty_files: Option<HashMap<PathBuf, String>>` from:
- `PreFileEdit` (line 56)
- `PostFileEdit` (line 63)
- `KnownHumanEdit` (line 72)

Remove `BashPreHookStrategy` enum (lines 24-27) and the `strategy` field from `PreBashCall` (line 87).

- [ ] **Step 2: Update all presets to remove `dirty_files` and `strategy` fields**

In each preset file, remove every `dirty_files: None` or `dirty_files: <expr>` field from struct construction:

- `claude.rs`: Lines 103, 113
- `codex.rs`: Lines 141 (strategy), 147, 175
- `continue_cli.rs`: Lines 57 (strategy), 62, 72
- `gemini.rs`: Lines 63 (strategy), 68, 78
- `amp.rs`: Lines 334 (strategy), 339, 349
- `agent_v1.rs`: Lines 43-66, 75-98 — remove dirty_files parsing and field. The hook input may still include dirty_files JSON but the preset just ignores it now.
- `known_human.rs`: Lines 47-59 — remove dirty_files parsing and field.

- [ ] **Step 3: Update preset tests**

Remove `dirty_files` assertions from preset unit tests:
- `claude.rs` test: Line 156 (`assert!(e.dirty_files.is_none())`)
- `gemini.rs` test: Line 121
- `agent_v1.rs` tests: Lines 120, 136, 182
- `codex.rs` test: Line 223 (`assert_eq!(e.strategy, ...)`)
- `amp.rs` test: Line 431
- `continue_cli.rs` test: Line 147

- [ ] **Step 4: Build and verify**

Run: `task build`

Expected: Compile errors in orchestrator.rs `execute_*` functions that still reference `dirty_files` and `strategy`. That's expected — Task 3 fixes those.

- [ ] **Step 5: Commit**

```bash
git add src/commands/checkpoint_agent/presets/
git commit -m "refactor: remove dirty_files from preset event types and BashPreHookStrategy enum"
```

---

## Task 3: Rewrite Orchestrator `execute_*` Handlers

**Files:**
- Modify: `src/commands/checkpoint_agent/orchestrator.rs`

Each handler now resolves per-file metadata (repo, base_commit, content) and builds `CheckpointFile` structs.

- [ ] **Step 1: Add helper functions for repo resolution and file content capture**

At the top of orchestrator.rs, add:

```rust
use crate::git::repository::Repository;
use std::fs;

fn rev_parse_head(repo: &Repository) -> Result<String, GitAiError> {
    repo.head_commit_sha()
        .map_err(|e| GitAiError::Generic(format!("Failed to resolve HEAD: {}", e)))
}

fn read_file_content(path: &Path) -> Result<String, GitAiError> {
    fs::read_to_string(path)
        .map_err(|e| GitAiError::Generic(format!("Failed to read {}: {}", path.display(), e)))
}

fn validate_absolute_paths(paths: &[PathBuf]) -> Result<(), GitAiError> {
    for path in paths {
        if !path.is_absolute() {
            return Err(GitAiError::Generic(format!(
                "Checkpoint requires absolute file paths, got relative: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn build_checkpoint_files(
    file_paths: &[PathBuf],
    repo: &Repository,
) -> Result<Vec<CheckpointFile>, GitAiError> {
    validate_absolute_paths(file_paths)?;
    let repo_work_dir = repo.workdir()?;
    let base_commit_sha = rev_parse_head(repo)?;
    let mut files = Vec::with_capacity(file_paths.len());
    for path in file_paths {
        let content = if path.exists() {
            read_file_content(path).unwrap_or_default()
        } else {
            String::new()
        };
        files.push(CheckpointFile {
            path: path.clone(),
            content,
            repo_work_dir: repo_work_dir.clone(),
            base_commit_sha: base_commit_sha.clone(),
        });
    }
    Ok(files)
}
```

Note: Check that `Repository::head_commit_sha()` exists — if not, the implementation should use `repo.rev_parse_head()` or whichever method the `Repository` type provides. Grep for `head_commit\|rev_parse\|HEAD` in `src/git/repository.rs` to find the right method.

- [ ] **Step 2: Rewrite `execute_pre_file_edit`**

```rust
fn execute_pre_file_edit(e: PreFileEdit) -> Result<CheckpointRequest, GitAiError> {
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.context.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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

Rename existing `resolve_repo_working_dir_from_file_paths` → `find_repository_for_file_paths` (return `Repository` not `PathBuf`). Same for `resolve_repo_working_dir_from_cwd` → `find_repository_for_cwd`.

- [ ] **Step 3: Rewrite `execute_post_file_edit`**

```rust
fn execute_post_file_edit(
    e: PostFileEdit,
    preset_name: &str,
) -> Result<CheckpointRequest, GitAiError> {
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.context.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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

- [ ] **Step 4: Rewrite `execute_known_human_edit`**

```rust
fn execute_known_human_edit(e: KnownHumanEdit) -> Result<CheckpointRequest, GitAiError> {
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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

- [ ] **Step 5: Rewrite `execute_untracked_edit`**

```rust
fn execute_untracked_edit(e: UntrackedEdit) -> Result<CheckpointRequest, GitAiError> {
    let repo = if !e.file_paths.is_empty() {
        find_repository_for_file_paths(&e.file_paths)?
    } else {
        find_repository_for_cwd(&e.cwd)?
    };
    let files = build_checkpoint_files(&e.file_paths, &repo)?;

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

- [ ] **Step 6: Rewrite `execute_pre_bash_call`**

The strategy field is gone. The pre-hook always does the full flow: query watermarks, take snapshot, store in daemon, find stale files, return checkpoint if any.

```rust
fn execute_pre_bash_call(e: PreBashCall) -> Result<Option<CheckpointRequest>, GitAiError> {
    let repo = find_repository_for_cwd(&e.context.cwd)?;
    let repo_work_dir = repo.workdir()?;
    let repo_working_dir_str = repo_work_dir.to_string_lossy().to_string();

    // 1. Query daemon for watermarks
    let watermarks = bash_tool::query_daemon_watermarks_pub(&repo_working_dir_str);

    // 2. Take stat snapshot locally
    let snap = bash_tool::snapshot(
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
        watermarks.as_ref(),
    )?;

    // 3. Store snapshot + agent context in daemon
    bash_tool::store_invocation_in_daemon(
        &repo_working_dir_str,
        &format!("{}:{}", e.context.session_id, e.tool_use_id),
        &e.context.agent_id,
        e.context.metadata.as_ref(),
        &snap,
    );

    // 4. Find stale files and build checkpoint if any
    let stale_files = bash_tool::find_stale_files(&snap);
    if stale_files.is_empty() {
        return Ok(None);
    }

    let base_commit_sha = rev_parse_head(&repo)?;
    let files: Vec<CheckpointFile> = stale_files
        .iter()
        .filter_map(|rel_path| {
            let abs_path = repo_work_dir.join(rel_path);
            let content = fs::read_to_string(&abs_path).ok()?;
            Some(CheckpointFile {
                path: abs_path,
                content,
                repo_work_dir: repo_work_dir.clone(),
                base_commit_sha: base_commit_sha.clone(),
            })
        })
        .collect();

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
```

Note: The exact function names (`query_daemon_watermarks_pub`, `store_invocation_in_daemon`, `find_stale_files` as public) will need to be adjusted based on what's already public in bash_tool.rs vs what needs to be exposed. Check during implementation.

- [ ] **Step 7: Rewrite `execute_post_bash_call`**

```rust
fn execute_post_bash_call(e: PostBashCall) -> Result<CheckpointRequest, GitAiError> {
    let repo = find_repository_for_cwd(&e.context.cwd)?;
    let repo_work_dir = repo.workdir()?;
    let repo_working_dir_str = repo_work_dir.to_string_lossy().to_string();
    let invocation_id = format!("{}:{}", e.context.session_id, e.tool_use_id);

    // 1. Retrieve stored snapshot from daemon (consumes it)
    let pre_snap = bash_tool::retrieve_invocation_from_daemon(
        &repo_working_dir_str,
        &invocation_id,
    );

    // 2. Take new stat snapshot
    let post_snap = bash_tool::snapshot(
        &repo_work_dir,
        &e.context.session_id,
        &e.tool_use_id,
        None, // no watermark filtering needed for post
    )?;

    // 3. Diff
    let changed_paths = match pre_snap {
        Some(pre) => {
            let diff = bash_tool::diff(&pre, &post_snap);
            diff.all_changed_paths()
        }
        None => vec![], // No pre-snapshot = no changes detectable
    };

    // 4. Build checkpoint files from changed paths
    let base_commit_sha = rev_parse_head(&repo)?;
    let files: Vec<CheckpointFile> = changed_paths
        .iter()
        .filter_map(|rel_path| {
            let abs_path = repo_work_dir.join(rel_path);
            let content = fs::read_to_string(&abs_path).ok()?;
            Some(CheckpointFile {
                path: abs_path,
                content,
                repo_work_dir: repo_work_dir.clone(),
                base_commit_sha: base_commit_sha.clone(),
            })
        })
        .collect();

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

- [ ] **Step 8: Build and verify**

Run: `task build`

Fix any remaining compilation issues in orchestrator.rs. The downstream consumers (handle_checkpoint, daemon) will still be broken — that's expected.

- [ ] **Step 9: Commit**

```bash
git add src/commands/checkpoint_agent/orchestrator.rs
git commit -m "refactor: rewrite orchestrator handlers to produce CheckpointFile with content/repo/base_commit"
```

---

## Task 4: Rewrite `handle_checkpoint` as Thin Dispatcher

**Files:**
- Modify: `src/commands/git_ai_handlers.rs:294-877` (rewrite), `886-1211` (delete)

- [ ] **Step 1: Rewrite `handle_checkpoint` to ~50 lines**

Replace the entire `handle_checkpoint` function (lines 294-877) with:

```rust
fn handle_checkpoint(args: &[String]) {
    // 1. Parse hook input
    let mut hook_input = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--hook-input" && i + 1 < args.len() {
            hook_input = Some(strip_utf8_bom(args[i + 1].clone()));
            if hook_input.as_ref().unwrap() == "stdin" {
                let mut buffer = String::new();
                if let Err(e) = std::io::stdin().read_to_string(&mut buffer) {
                    eprintln!("Failed to read stdin for hook input: {}", e);
                    std::process::exit(1);
                }
                hook_input = Some(strip_utf8_bom(buffer));
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    // 2. Detect preset and synthesize hook input if needed
    if args.is_empty()
        || crate::commands::checkpoint_agent::presets::resolve_preset(args[0].as_str()).is_err()
    {
        eprintln!("No valid preset specified");
        std::process::exit(1);
    }
    let preset_name = args[0].as_str();
    let effective_hook_input = hook_input.unwrap_or_else(|| {
        synthesize_hook_input_from_cli_args(preset_name, &args[1..])
    });

    // 3. Run orchestrator
    let requests = match crate::commands::checkpoint_agent::orchestrator::execute_preset_checkpoint(
        preset_name,
        &effective_hook_input,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} preset error: {}", preset_name, e);
            std::process::exit(1);
        }
    };

    if requests.is_empty() {
        std::process::exit(0);
    }

    // 4. Validate absolute paths
    for req in &requests {
        for file in &req.files {
            if !file.path.is_absolute() {
                eprintln!("Fatal: checkpoint file path is not absolute: {}", file.path.display());
                std::process::exit(1);
            }
        }
    }

    // 5. Send to daemon
    let checkpoint_start = std::time::Instant::now();
    let daemon_config = match crate::commands::daemon::ensure_daemon_running(
        std::time::Duration::from_secs(5),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[git-ai] checkpoint failed: daemon unavailable: {}", e);
            std::process::exit(1);
        }
    };

    for request in requests {
        let control_request = ControlRequest::CheckpointRun {
            request: Box::new(request),
        };
        match send_control_request(&daemon_config.control_socket_path, &control_request) {
            Ok(response) if response.ok => {}
            Ok(response) => {
                let msg = response.error.unwrap_or_else(|| "unknown".to_string());
                eprintln!("[git-ai] checkpoint rejected by daemon: {}", msg);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("[git-ai] checkpoint send failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    eprintln!("Checkpoint dispatched in {:?}", checkpoint_start.elapsed());
}
```

- [ ] **Step 2: Update `synthesize_hook_input_from_cli_args` to make paths absolute**

In the existing function (lines 1719-1785), for the `"human" | "mock_ai" | "mock_known_human"` arm, convert relative paths to absolute:

```rust
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
```

Do the same for the `"known_human"` arm's `files` vector.

- [ ] **Step 3: Delete dead code from git_ai_handlers.rs**

Delete the following functions entirely:
- `run_checkpoint_via_daemon_or_local` (lines 886-1125)
- `checkpoint_request_has_explicit_capture_scope` (lines 1127-1155)
- `cleanup_captured_checkpoint_after_delegate_failure` (lines 1157-1178)
- `log_daemon_checkpoint_delegate_failure` (lines 1180-1211)
- `estimate_checkpoint_file_count` (grep for it)
- `get_all_files_for_mock_ai` (lines 1702-1715)
- `checkpoint_kind_to_str` (lines 1204-1211) — only if no other callers remain
- `log_performance_for_checkpoint` — only if no other callers remain
- `CheckpointDispatchOutcome` struct (lines 879-883)

- [ ] **Step 4: Build and verify**

Run: `task build`

Expected: Checkpoint command path compiles. Daemon-side will have errors from missing old types — that's Task 5.

- [ ] **Step 5: Commit**

```bash
git add src/commands/git_ai_handlers.rs
git commit -m "feat: rewrite handle_checkpoint as thin ~50-line dispatcher"
```

---

## Task 5: Adapt Daemon Checkpoint Reception

**Files:**
- Modify: `src/daemon.rs` — `handle_control_request`, `apply_checkpoint_side_effect`, `drain_ready_family_sequencer_entries_locked`, `sync_pre_commit_checkpoint_for_daemon_commit`

- [ ] **Step 1: Update `handle_control_request` for new `CheckpointRun` shape**

In the `ControlRequest::CheckpointRun` match arm (around line 7271), the request is now directly a `Box<CheckpointRequest>` — no `Live`/`Captured` dispatch. Update to extract transcript info and checkpoint data from the new type.

Add handlers for the new bash invocation messages:

```rust
ControlRequest::BashBeginInvocation {
    repo_working_dir,
    invocation_id,
    agent_context,
    stat_snapshot,
} => {
    // Get or create family for this repo, store invocation
    let invocation = BashInvocation {
        agent_context,
        stat_snapshot,
        stored_at: std::time::Instant::now(),
    };
    // Route to family actor via coordinator
    match self.coordinator.store_bash_invocation_family(
        Path::new(&repo_working_dir),
        invocation_id,
        invocation,
    ).await {
        Ok(()) => ControlResponse::ok(None, None),
        Err(e) => ControlResponse::err(e.to_string()),
    }
}
ControlRequest::BashCompleteInvocation {
    repo_working_dir,
    invocation_id,
} => {
    match self.coordinator.consume_bash_invocation_family(
        Path::new(&repo_working_dir),
        invocation_id,
    ).await {
        Ok(Some(inv)) => {
            let data = serde_json::json!({
                "agent_context": inv.agent_context,
                "stat_snapshot": inv.stat_snapshot,
            });
            ControlResponse::ok(None, Some(data))
        }
        Ok(None) => ControlResponse::ok(None, None),
        Err(e) => ControlResponse::err(e.to_string()),
    }
}
```

Add corresponding methods to the coordinator (in `src/daemon/coordinator.rs`).

- [ ] **Step 2: Rewrite `apply_checkpoint_side_effect`**

The function (lines 1354-1393) currently matches on `Live`/`Captured`. Rewrite to receive `CheckpointRequest` directly:

```rust
fn apply_checkpoint_side_effect(
    request: &CheckpointRequest,
) -> Result<(usize, usize, usize), GitAiError> {
    // Group files by repo_work_dir
    let mut repos: HashMap<PathBuf, Vec<&CheckpointFile>> = HashMap::new();
    for file in &request.files {
        repos.entry(file.repo_work_dir.clone()).or_default().push(file);
    }

    let mut total = (0usize, 0usize, 0usize);
    for (repo_work_dir, files) in repos {
        let repo = find_repository_in_path(&repo_work_dir.to_string_lossy())?;
        let author = repo.git_author_identity().formatted_or_unknown();

        let stats = crate::commands::checkpoint::run(
            &repo,
            &author,
            request,
            &files,
        )?;
        total.0 += stats.0;
        total.1 += stats.1;
        total.2 += stats.2;
    }
    Ok(total)
}
```

Note: The exact `checkpoint::run()` signature change is in Task 6. For now, make this compile against what will exist.

- [ ] **Step 3: Update `drain_ready_family_sequencer_entries_locked` checkpoint section**

Update the checkpoint handling section (lines 5645-5847) to work with the new `CheckpointRequest` type:
- Remove `captured_checkpoint_id` extraction (line 5650-5651)
- Remove `CheckpointRunRequest::Captured` handling (lines 5666-5673)
- Extract file paths directly from `request.files` instead of `checkpoint_request.file_paths`
- Watermark computation uses the file paths from `request.files`
- Remove `is_live_human_checkpoint` distinction — all checkpoints are "live" now

- [ ] **Step 4: Update `sync_pre_commit_checkpoint_for_daemon_commit`**

In lines 2500-2618, replace the `checkpoint_context_from_active_bash` filesystem scan with a daemon-side query:

```rust
// Instead of:
// checkpoint_context_from_active_bash(repo_root, &repo_working_dir)
// Use:
// Query the family's in-memory bash invocations
let active_bash_context = coordinator
    .get_active_bash_context_family(Path::new(&repo_working_dir))
    .await;
```

Build the checkpoint request using the new types. The committed files already come from the rewrite event diff — just wrap them as `CheckpointFile` structs.

- [ ] **Step 5: Move AgentUsage metric emission to daemon**

After successful checkpoint processing in `drain_ready_family_sequencer_entries_locked`, emit the AgentUsage metric that was previously in `handle_checkpoint` CLI-side. Use the `agent_id` from the `CheckpointRequest`.

- [ ] **Step 6: Build and verify**

Run: `task build`

- [ ] **Step 7: Commit**

```bash
git add src/daemon.rs src/daemon/coordinator.rs
git commit -m "feat: adapt daemon to receive CheckpointRequest directly, add bash invocation store"
```

---

## Task 6: Simplify `checkpoint.rs` Processing Engine

**Files:**
- Modify: `src/commands/checkpoint.rs`

- [ ] **Step 1: Simplify `run()` signature**

Replace the current signature (lines 322-329):

```rust
pub fn run(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    quiet: bool,
    checkpoint_request: Option<CheckpointRequest>,
    is_pre_commit: bool,
) -> Result<(usize, usize, usize), GitAiError>
```

With:

```rust
pub fn run(
    repo: &Repository,
    author: &str,
    request: &CheckpointRequest,
    files: &[&CheckpointFile],
) -> Result<(usize, usize, usize), GitAiError>
```

The `kind`, `quiet`, `checkpoint_request`, and `is_pre_commit` params are gone — `kind` comes from `request.checkpoint_kind`, content comes from `CheckpointFile.content`, `base_commit` from `CheckpointFile.base_commit_sha`.

- [ ] **Step 2: Collapse resolution paths**

Delete:
- `resolve_base_override_dirty_file_execution` (lines 450-494)
- `resolve_explicit_path_execution`'s dirty_files branches
- `resolve_live_checkpoint_execution`'s dirty_files extraction and branching

Replace with a single resolution that takes the files directly from the `CheckpointFile` slice:

```rust
fn resolve_checkpoint_execution(
    repo: &Repository,
    request: &CheckpointRequest,
    files: &[&CheckpointFile],
) -> Result<ResolvedCheckpointExecution, GitAiError> {
    let base_commit = files.first()
        .map(|f| f.base_commit_sha.as_str())
        .unwrap_or("HEAD");

    let file_paths: Vec<String> = files.iter()
        .map(|f| {
            // Convert absolute path to repo-relative
            let rel = f.path.strip_prefix(&f.repo_work_dir)
                .unwrap_or(&f.path);
            normalize_to_posix(&rel.to_string_lossy())
        })
        .collect();

    let dirty_file_contents: HashMap<String, String> = files.iter()
        .map(|f| {
            let rel = f.path.strip_prefix(&f.repo_work_dir)
                .unwrap_or(&f.path);
            (normalize_to_posix(&rel.to_string_lossy()), f.content.clone())
        })
        .collect();

    // Load working log, compute file hashes, etc.
    // ... (reuse existing working log loading code)

    Ok(ResolvedCheckpointExecution {
        base_commit: base_commit.to_string(),
        ts: /* timestamp */,
        files: file_paths,
        dirty_files: dirty_file_contents,
        // ... remaining fields
    })
}
```

Note: The `ResolvedCheckpointExecution` struct's `dirty_files` field is still used internally by the attribution engine — it represents "the current content of these files." But it's always populated from `CheckpointFile.content` now, never from a separate `dirty_files` HashMap or disk read.

- [ ] **Step 3: Delete captured checkpoint code**

Delete these functions entirely:
- `prepare_captured_checkpoint` (lines 1029-1135)
- `execute_captured_checkpoint` (lines 1211-1262)
- `delete_captured_checkpoint` (lines 282-288)
- `load_captured_checkpoint_manifest` (lines 1175-1187)
- `update_captured_checkpoint_agent_context` (grep for it)
- `explicit_capture_target_paths` (lines 194-216)
- `prune_stale_captured_checkpoints` (lines 290-319)

Delete these types:
- `PreparedCheckpointManifest` (lines 81-94)
- `PreparedCheckpointFile` (lines 75-79)
- `PreparedCheckpointFileSource` (lines 68-73)
- `PreparedCheckpointCapture` (lines 96-101)

Keep `PreparedPathRole` — it's still used in the new `CheckpointRequest`.

- [ ] **Step 4: Remove `is_pre_commit` threading**

Remove `is_pre_commit` parameter from:
- `run_with_base_commit_override` (line 348)
- `run_with_base_commit_override_with_policy` (line 371)
- `resolve_live_checkpoint_execution` (line 613)
- `resolve_explicit_path_execution` (line 517)
- `get_checkpoint_entries` (line 1909)
- `get_checkpoint_entry_for_file` (line 1651)

The `is_pre_commit` conditionals at lines 554, 637, 1432, 1679 need to be evaluated:
- Line 554: `preserve_unchanged_explicit_paths = kind == Human && is_pre_commit` — this preserved files even if unchanged during pre-commit. With no pre-commit checkpoint path, this can be simplified. Check if this logic is still needed.
- Line 637: `if is_pre_commit && base_commit_override.is_none()` — skips if no AI edits in pre-commit. Dead path.
- Line 1432: `if is_pre_commit && !has_ai_checkpoints` — conditional skip. Dead path.
- Line 1679: `if is_pre_commit && !kind.is_ai()` — early return for non-AI pre-commit. Dead path.

- [ ] **Step 5: Build and verify**

Run: `task build`

- [ ] **Step 6: Commit**

```bash
git add src/commands/checkpoint.rs
git commit -m "refactor: simplify checkpoint.rs - collapse resolution paths, delete captured checkpoint code"
```

---

## Task 7: Update Bash Tool to Use Daemon Storage

**Files:**
- Modify: `src/commands/checkpoint_agent/bash_tool.rs`

- [ ] **Step 1: Add daemon communication functions**

Add public functions that the orchestrator will call:

```rust
pub fn store_invocation_in_daemon(
    repo_working_dir: &str,
    invocation_id: &str,
    agent_id: &AgentId,
    agent_metadata: Option<&HashMap<String, String>>,
    snap: &StatSnapshot,
) {
    let config = match DaemonConfig::from_env_or_default_paths() {
        Ok(c) => c,
        Err(_) => return,
    };
    if !config.control_socket_path.exists() {
        return;
    }

    let stat_snapshot = BashStatSnapshot {
        entries: snap.entries.clone(),
        repo_root: snap.repo_root.clone(),
        effective_worktree_wm: snap.effective_worktree_wm,
        per_file_wm: snap.per_file_wm.clone(),
    };

    let request = ControlRequest::BashBeginInvocation {
        repo_working_dir: repo_working_dir.to_string(),
        invocation_id: invocation_id.to_string(),
        agent_context: InflightBashAgentContext {
            session_id: /* extract from invocation_id */,
            tool_use_id: /* extract from invocation_id */,
            agent_id: agent_id.clone(),
            agent_metadata: agent_metadata.cloned(),
        },
        stat_snapshot,
    };

    let _ = send_control_request_with_timeout(
        &config.control_socket_path,
        &request,
        Duration::from_millis(500),
    );
}

pub fn retrieve_invocation_from_daemon(
    repo_working_dir: &str,
    invocation_id: &str,
) -> Option<(InflightBashAgentContext, BashStatSnapshot)> {
    let config = DaemonConfig::from_env_or_default_paths().ok()?;
    if !config.control_socket_path.exists() {
        return None;
    }

    let request = ControlRequest::BashCompleteInvocation {
        repo_working_dir: repo_working_dir.to_string(),
        invocation_id: invocation_id.to_string(),
    };

    let response = send_control_request_with_timeout(
        &config.control_socket_path,
        &request,
        Duration::from_millis(500),
    ).ok()?;

    if !response.ok {
        return None;
    }

    let data = response.data?;
    let agent_context: InflightBashAgentContext =
        serde_json::from_value(data.get("agent_context")?.clone()).ok()?;
    let stat_snapshot: BashStatSnapshot =
        serde_json::from_value(data.get("stat_snapshot")?.clone()).ok()?;
    Some((agent_context, stat_snapshot))
}
```

- [ ] **Step 2: Make `find_stale_files` and `query_daemon_watermarks` public**

Change visibility of:
- `find_stale_files` (line 1108) — `pub fn find_stale_files`
- `query_daemon_watermarks` (line 1059) — rename to `pub fn query_daemon_watermarks` or create a public wrapper

- [ ] **Step 3: Delete filesystem snapshot functions**

Delete:
- `save_snapshot` (lines 839-855)
- `load_and_consume_snapshot` (lines 858-887)
- `cleanup_stale_snapshots` (lines 890-910)
- `snapshot_cache_dir` (lines 832-837)
- `sanitize_key` (lines 913-921)
- `cache_entry_is_fresh` (grep for it)

- [ ] **Step 4: Delete captured checkpoint helpers**

Delete:
- `attempt_pre_hook_capture` (lines 1149-1234)
- `attempt_post_hook_capture` (lines 1244+)
- `CapturedCheckpointInfo` struct (lines 288-291)

- [ ] **Step 5: Delete `scan_active_bash_snapshots` and `checkpoint_context_from_active_bash`**

Delete:
- `scan_active_bash_snapshots` (lines 319-371)
- `checkpoint_context_from_active_bash` (lines 373-409)
- `ActiveBashSnapshotScan` struct (lines 282-285)
- `has_active_bash_inflight` (lines 1353-1355)

- [ ] **Step 6: Simplify `handle_bash_pre_tool_use_with_context` and `handle_bash_tool`**

These functions currently manage filesystem snapshots and captured checkpoints. Simplify them:
- `handle_bash_pre_tool_use_with_context`: Remove `save_snapshot` call, remove `attempt_pre_hook_capture` call. The orchestrator now handles daemon storage and stale file detection.
- `handle_bash_tool`: Remove `load_and_consume_snapshot` call, remove `attempt_post_hook_capture` call. The orchestrator handles daemon retrieval and diff.

Actually — with the orchestrator handling the full bash flow (query watermarks, snapshot, daemon store, stale detection, diff), these functions may become thin enough to inline into the orchestrator or become simple wrappers. Evaluate during implementation whether to keep them or fold their core logic (stat walk, diff) directly into the orchestrator handlers.

- [ ] **Step 7: Build and verify**

Run: `task build`

- [ ] **Step 8: Commit**

```bash
git add src/commands/checkpoint_agent/bash_tool.rs
git commit -m "refactor: replace filesystem bash snapshots with daemon in-memory storage"
```

---

## Task 8: Delete Dead Code and Modules

**Files:**
- Delete: `src/authorship/pre_commit.rs`
- Modify: `src/authorship/mod.rs` — remove `pub mod pre_commit;`
- Modify: `src/commands/checkpoint.rs` — remove any remaining references to deleted types
- Modify: `src/daemon.rs` — remove `prune_stale_captured_checkpoints` calls, captured checkpoint handling

- [ ] **Step 1: Delete `src/authorship/pre_commit.rs`**

Delete the file entirely. Remove `pub mod pre_commit;` from `src/authorship/mod.rs` (line 12).

- [ ] **Step 2: Remove captured checkpoint pruning from daemon**

Grep for `prune_stale_captured_checkpoints` in daemon.rs and remove all calls to it.

- [ ] **Step 3: Remove captured checkpoint manifest loading from daemon**

In `drain_ready_family_sequencer_entries_locked` (daemon.rs), remove the `Captured` branch that calls `load_captured_checkpoint_manifest` (around lines 5666-5673).

- [ ] **Step 4: Clean up any remaining references to deleted types**

Run: `task build`

Fix any remaining compilation errors from references to:
- `CheckpointRunRequest`
- `LiveCheckpointRunRequest`
- `CapturedCheckpointRunRequest`
- `PreparedCheckpointManifest`
- `CapturedCheckpointInfo`
- `dirty_files` (as a field name on old types)
- `captured_checkpoint_id`
- `checkpoint_context_from_active_bash`
- `has_active_bash_inflight`

- [ ] **Step 5: Build clean**

Run: `task build`

Expected: Clean compilation.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: delete dead code - pre_commit module, captured checkpoint ceremony, filesystem snapshots"
```

---

## Task 9: Update Integration Tests

**Files:**
- Modify: `tests/integration/agent_v1.rs`
- Modify: `tests/integration/codex.rs`
- Modify: `tests/integration/bash_tool_conformance.rs`
- Modify: `tests/integration/checkpoint_explicit_paths.rs`
- Modify: Other test files as needed

- [ ] **Step 1: Fix agent_v1 tests**

The `agent_v1` preset no longer accepts `dirty_files` in its hook input. Update or remove:
- `test_agent_v1_human_checkpoint_with_dirty_files` — remove or convert to test without dirty_files
- `test_agent_v1_ai_agent_checkpoint_with_dirty_files` — same
- `test_agent_v1_dirty_files_multiple_files` — remove
- Keep `test_agent_v1_human_checkpoint_without_dirty_files` and `test_agent_v1_ai_agent_checkpoint_without_dirty_files` — these should still work

- [ ] **Step 2: Fix codex bash tests**

Update codex tests that exercise the bash checkpoint flow:
- `test_codex_preset_bash_pre_tool_use_skips_checkpoint_after_capturing_snapshot` — the "skips" behavior is gone (strategy removed). Update or remove.
- `test_codex_e2e_bash_pre_and_post_tool_use_full_cycle` — update to work with daemon-based snapshot storage
- `test_codex_commit_inside_bash_inflight_*` — update to use daemon-based bash context detection

These tests use the `TestRepo` harness which spawns a daemon. The daemon communication should work, but the tests may need updated assertions since the flow changed.

- [ ] **Step 3: Fix bash_tool_conformance tests**

Many of these test the stat-diff logic directly (which is unchanged) so they should mostly work. Update tests that reference:
- `save_snapshot` / `load_and_consume_snapshot` → these functions are gone
- `BashCheckpointAction::Fallback` → may need removal if fallback is gone
- `handle_bash_tool` / `handle_bash_pre_tool_use_with_context` → if signatures changed

- [ ] **Step 4: Fix checkpoint_explicit_paths tests**

These test explicit-path checkpoints. They use `TestRepo::git_ai(&["checkpoint", "mock_ai", "file.txt"])` which goes through `synthesize_hook_input_from_cli_args`. Paths will now be absolute. Verify these still work since synthesize now makes paths absolute.

- [ ] **Step 5: Run full test suite**

Run: `task test`

Fix any remaining test failures. The test harness uses `GIT_AI=git` env var and spawns daemon processes, so daemon communication should work end-to-end.

- [ ] **Step 6: Run lint and format**

Run: `task lint && task fmt`

- [ ] **Step 7: Commit**

```bash
git add tests/
git commit -m "test: update integration tests for checkpoint rewrite"
```

---

## Task 10: Final Verification and Cleanup

**Files:** Various

- [ ] **Step 1: Run full test suite in daemon mode**

Run: `task test`

All tests must pass.

- [ ] **Step 2: Run full test suite in wrapper-daemon mode**

Run: `task test:wrapper-daemon`

All tests must pass.

- [ ] **Step 3: Grep for orphaned references**

```bash
grep -rn "dirty_files\|captured_checkpoint_id\|captured_checkpoint\|CapturedCheckpoint\|PreparedCheckpoint\|LiveCheckpointRunRequest\|CapturedCheckpointRunRequest\|CheckpointRunRequest\|BashPreHookStrategy\|is_pre_commit\|pre_commit\b" src/ --include="*.rs" | grep -v "//\|test\|#\[cfg"
```

Verify all results are either in test code, comments, or false positives. Remove any remaining dead references.

- [ ] **Step 4: Run lint and format**

Run: `task lint && task fmt`

- [ ] **Step 5: Final commit if any cleanup was needed**

```bash
git add -A
git commit -m "chore: final cleanup for checkpoint rewrite"
```

- [ ] **Step 6: Verify `task dev` works end-to-end**

Run: `task dev`

This installs the debug build system-wide. Create a test repo and manually verify:
- `git-ai checkpoint mock_ai /tmp/test-repo/file.txt` works
- `git-ai checkpoint mock_known_human /tmp/test-repo/file.txt` works
- `git-ai checkpoint human /tmp/test-repo/file.txt` works
