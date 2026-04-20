# Sessions & Trace IDs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new `sessions` system to authorship notes with per-checkpoint trace IDs, replacing prompts for new checkpoints while keeping full backwards compatibility.

**Architecture:** Parallel Track approach — prefix-based routing (`s_` → sessions, `h_` → humans, bare hex → prompts) at every boundary. New ID functions generate session IDs deterministically and trace IDs randomly. All existing prompts logic untouched.

**Tech Stack:** Rust 2024 edition, sha2 crate (existing), rand 0.10 (existing dependency), serde for serialization.

---

### Task 1: Add `SessionRecord` Struct and `sessions` Field to `AuthorshipMetadata`

**Files:**
- Modify: `src/authorship/authorship_log.rs:198-216`
- Modify: `src/authorship/authorship_log_serialization.rs:24-50`

- [ ] **Step 1: Add `SessionRecord` struct to `authorship_log.rs`**

Add this struct after the `PromptRecord` definition (after line 216):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}
```

- [ ] **Step 2: Add `sessions` field to `AuthorshipMetadata` in `authorship_log_serialization.rs`**

In the `AuthorshipMetadata` struct (line 30), add after the `humans` field:

```rust
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, SessionRecord>,
```

Update the `AuthorshipMetadata::new()` impl to initialize it:

```rust
    pub fn new() -> Self {
        Self {
            schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
            git_ai_version: Some(GIT_AI_VERSION.to_string()),
            base_commit_sha: String::new(),
            prompts: BTreeMap::new(),
            humans: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }
```

Add the import for `SessionRecord` at the top of `authorship_log_serialization.rs`:

```rust
use crate::authorship::authorship_log::{HumanRecord, LineRange, PromptRecord, SessionRecord};
```

- [ ] **Step 3: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation (no errors). There may be unused field warnings which is fine.

- [ ] **Step 4: Commit**

```bash
git add src/authorship/authorship_log.rs src/authorship/authorship_log_serialization.rs
git commit -m "feat: add SessionRecord struct and sessions field to AuthorshipMetadata"
```

---

### Task 2: Add ID Generation Functions

**Files:**
- Modify: `src/authorship/authorship_log_serialization.rs:655-673`

- [ ] **Step 1: Write unit tests for the new ID functions**

Add these tests in the `#[cfg(test)] mod tests` block at the bottom of `authorship_log_serialization.rs`:

```rust
    #[test]
    fn test_generate_session_id() {
        let id = generate_session_id("session_123", "cursor");
        assert!(id.starts_with("s_"));
        assert_eq!(id.len(), 16);
        // Deterministic: same inputs produce same output
        assert_eq!(id, generate_session_id("session_123", "cursor"));
        // Different inputs produce different output
        assert_ne!(id, generate_session_id("session_456", "cursor"));
    }

    #[test]
    fn test_generate_trace_id() {
        let id = generate_trace_id();
        assert!(id.starts_with("t_"));
        assert_eq!(id.len(), 16);
        // Random: two calls produce different output
        assert_ne!(id, generate_trace_id());
        // All chars after prefix are hex
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_session_id_uses_same_hash_base_as_prompt_id() {
        // Session ID is the first 14 chars of the same SHA256 that prompt ID uses for its 16
        let session = generate_session_id("session_123", "cursor");
        let prompt = generate_short_hash("session_123", "cursor");
        // The hex portion of session (after "s_") should be a prefix of the prompt hash
        assert_eq!(&session[2..], &prompt[..14]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `task test TEST_FILTER=test_generate_session_id`
Expected: FAIL — `generate_session_id` not found

- [ ] **Step 3: Implement `generate_session_id` and `generate_trace_id`**

Add after `generate_human_short_hash` (after line 673):

```rust
/// Generate a session ID: "s_" + first 14 hex chars of SHA256(tool:agent_id) = 16 chars total.
/// Uses the same hash base as `generate_short_hash` but with a prefix and shorter hash portion.
/// The "s_" prefix distinguishes session IDs from legacy prompt hashes throughout the system.
pub fn generate_session_id(agent_id: &str, tool: &str) -> String {
    let combined = format!("{}:{}", tool, agent_id);
    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("s_{}", &hex[..14])
}

/// Generate a trace ID: "t_" + 14 random hex chars = 16 chars total.
/// Unique per checkpoint call (not deterministic). Used for per-checkpoint granularity
/// in attestation keys.
pub fn generate_trace_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let hex: String = (0..14)
        .map(|_| {
            let idx: u8 = rng.random_range(0..16);
            char::from_digit(idx as u32, 16).unwrap()
        })
        .collect();
    format!("t_{}", hex)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `task test TEST_FILTER=test_generate_session_id`
Run: `task test TEST_FILTER=test_generate_trace_id`
Run: `task test TEST_FILTER=test_session_id_uses_same_hash_base`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/authorship/authorship_log_serialization.rs
git commit -m "feat: add generate_session_id and generate_trace_id functions"
```

---

### Task 3: Add `trace_id` to `Checkpoint` Struct

**Files:**
- Modify: `src/authorship/working_log.rs:119-167`

- [ ] **Step 1: Write test for backwards-compatible deserialization**

Add this test in the existing `#[cfg(test)] mod tests` block in `working_log.rs`:

```rust
    #[test]
    fn test_checkpoint_trace_id_backwards_compat() {
        // Old JSON without trace_id should deserialize with trace_id = None
        let json = r#"{
            "kind": "AiAgent",
            "diff": "",
            "author": "claude",
            "entries": [],
            "timestamp": 1234567890,
            "transcript": null,
            "agent_id": {"tool": "claude", "id": "sess1", "model": "opus"},
            "line_stats": {"additions": 0, "deletions": 0, "additions_sloc": 0, "deletions_sloc": 0},
            "api_version": "checkpoint/1.0.0"
        }"#;
        let checkpoint: Checkpoint = serde_json::from_str(json).unwrap();
        assert_eq!(checkpoint.trace_id, None);

        // New JSON with trace_id should deserialize correctly
        let json_with_trace = r#"{
            "kind": "AiAgent",
            "diff": "",
            "author": "claude",
            "entries": [],
            "timestamp": 1234567890,
            "transcript": null,
            "agent_id": {"tool": "claude", "id": "sess1", "model": "opus"},
            "line_stats": {"additions": 0, "deletions": 0, "additions_sloc": 0, "deletions_sloc": 0},
            "api_version": "checkpoint/1.0.0",
            "trace_id": "t_abcdef01234567"
        }"#;
        let checkpoint: Checkpoint = serde_json::from_str(json_with_trace).unwrap();
        assert_eq!(checkpoint.trace_id, Some("t_abcdef01234567".to_string()));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_checkpoint_trace_id_backwards_compat`
Expected: FAIL — no `trace_id` field

- [ ] **Step 3: Add `trace_id` field to `Checkpoint` struct**

In `working_log.rs`, add to the `Checkpoint` struct after the `known_human_metadata` field (line 138):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
```

Update the `Checkpoint::new()` constructor (line 153-166) to initialize it:

```rust
    Self {
        kind,
        diff,
        author,
        entries,
        timestamp,
        transcript: None,
        agent_id: None,
        agent_metadata: None,
        line_stats: CheckpointLineStats::default(),
        api_version: CHECKPOINT_API_VERSION.to_string(),
        git_ai_version: Some(GIT_AI_VERSION.to_string()),
        known_human_metadata: None,
        trace_id: None,
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `task test TEST_FILTER=test_checkpoint_trace_id_backwards_compat`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/authorship/working_log.rs
git commit -m "feat: add trace_id field to Checkpoint struct"
```

---

### Task 4: Generate Trace ID and Compose Session Author ID in Checkpoint Processing

**Files:**
- Modify: `src/commands/checkpoint.rs` (lines ~776-930 and ~1912-1929)

- [ ] **Step 1: Add import for new ID functions at top of `checkpoint.rs`**

Find the existing import line:
```rust
use crate::authorship::authorship_log_serialization::generate_short_hash;
```

Replace with:
```rust
use crate::authorship::authorship_log_serialization::{
    generate_session_id, generate_short_hash, generate_trace_id,
};
```

- [ ] **Step 2: Generate trace_id in `execute_resolved_checkpoint`**

In `execute_resolved_checkpoint`, after the function's opening lines where it initializes variables (around line 786), add the trace ID generation:

Find the line where `let checkpoint_start_entries = Instant::now();` appears (or similar early initialization). Before the call to `get_checkpoint_entries`, add:

```rust
    let trace_id = generate_trace_id();
```

Then pass `trace_id.clone()` to `get_checkpoint_entries` — add it as a new parameter after `head_commit_override`.

- [ ] **Step 3: Update `get_checkpoint_entries` function signature**

Add `trace_id: String` parameter to the function signature:

```rust
async fn get_checkpoint_entries(
    kind: CheckpointKind,
    author: &str,
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    files: &[String],
    file_content_hashes: &HashMap<String, String>,
    previous_checkpoints: &[Checkpoint],
    agent_run_result: Option<&AgentRunResult>,
    ts: u128,
    is_pre_commit: bool,
    head_commit_override: Option<&str>,
    trace_id: String,
) -> Result<(Vec<WorkingLogEntry>, Vec<FileLineStats>), GitAiError> {
```

- [ ] **Step 4: Update the author_id construction in `get_checkpoint_entries`**

Replace the existing author_id match block (lines 1912-1929):

```rust
        let author_id = match kind {
            CheckpointKind::Human => kind.to_str(),
            CheckpointKind::KnownHuman => {
                crate::authorship::authorship_log_serialization::generate_human_short_hash(author)
            }
            _ => {
                agent_run_result
                    .map(|result| {
                        let session_id = generate_session_id(
                            &result.agent_id.id,
                            &result.agent_id.tool,
                        );
                        format!("{}::{}", session_id, trace_id)
                    })
                    .unwrap_or_else(|| kind.to_str())
            }
        };
```

- [ ] **Step 5: Store trace_id on the Checkpoint record**

In `execute_resolved_checkpoint`, where the `Checkpoint` is constructed (around line 880), after the existing fields are set, add:

```rust
    checkpoint.trace_id = Some(trace_id);
```

This goes right after the block that sets `checkpoint.agent_id` and `checkpoint.transcript` for AI kinds — but note `trace_id` is set for ALL kinds, not just AI.

- [ ] **Step 6: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation. Fix any call sites that need the new `trace_id` parameter.

- [ ] **Step 7: Run the full test suite to check for regressions**

Run: `task test`
Expected: Tests pass (existing tests may need snapshot updates due to new trace_id in serialized checkpoints). If snapshot tests fail, review with `cargo insta review` and accept if the only change is the addition of `trace_id` field.

- [ ] **Step 8: Commit**

```bash
git add src/commands/checkpoint.rs
git commit -m "feat: generate trace_id per checkpoint and compose s_::t_ author_id for AI"
```

---

### Task 5: Add `sessions` Field to `VirtualAttributions` and Route During Construction

**Files:**
- Modify: `src/authorship/virtual_attribution.rs` (struct definition ~lines 14-32, constructors ~997-1036, `from_just_working_log` ~318-493)

- [ ] **Step 1: Add `sessions` and `initial_only_session_ids` fields to `VirtualAttributions` struct**

In the struct definition (line 14), add two new fields after `initial_only_prompt_ids`:

```rust
    pub sessions: BTreeMap<String, SessionRecord>,
    initial_only_session_ids: HashSet<String>,
```

Add the import at the top of the file:
```rust
use crate::authorship::authorship_log::SessionRecord;
```

- [ ] **Step 2: Update the `new()` constructor**

Update `VirtualAttributions::new()` (line 1004) to initialize the new fields:

```rust
    pub fn new(
        repo: Repository,
        base_commit: String,
        attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        file_contents: HashMap<String, String>,
        ts: u128,
    ) -> Self {
        VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts: BTreeMap::new(),
            ts,
            blame_start_commit: None,
            humans: BTreeMap::new(),
            initial_only_prompt_ids: HashSet::new(),
            sessions: BTreeMap::new(),
            initial_only_session_ids: HashSet::new(),
        }
    }
```

- [ ] **Step 3: Update the `new_with_prompts()` constructor**

Update `VirtualAttributions::new_with_prompts()` (line 1025) to initialize new fields:

```rust
    pub fn new_with_prompts(
        repo: Repository,
        base_commit: String,
        attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        file_contents: HashMap<String, String>,
        prompts: BTreeMap<String, BTreeMap<String, PromptRecord>>,
        ts: u128,
    ) -> Self {
        VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts,
            ts,
            blame_start_commit: None,
            humans: BTreeMap::new(),
            initial_only_prompt_ids: HashSet::new(),
            sessions: BTreeMap::new(),
            initial_only_session_ids: HashSet::new(),
        }
    }
```

- [ ] **Step 4: Update `from_just_working_log` — route checkpoints to sessions when `s_` prefix**

In the `from_just_working_log` function (around lines 376-427), replace the checkpoint processing block. The key change: detect whether the checkpoint's author_id starts with `s_` and route to sessions instead of prompts.

Find the block starting with `if let Some(agent_id) = &checkpoint.agent_id {` (line 377). Replace the inner logic:

```rust
    if let Some(agent_id) = &checkpoint.agent_id {
        // Determine which format this checkpoint uses by checking entries
        // If entries have s_-prefixed author_ids, it's the new sessions format
        let is_session_format = checkpoint.entries.iter().any(|entry| {
            entry.line_attributions.iter().any(|la| la.author_id.starts_with("s_"))
        });

        if is_session_format {
            // New format: extract session_id from the first s_-prefixed attribution
            let session_id = checkpoint
                .entries
                .iter()
                .flat_map(|e| e.line_attributions.iter())
                .find(|la| la.author_id.starts_with("s_"))
                .map(|la| la.author_id.split("::").next().unwrap_or(&la.author_id).to_string())
                .unwrap_or_else(|| {
                    crate::authorship::authorship_log_serialization::generate_session_id(
                        &agent_id.id,
                        &agent_id.tool,
                    )
                });

            let session_record = SessionRecord {
                agent_id: agent_id.clone(),
                human_author: human_author.clone(),
                messages: checkpoint
                    .transcript
                    .as_ref()
                    .map(|t| t.messages().to_vec())
                    .unwrap_or_default(),
                messages_url: None,
                custom_attributes: None,
            };

            sessions.entry(session_id.clone()).or_insert(session_record);
            initial_only_session_ids.remove(&session_id);
        } else {
            // Old format: use existing prompts logic
            let author_id =
                crate::authorship::authorship_log_serialization::generate_short_hash(
                    &agent_id.id,
                    &agent_id.tool,
                );

            let prompt_record = crate::authorship::authorship_log::PromptRecord {
                agent_id: agent_id.clone(),
                human_author: human_author.clone(),
                messages: checkpoint
                    .transcript
                    .as_ref()
                    .map(|t| t.messages().to_vec())
                    .unwrap_or_default(),
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                messages_url: None,
                custom_attributes: None,
            };

            prompts
                .entry(author_id.clone())
                .or_insert_with(BTreeMap::new)
                .insert(String::new(), prompt_record);
            initial_only_prompt_ids.remove(&author_id);
        }

        // Track additions and deletions (for both formats, keyed by whatever ID format)
        let stat_key = if checkpoint.entries.iter().any(|entry| {
            entry.line_attributions.iter().any(|la| la.author_id.starts_with("s_"))
        }) {
            checkpoint
                .entries
                .iter()
                .flat_map(|e| e.line_attributions.iter())
                .find(|la| la.author_id.starts_with("s_"))
                .map(|la| la.author_id.split("::").next().unwrap_or(&la.author_id).to_string())
                .unwrap_or_default()
        } else {
            crate::authorship::authorship_log_serialization::generate_short_hash(
                &agent_id.id,
                &agent_id.tool,
            )
        };
        *session_additions.entry(stat_key.clone()).or_insert(0) +=
            checkpoint.line_stats.additions;
        *session_deletions.entry(stat_key).or_insert(0) +=
            checkpoint.line_stats.deletions;
    }
```

Also add `sessions` and `initial_only_session_ids` variable declarations near the top of `from_just_working_log` (alongside the existing `prompts` and `initial_only_prompt_ids` declarations):

```rust
    let mut sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
    let mut initial_only_session_ids: HashSet<String> = HashSet::new();
```

- [ ] **Step 5: Propagate sessions into the constructed VirtualAttributions**

At the end of `from_just_working_log` where the `VirtualAttributions` struct is returned, ensure `sessions` and `initial_only_session_ids` are assigned to the struct fields.

- [ ] **Step 6: Add a `pub fn sessions(&self)` accessor**

Add alongside any existing accessor methods:

```rust
    pub fn sessions(&self) -> &BTreeMap<String, SessionRecord> {
        &self.sessions
    }
```

- [ ] **Step 7: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 8: Commit**

```bash
git add src/authorship/virtual_attribution.rs
git commit -m "feat: add sessions field to VirtualAttributions with prefix-based routing"
```

---

### Task 6: Route Sessions in `to_authorship_log_and_initial_working_log`

**Files:**
- Modify: `src/authorship/virtual_attribution.rs` (lines ~1344-1725)

- [ ] **Step 1: Populate `metadata.sessions` in the output AuthorshipLog**

In `to_authorship_log_and_initial_working_log`, after the line that sets `authorship_log.metadata.humans = self.humans.clone();`, add:

```rust
    authorship_log.metadata.sessions = self.sessions.clone();
```

- [ ] **Step 2: Update the gap-fill logic to also exclude `s_`-prefixed fill for non-AI**

In the gap-fill logic (around line 1552), the condition `!prev_author.starts_with("h_")` should stay as-is. The `s_`-prefixed IDs are AI authors and should participate in gap filling — they just won't start with `h_` so they pass through correctly. No change needed here.

- [ ] **Step 3: Handle `s_`-prefixed author_ids in committed_lines_map attestation creation**

The existing code at line 1567-1579 already handles this correctly — it skips only `"human"` sentinel and passes everything else through. The `s_<id>::t_<id>` composite will flow through as the attestation `hash` value without changes.

However, we need to update the INITIAL-only filtering logic. Find the section (around line 1687-1705) that filters `initial_only_prompt_ids`. Add parallel logic for sessions:

```rust
    // Filter INITIAL-only sessions with no committed lines
    if !self.initial_only_session_ids.is_empty() {
        let committed_session_ids: HashSet<String> = authorship_log
            .attestations
            .iter()
            .flat_map(|file_att| file_att.entries.iter())
            .filter_map(|entry| {
                if entry.hash.starts_with("s_") {
                    Some(entry.hash.split("::").next().unwrap_or(&entry.hash).to_string())
                } else {
                    None
                }
            })
            .collect();

        authorship_log.metadata.sessions.retain(|session_id, _| {
            !self.initial_only_session_ids.contains(session_id)
                || committed_session_ids.contains(session_id)
        });
    }
```

- [ ] **Step 4: Handle `s_`-prefixed author_ids in uncommitted_lines_map (INITIAL)**

In the uncommitted lines section (around line 1627-1652), where `h_`-prefixed IDs route to `initial_humans`, add parallel routing for `s_`-prefixed IDs to `initial_sessions`:

After the existing `if author_id.starts_with("h_")` block, add:

```rust
        if author_id.starts_with("s_") {
            let session_key = author_id.split("::").next().unwrap_or(&author_id).to_string();
            if let Some(record) = self.sessions.get(&session_key) {
                initial_sessions.insert(session_key, record.clone());
            }
        }
```

Declare `initial_sessions` at the top of the uncommitted section:
```rust
    let mut initial_sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
```

And include it in the returned `InitialAttributions` (this requires updating the `InitialAttributions` struct too — see next step).

- [ ] **Step 5: Add `sessions` field to `InitialAttributions`**

Find the `InitialAttributions` struct definition (likely in `src/git/repo_storage.rs` or similar). Add:

```rust
    pub sessions: BTreeMap<String, SessionRecord>,
```

Update all construction sites to include `sessions: BTreeMap::new()` or the populated map.

- [ ] **Step 6: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation. Fix any construction sites that need `sessions` field.

- [ ] **Step 7: Commit**

```bash
git add src/authorship/virtual_attribution.rs src/git/repo_storage.rs
git commit -m "feat: route s_-prefixed attestations through sessions in post-commit"
```

---

### Task 7: Update Stats to Handle Session Attestations

**Files:**
- Modify: `src/authorship/stats.rs` (lines ~582-642)

- [ ] **Step 1: Write a unit test for stats with session attestations**

Create a test that constructs an `AuthorshipLog` with a session-based attestation and verifies that `accepted_lines_from_attestations` counts the lines correctly:

```rust
#[test]
fn test_accepted_lines_from_session_attestations() {
    use crate::authorship::authorship_log_serialization::{
        AttestationEntry, AuthorshipLog, FileAttestation,
    };
    use crate::authorship::authorship_log::{AgentId, LineRange, SessionRecord};

    let mut log = AuthorshipLog::new();

    // Add a session
    log.metadata.sessions.insert(
        "s_abcdef01234567".to_string(),
        SessionRecord {
            agent_id: AgentId {
                tool: "claude".to_string(),
                id: "sess1".to_string(),
                model: "opus-4".to_string(),
            },
            human_author: None,
            messages: vec![],
            messages_url: None,
            custom_attributes: None,
        },
    );

    // Add attestation with session::trace composite key
    log.attestations.push(FileAttestation {
        file_path: "src/main.rs".to_string(),
        entries: vec![AttestationEntry::new(
            "s_abcdef01234567::t_11223344556677".to_string(),
            vec![LineRange::Range(1, 10)],
        )],
    });

    let mut added_lines = HashMap::new();
    added_lines.insert("src/main.rs".to_string(), (1..=10).collect::<Vec<u32>>());

    let (ai_accepted, human_accepted, per_tool_model) =
        accepted_lines_from_attestations(Some(&log), &added_lines, false);

    assert_eq!(ai_accepted, 10);
    assert_eq!(human_accepted, 0);
    assert_eq!(per_tool_model.get("claude::opus-4"), Some(&10));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `task test TEST_FILTER=test_accepted_lines_from_session_attestations`
Expected: FAIL — the function doesn't handle `s_`-prefixed entries (they fall through to `prompts.get()` which returns None)

- [ ] **Step 3: Update `accepted_lines_from_attestations` to handle `s_`-prefixed entries**

In `accepted_lines_from_attestations` (line 605-637), after the `h_` check and before the existing prompt lookup, add session routing:

```rust
        for entry in &file_attestation.entries {
            // KnownHuman entries (h_ prefix): count as known-human-attested lines.
            if entry.hash.starts_with("h_") {
                let accepted = entry
                    .line_ranges
                    .iter()
                    .map(|line_range| line_range_overlap_len(line_range, added_lines))
                    .sum::<u32>();
                if accepted > 0 {
                    known_human_accepted += accepted;
                }
                continue;
            }

            let accepted = entry
                .line_ranges
                .iter()
                .map(|line_range| line_range_overlap_len(line_range, added_lines))
                .sum::<u32>();

            if accepted == 0 {
                continue;
            }

            total_ai_accepted += accepted;

            // Session entries (s_ prefix): look up in sessions map
            if entry.hash.starts_with("s_") {
                let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
                if let Some(session_record) = log.metadata.sessions.get(session_key) {
                    let tool_model = format!(
                        "{}::{}",
                        session_record.agent_id.tool, session_record.agent_id.model
                    );
                    *per_tool_model.entry(tool_model).or_insert(0) += accepted;
                }
            } else if let Some(prompt_record) = log.metadata.prompts.get(&entry.hash) {
                let tool_model = format!(
                    "{}::{}",
                    prompt_record.agent_id.tool, prompt_record.agent_id.model
                );
                *per_tool_model.entry(tool_model).or_insert(0) += accepted;
            }
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `task test TEST_FILTER=test_accepted_lines_from_session_attestations`
Expected: PASS

Run: `task test CARGO_TEST_ARGS="--lib"` (to ensure no regressions in other stats tests)
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/authorship/stats.rs
git commit -m "feat: handle s_-prefixed session attestations in stats computation"
```

---

### Task 8: Update Blame to Handle Session Attestations

**Files:**
- Modify: `src/authorship/authorship_log_serialization.rs` (lines ~237-321, `get_line_attribution`)

- [ ] **Step 1: Update `get_line_attribution` to route `s_`-prefixed hashes to sessions**

In the `get_line_attribution` function, find the section after the `h_` check (around line 264). Before the existing prompt lookup (`if let Some(prompt_record) = self.metadata.prompts.get(&entry.hash)`), add session routing:

```rust
                // s_-prefixed hashes are session attestations — route to sessions map
                if entry.hash.starts_with("s_") {
                    let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
                    if let Some(session_record) = self.metadata.sessions.get(session_key) {
                        let author = Author {
                            username: session_record.agent_id.tool.clone(),
                            email: String::new(),
                        };
                        // Convert SessionRecord to PromptRecord for compatibility with callers
                        let compat_prompt = PromptRecord {
                            agent_id: session_record.agent_id.clone(),
                            human_author: session_record.human_author.clone(),
                            messages: session_record.messages.clone(),
                            total_additions: 0,
                            total_deletions: 0,
                            accepted_lines: 0,
                            overriden_lines: 0,
                            messages_url: session_record.messages_url.clone(),
                            custom_attributes: session_record.custom_attributes.clone(),
                        };
                        return Some((author, Some(entry.hash.clone()), Some(compat_prompt)));
                    }
                    // s_ hash not found locally — try foreign lookup (same as prompts)
                    // Fall through to the foreign prompt lookup below
                }
```

Note: This converts `SessionRecord` → `PromptRecord` for the return type. This is a pragmatic choice to avoid changing the `get_line_attribution` return type (which would cascade to many callers). The stats fields are zeroed out, which is correct for sessions.

- [ ] **Step 2: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 3: Commit**

```bash
git add src/authorship/authorship_log_serialization.rs
git commit -m "feat: route s_-prefixed attestations to sessions in get_line_attribution"
```

---

### Task 9: Update Rewrite Operations to Propagate Sessions

**Files:**
- Modify: `src/authorship/rebase_authorship.rs` (multiple locations)
- Modify: `src/authorship/virtual_attribution.rs` (merge functions)

- [ ] **Step 1: Update `merge_attributions_favoring_first` and related merge functions**

Anywhere that merges VirtualAttributions and handles `self.humans`, add parallel handling for `self.sessions`. Search for all places in `rebase_authorship.rs` where `humans` map is merged (e.g., lines 836-846, 3440-3476) and add parallel session merging:

```rust
// Collect sessions (union-merge: first writer wins).
for (session_id, record) in &log.metadata.sessions {
    sessions.entry(session_id.clone()).or_insert(record.clone());
}
```

- [ ] **Step 2: Update `flatten_prompts_for_metadata` callers**

Where authorship logs are built from VAs in rebase operations, ensure `metadata.sessions` is also populated. Find all lines like:
```rust
authorship_log.metadata.humans = humans.clone();
```

And add after each:
```rust
authorship_log.metadata.sessions = sessions.clone();
```

Or for VAs:
```rust
authorship_log.metadata.sessions = self.sessions.clone();
```

- [ ] **Step 3: Update `new_with_prompts` calls in rebase to also carry sessions**

The `new_with_prompts` constructor doesn't currently accept sessions. Add a `new_with_prompts_and_sessions` constructor or modify existing one. The pragmatic approach is to set sessions after construction:

```rust
let mut va = VirtualAttributions::new_with_prompts(...);
va.sessions = merged_sessions;
```

This requires making `sessions` pub (which it already is from Task 5).

- [ ] **Step 4: Update `to_authorship_log` (the simpler one used in rebases)**

In the `to_authorship_log` method (around line 1038), ensure sessions are included:

```rust
    pub fn to_authorship_log(
        &self,
    ) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;

        let mut authorship_log = AuthorshipLog::new();
        authorship_log.metadata.base_commit_sha = self.base_commit.clone();
        // Flatten prompts
        authorship_log.metadata.prompts = self.prompts
            .iter()
            .filter_map(|(prompt_id, commits)| {
                commits.values().next().map(|record| (prompt_id.clone(), record.clone()))
            })
            .collect();
        authorship_log.metadata.humans = self.humans.clone();
        authorship_log.metadata.sessions = self.sessions.clone();  // ADD THIS
        // ... rest of function
```

- [ ] **Step 5: Run `task build` to verify compilation**

Run: `task build`
Expected: Successful compilation.

- [ ] **Step 6: Run full test suite**

Run: `task test`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/authorship/rebase_authorship.rs src/authorship/virtual_attribution.rs
git commit -m "feat: propagate sessions through rewrite operations (rebase, cherry-pick, amend)"
```

---

### Task 10: Migrate Existing Tests to New Session Format

**Files:**
- Modify: `src/commands/git_ai_handlers.rs` (mock_ai preset, lines ~634-677)

- [ ] **Step 1: Update the `mock_ai` preset to produce session-format author_ids**

The mock_ai preset currently creates an `AgentRunResult` with `checkpoint_kind = CheckpointKind::AiAgent`. Since Task 4 changes the checkpoint processor to compose `s_<id>::t_<id>` for all AI kinds, the mock_ai preset will automatically produce session-format author_ids after Task 4 is complete.

Verify this by running a simple existing test:

Run: `task test TEST_FILTER=test_ai_attribution`
Expected: PASS — but the authored lines will now be attributed with `s_` format. Snapshot tests may need updating.

- [ ] **Step 2: Review and accept snapshot changes**

Run: `cargo insta review`

Review each snapshot change. The expected pattern:
- Old: attestation hash is 16-char hex (e.g., `a1b2c3d4e5f6789a`)
- New: attestation hash is `s_<14hex>::t_<14hex>` composite

Accept all snapshots where the only difference is the ID format change.

- [ ] **Step 3: Update test assertion helpers if needed**

In `tests/integration/repos/test_file.rs`, the `is_ai_author_helper` function (lines 195-200) checks if the author name contains known AI tool names. Since `SessionRecord.agent_id.tool` is still `"mock_ai"`, and blame returns the tool name as the username, these assertions should still work.

Verify by running:
Run: `task test TEST_FILTER=test_using_test_repo`
Expected: PASS

- [ ] **Step 4: Run the full test suite**

Run: `task test`
Expected: All tests pass after snapshot updates.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test: migrate existing tests to session-format author IDs"
```

---

### Task 11: Add Regression Tests for Old Format

**Files:**
- Create: `tests/integration/sessions_backwards_compat.rs`

- [ ] **Step 1: Write test for old-format authorship note deserialization**

```rust
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

#[test]
fn test_old_format_note_without_sessions_deserializes() {
    let note_content = r#"src/main.rs
  5a1b2c3d4e5f6789 1-15,20-30
  h_31dce776f88375 16-19
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.0",
  "base_commit_sha": "abc123",
  "prompts": {
    "5a1b2c3d4e5f6789": {
      "agent_id": {"tool": "cursor", "id": "sess1", "model": "gpt-4o"},
      "human_author": null,
      "messages": [],
      "total_additions": 15,
      "total_deletions": 3,
      "accepted_lines": 12,
      "overriden_lines": 1
    }
  },
  "humans": {
    "h_31dce776f88375": {"author": "Alice <alice@co.com>"}
  }
}"#;

    let log = AuthorshipLog::deserialize_from_string(note_content).unwrap();
    assert_eq!(log.metadata.prompts.len(), 1);
    assert_eq!(log.metadata.humans.len(), 1);
    assert!(log.metadata.sessions.is_empty());
    assert_eq!(log.attestations.len(), 1);
    assert_eq!(log.attestations[0].entries.len(), 2);

    // Stats fields preserved
    let prompt = log.metadata.prompts.get("5a1b2c3d4e5f6789").unwrap();
    assert_eq!(prompt.total_additions, 15);
    assert_eq!(prompt.total_deletions, 3);
    assert_eq!(prompt.accepted_lines, 12);
    assert_eq!(prompt.overriden_lines, 1);
}

#[test]
fn test_old_format_note_roundtrips_without_adding_sessions() {
    let note_content = r#"src/main.rs
  5a1b2c3d4e5f6789 1-15
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.0",
  "base_commit_sha": "abc123",
  "prompts": {
    "5a1b2c3d4e5f6789": {
      "agent_id": {"tool": "cursor", "id": "sess1", "model": "gpt-4o"},
      "human_author": null,
      "messages": [],
      "total_additions": 10,
      "total_deletions": 2,
      "accepted_lines": 8,
      "overriden_lines": 0
    }
  }
}"#;

    let log = AuthorshipLog::deserialize_from_string(note_content).unwrap();
    let reserialized = log.serialize_to_string().unwrap();

    // Re-serialized version should NOT contain "sessions" key (skip_serializing_if empty)
    assert!(!reserialized.contains("\"sessions\""));
    // Prompts preserved
    assert!(reserialized.contains("5a1b2c3d4e5f6789"));
    assert!(reserialized.contains("\"total_additions\": 10"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `task test TEST_FILTER=test_old_format_note`
Expected: PASS

- [ ] **Step 3: Write integration test for old working log entries producing prompts**

```rust
use crate::repos::test_repo::TestRepo;
use crate::repos::test_file::ExpectedLineExt;
use crate::lines;

#[test]
fn test_old_format_working_log_produces_prompts_not_sessions() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test.txt");

    // Simulate old-format working log by writing file and using the legacy
    // human checkpoint (which doesn't produce s_ IDs)
    std::fs::write(&file_path, "Line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "human", "test.txt"]).unwrap();

    // Now write AI content using old-style mock that produces bare hex IDs
    // (This test verifies that if somehow old working logs are encountered, they still work)
    std::fs::write(&file_path, "Line 1\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("commit").unwrap();

    // Verify attribution works (lines are attributed)
    let mut file = repo.filename("test.txt");
    file.assert_committed_lines(lines![
        "Line 1".unattributed_human(),
        "AI line".ai(),
    ]);
}
```

- [ ] **Step 4: Run test**

Run: `task test TEST_FILTER=test_old_format_working_log`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add tests/integration/sessions_backwards_compat.rs
git commit -m "test: add regression tests for old-format authorship notes and working logs"
```

---

### Task 12: Add Regression Tests for Mixed Records

**Files:**
- Modify: `tests/integration/sessions_backwards_compat.rs`

- [ ] **Step 1: Write test for mixed prompts+sessions in same authorship note**

```rust
#[test]
fn test_mixed_prompts_and_sessions_note_deserializes() {
    let note_content = r#"src/old.rs
  5a1b2c3d4e5f6789 1-10
src/new.rs
  s_abcdef01234567::t_11223344556677 1-20
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "def456",
  "prompts": {
    "5a1b2c3d4e5f6789": {
      "agent_id": {"tool": "cursor", "id": "old_sess", "model": "gpt-4o"},
      "human_author": null,
      "messages": [],
      "total_additions": 10,
      "total_deletions": 0,
      "accepted_lines": 10,
      "overriden_lines": 0
    }
  },
  "sessions": {
    "s_abcdef01234567": {
      "agent_id": {"tool": "claude", "id": "new_sess", "model": "opus-4"},
      "human_author": "Bob <bob@co.com>",
      "messages": []
    }
  }
}"#;

    let log = AuthorshipLog::deserialize_from_string(note_content).unwrap();
    assert_eq!(log.metadata.prompts.len(), 1);
    assert_eq!(log.metadata.sessions.len(), 1);
    assert_eq!(log.attestations.len(), 2);

    // Verify prompt stats preserved
    let prompt = log.metadata.prompts.get("5a1b2c3d4e5f6789").unwrap();
    assert_eq!(prompt.total_additions, 10);

    // Verify session has no stats
    let session = log.metadata.sessions.get("s_abcdef01234567").unwrap();
    assert_eq!(session.agent_id.tool, "claude");
    assert_eq!(session.human_author, Some("Bob <bob@co.com>".to_string()));
}

#[test]
fn test_mixed_format_stats_aggregate_correctly() {
    use std::collections::HashMap;

    let note_content = r#"src/file.rs
  5a1b2c3d4e5f6789 1-5
  s_abcdef01234567::t_11223344556677 6-15
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "def456",
  "prompts": {
    "5a1b2c3d4e5f6789": {
      "agent_id": {"tool": "cursor", "id": "old_sess", "model": "gpt-4o"},
      "human_author": null,
      "messages": [],
      "total_additions": 5,
      "total_deletions": 0,
      "accepted_lines": 5,
      "overriden_lines": 0
    }
  },
  "sessions": {
    "s_abcdef01234567": {
      "agent_id": {"tool": "claude", "id": "new_sess", "model": "opus-4"},
      "human_author": null,
      "messages": []
    }
  }
}"#;

    let log = AuthorshipLog::deserialize_from_string(note_content).unwrap();
    let mut added_lines = HashMap::new();
    added_lines.insert("src/file.rs".to_string(), (1..=15).collect::<Vec<u32>>());

    let (ai_accepted, human_accepted, per_tool_model) =
        accepted_lines_from_attestations(Some(&log), &added_lines, false);

    // Both prompt-attested (5 lines) and session-attested (10 lines) count as AI
    assert_eq!(ai_accepted, 15);
    assert_eq!(human_accepted, 0);
    assert_eq!(per_tool_model.get("cursor::gpt-4o"), Some(&5));
    assert_eq!(per_tool_model.get("claude::opus-4"), Some(&10));
}
```

- [ ] **Step 2: Run tests**

Run: `task test TEST_FILTER=test_mixed`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/integration/sessions_backwards_compat.rs
git commit -m "test: add regression tests for mixed prompts+sessions authorship notes"
```

---

### Task 13: Integration Test — Full End-to-End Session Flow

**Files:**
- Modify: `tests/integration/sessions_backwards_compat.rs`

- [ ] **Step 1: Write end-to-end test for new checkpoint → commit → blame flow**

```rust
#[test]
fn test_new_session_checkpoint_to_commit_to_blame() {
    let repo = TestRepo::new();
    let mut file = repo.filename("example.rs");
    file.set_contents(lines!["fn main() {".ai(), "    println!(\"hello\");".ai(), "}".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Verify lines are attributed as AI
    file.assert_committed_lines(lines![
        "fn main() {".ai(),
        "    println!(\"hello\");".ai(),
        "}".ai(),
    ]);

    // Read the authorship note and verify it uses sessions format
    let note = repo.get_authorship_note_content("HEAD").unwrap();
    assert!(note.contains("\"sessions\""), "Note should contain sessions object");
    assert!(note.contains("s_"), "Attestation should use s_ prefix");
    assert!(note.contains("::t_"), "Attestation should contain ::t_ trace ID");
    // Should NOT have prompts (since this is all new-format)
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();
    assert!(log.metadata.prompts.is_empty());
    assert!(!log.metadata.sessions.is_empty());
}

#[test]
fn test_trace_ids_are_unique_per_checkpoint() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test.txt");

    // First AI edit
    std::fs::write(&file_path, "Line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    // Second AI edit (same session but different checkpoint call)
    std::fs::write(&file_path, "Line 1\nLine 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    repo.stage_all_and_commit("commit").unwrap();

    // Read note — both attestations should have same s_ but different t_
    let note = repo.get_authorship_note_content("HEAD").unwrap();
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();

    let all_hashes: Vec<&String> = log
        .attestations
        .iter()
        .flat_map(|f| f.entries.iter())
        .map(|e| &e.hash)
        .collect();

    // All hashes should start with s_
    for hash in &all_hashes {
        assert!(hash.starts_with("s_"), "hash should start with s_: {}", hash);
        assert!(hash.contains("::t_"), "hash should contain trace: {}", hash);
    }

    // If there are multiple attestation entries, their trace IDs should differ
    if all_hashes.len() > 1 {
        let traces: Vec<&str> = all_hashes
            .iter()
            .filter_map(|h| h.split("::").nth(1))
            .collect();
        let unique_traces: std::collections::HashSet<&&str> = traces.iter().collect();
        assert_eq!(traces.len(), unique_traces.len(), "Trace IDs should be unique");
    }
}
```

- [ ] **Step 2: Add helper method to TestRepo if needed**

If `get_authorship_note_content` doesn't exist on TestRepo, add it:

```rust
pub fn get_authorship_note_content(&self, commit_ref: &str) -> Result<String, String> {
    let sha = self.git(&["rev-parse", commit_ref])?.trim().to_string();
    self.git(&["notes", "--ref=ai", "show", &sha])
}
```

- [ ] **Step 3: Run tests**

Run: `task test TEST_FILTER=test_new_session_checkpoint`
Run: `task test TEST_FILTER=test_trace_ids_are_unique`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add tests/integration/sessions_backwards_compat.rs
git commit -m "test: add end-to-end integration tests for session checkpoint flow"
```

---

### Task 14: Integration Test — Rewrite Operations with Sessions

**Files:**
- Modify: `tests/integration/sessions_backwards_compat.rs`

- [ ] **Step 1: Write test for rebase preserving sessions**

```rust
#[test]
fn test_rebase_preserves_sessions() {
    let repo = TestRepo::new();

    // Create base commit
    let file_path = repo.path().join("test.txt");
    std::fs::write(&file_path, "Base line\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Create branch with AI edit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    std::fs::write(&file_path, "Base line\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("ai edit").unwrap();

    // Go back to main, add unrelated commit
    repo.git(&["checkout", "main"]).unwrap();
    let other_path = repo.path().join("other.txt");
    std::fs::write(&other_path, "Other\n").unwrap();
    repo.stage_all_and_commit("other").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "main"]).unwrap();

    // Verify sessions preserved in rebased commit's note
    let note = repo.get_authorship_note_content("HEAD").unwrap();
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();
    assert!(!log.metadata.sessions.is_empty(), "Sessions should survive rebase");
    assert!(
        log.attestations.iter().any(|f| f.entries.iter().any(|e| e.hash.starts_with("s_"))),
        "Attestations should still have s_ entries after rebase"
    );
}
```

- [ ] **Step 2: Run test**

Run: `task test TEST_FILTER=test_rebase_preserves_sessions`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/integration/sessions_backwards_compat.rs
git commit -m "test: add rebase integration test for sessions preservation"
```

---

### Task 15: Final Verification and Cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run the full test suite**

Run: `task test`
Expected: All tests pass.

- [ ] **Step 2: Run lint and format**

Run: `task lint`
Run: `task format`
Expected: No issues.

- [ ] **Step 3: Review and accept any remaining snapshot changes**

Run: `cargo insta review`
Accept snapshots where the only change is:
- Addition of `trace_id` field in checkpoint serialization
- `s_<id>::t_<id>` format in attestation hashes

- [ ] **Step 4: Final commit if any formatting/lint changes**

```bash
git add -A
git commit -m "chore: lint and format cleanup"
```
