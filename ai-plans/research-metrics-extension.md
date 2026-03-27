---
name: Research metrics extension
overview: Extend git-ai to store per-checkpoint change history and prompt-level linking in git notes, enabling research analysis of developer-AI interaction patterns. Changes are additive to the existing format -- existing CLI commands (blame, diff, status) continue working.
todos:
  - id: message-id
    content: Add optional `id` field to `Message` enum variants in transcript.rs
    status: pending
  - id: bubble-id
    content: Propagate Cursor bubble_id into Message constructors in agent_presets.rs
    status: pending
  - id: checkpoint-fields
    content: Add `prompt_id` to Checkpoint, add `added_line_ranges`/`deleted_line_ranges` to WorkingLogEntry in working_log.rs
    status: pending
  - id: compute-ranges
    content: Compute added/deleted line ranges from diff in make_entry_for_file (checkpoint.rs)
    status: pending
  - id: set-prompt-id
    content: Set prompt_id on Checkpoint from last User message in transcript (checkpoint.rs)
    status: pending
  - id: history-structs
    content: Add ChangeHistoryEntry + FileChangeDetail structs, extend AuthorshipMetadata (authorship_log_serialization.rs)
    status: pending
  - id: build-history
    content: Build change_history from checkpoints in post_commit_with_final_state (post_commit.rs)
    status: pending
  - id: update-tests
    content: Update affected tests and run cargo insta review for snapshot changes
    status: pending
isProject: false
---

# Research Metrics Extension for git-ai

## Motivation and Goals

This is a research fork of git-ai. The purpose is to build a tool that tracks how developers interact with AI coding assistants, enabling analysis of developer-AI collaboration patterns.

The upstream git-ai tool already tracks AI vs human authorship of code at the line level. However, it **aggregates** checkpoint data into a single final attribution snapshot at commit time and then **deletes** the intermediate states. For research, we need the data that is currently thrown away:

1. **Per-checkpoint change history**: We want to see how code evolves through each individual AI edit and human edit between commits -- not just the final result. Each checkpoint represents one tool use (e.g., one file edit by the AI), and the sequence of checkpoints tells the story of how the developer and AI collaborated to produce the commit.
2. **Prompt-to-code linking**: The existing tool links code to entire AI *conversations* (identified by `conversation_id`). We need finer granularity: linking each code change to the specific *user prompt* that triggered it. This enables analysis of prompt effectiveness, iteration patterns, and how developers refine their instructions.
3. **Deletion tracking**: The existing tool counts deleted lines but discards which lines were deleted. We want the specific line ranges that were removed at each checkpoint, enabling analysis of revision patterns (e.g., how often AI-generated code is deleted by humans, or how AI rewrites its own code).

All three data points are already computed internally during checkpoint processing but are discarded before the git note is written. The implementation preserves them through to the final output.

**Scope**: Cursor agent only. Prompt linking uses Cursor's `bubbleId` (unique per-message identifier from Cursor's SQLite DB) as the prompt identifier.

## Architecture: What Changes and Why

The existing pipeline:

```
checkpoint -> working_log (JSONL + blobs) -> post_commit aggregation -> AuthorshipLog -> git note
                                                     ↓
                                          working_log deleted
```

The extended pipeline preserves checkpoint data through to the note:

```
checkpoint (+ prompt_id, deleted_lines) -> working_log -> post_commit
                                                              ↓
                                                    AuthorshipLog (existing)
                                                         +
                                                    change_history[] (NEW)
                                                         +
                                                    conversations{} (NEW, messages with IDs)
                                                              ↓
                                                          git note
```

## Implementation Steps

### Step 1: Add `id` field to `Message` enum

**File:** [src/authorship/transcript.rs](src/authorship/transcript.rs)

Add an optional `id: Option<String>` field to every variant of the `Message` enum, with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Update constructor functions (`user()`, `assistant()`, `tool_use()`, etc.) to accept an optional `id` parameter. This is backward-compatible -- existing serialized messages without `id` deserialize fine via `#[serde(default)]`.

### Step 2: Propagate Cursor bubble_id into Message

**File:** [src/commands/checkpoint_agent/agent_presets.rs](src/commands/checkpoint_agent/agent_presets.rs)

In `transcript_data_from_composer_payload` (around line 1700), the `bubble_id` is already extracted from each conversation header. Pass it through to the `Message` constructors:

```rust
// Currently (line 1724-1728):
transcript.add_message(Message::user(trimmed.to_string(), bubble_created_at.clone()));
// Change to:
transcript.add_message(Message::user_with_id(trimmed.to_string(), bubble_created_at.clone(), Some(bubble_id.to_string())));
```

Same for assistant messages and tool_use messages -- each gets the `bubbleId` from its header.

### Step 3: Add `prompt_id` to `Checkpoint` and diff ranges to `WorkingLogEntry`

**File:** [src/authorship/working_log.rs](src/authorship/working_log.rs)

- Add `prompt_id: Option<String>` to `Checkpoint` struct (serde-defaulted, optional). This is the `bubbleId` of the user message that triggered this checkpoint's edits.
- Add `added_line_ranges: Option<Vec<(u32, u32)>>` and `deleted_line_ranges: Option<Vec<(u32, u32)>>` to `WorkingLogEntry` (serde-defaulted, optional). Added ranges are in new-content line coordinates; deleted ranges are in previous-content line coordinates.

### Step 4: Compute deleted/added line ranges in checkpoint processing

**File:** [src/commands/checkpoint.rs](src/commands/checkpoint.rs)

In `make_entry_for_file` (line 1827), after calling `compute_file_line_stats`, also extract line ranges from the same diff. Write a helper:

```rust
fn compute_line_change_ranges(previous: &str, current: &str) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    let changes = compute_line_changes(previous, current);
    let mut added = Vec::new();
    let mut deleted = Vec::new();
    let mut old_line = 1u32;
    let mut new_line = 1u32;

    for change in changes {
        let num_lines = change.value().lines().count() as u32;
        match change.tag() {
            LineChangeTag::Equal => { old_line += num_lines; new_line += num_lines; }
            LineChangeTag::Delete => {
                deleted.push((old_line, old_line + num_lines - 1));
                old_line += num_lines;
            }
            LineChangeTag::Insert => {
                added.push((new_line, new_line + num_lines - 1));
                new_line += num_lines;
            }
        }
    }
    (added, deleted)
}
```

Store these on the `WorkingLogEntry` returned from `make_entry_for_file`.

### Step 5: Set `prompt_id` at checkpoint creation

**File:** [src/commands/checkpoint.rs](src/commands/checkpoint.rs) (around line 710-727 where `Checkpoint` is built)

After building the `Checkpoint`, determine the triggering prompt:

- If the checkpoint has a transcript, scan backward through messages to find the last `Message::User` with an `id`
- Set `checkpoint.prompt_id = that_user_message.id`

This works because at checkpoint creation time, `CursorPreset.run()` has already built the transcript with bubble IDs (Step 2), and the transcript is available on the `AgentRunResult` that flows into checkpoint creation.

### Step 6: Add `ChangeHistoryEntry` to AuthorshipMetadata

**File:** [src/authorship/authorship_log_serialization.rs](src/authorship/authorship_log_serialization.rs)

Add new structs:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeHistoryEntry {
    pub timestamp: u64,
    pub kind: String,                   // "human" or "ai_agent"
    pub conversation_id: Option<String>, // session hash (links to prompts map key)
    pub agent_type: Option<String>,      // tool name, e.g. "cursor"
    pub prompt_id: Option<String>,       // bubbleId of triggering user message
    pub model: Option<String>,
    pub files: BTreeMap<String, FileChangeDetail>,
    pub line_stats: CheckpointLineStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChangeDetail {
    pub added_lines: Vec<(u32, u32)>,
    pub deleted_lines: Vec<(u32, u32)>,
}
```

Add to `AuthorshipMetadata`:

```rust
pub struct AuthorshipMetadata {
    pub schema_version: String,        // bump to "authorship/4.0.0"
    pub git_ai_version: Option<String>,
    pub base_commit_sha: String,
    pub prompts: BTreeMap<String, PromptRecord>,
    pub change_history: Option<Vec<ChangeHistoryEntry>>,  // NEW
}
```

### Step 7: Build change_history in post-commit

**File:** [src/authorship/post_commit.rs](src/authorship/post_commit.rs)

In `post_commit_with_final_state` (line 69), after loading and refreshing checkpoints but **before** aggregating into `VirtualAttributions`:

- Iterate all checkpoints from the working log
- For each checkpoint, build a `ChangeHistoryEntry`:
  - `timestamp` from `checkpoint.timestamp`
  - `kind` from `checkpoint.kind`
  - `conversation_id` from `generate_short_hash(agent_id.id, agent_id.tool)` (same hash used in attestations)
  - `agent_type` from `agent_id.tool` (e.g. `"cursor"`)
  - `prompt_id` from `checkpoint.prompt_id`
  - `model` from `agent_id.model`
  - `files` from each `WorkingLogEntry`'s `added_line_ranges` / `deleted_line_ranges`
  - `line_stats` from `checkpoint.line_stats`
- Attach the array to `authorship_log.metadata.change_history`

Messages in `PromptRecord` will now include `id` fields (bubble_ids) automatically since the refetched transcript includes them (Step 2). No additional work needed for message IDs in the metadata prompts section.

### Step 8: Prevent pruning of new fields

**File:** [src/git/repo_storage.rs](src/git/repo_storage.rs)

`prune_old_char_attributions` (line 474) clears `entry.attributions` from older checkpoints per file. This is fine -- we don't need char-level attributions for change_history. But verify that `added_line_ranges` and `deleted_line_ranges` are NOT cleared by this function (they won't be, since it only touches `entry.attributions.clear()`).

## Output Format

The git note metadata JSON section will look like:

```json
{
  "schema_version": "authorship/4.0.0",
  "base_commit_sha": "abc123",
  "prompts": {
    "abc123def4567890": {
      "agent_id": { "tool": "cursor", "id": "conv-uuid", "model": "claude-4.6-opus" },
      "messages": [
        { "type": "user", "text": "refactor auth module", "id": "bubble-1", "timestamp": "..." },
        { "type": "assistant", "text": "I'll refactor...", "id": "bubble-2" },
        { "type": "tool_use", "name": "edit_file", "input": {"file_path": "src/auth.rs"}, "id": "bubble-3" }
      ],
      "total_additions": 50,
      "total_deletions": 20,
      "accepted_lines": 45,
      "overriden_lines": 5
    }
  },
  "change_history": [
    {
      "timestamp": 1711468800,
      "kind": "ai_agent",
      "conversation_id": "abc123def4567890",
      "agent_type": "cursor",
      "prompt_id": "bubble-1",
      "model": "claude-4.6-opus",
      "files": {
        "src/auth.rs": {
          "added_lines": [[5, 15], [30, 40]],
          "deleted_lines": [[5, 10], [25, 28]]
        }
      },
      "line_stats": { "additions": 22, "deletions": 10, "additions_sloc": 18, "deletions_sloc": 8 }
    },
    {
      "timestamp": 1711468900,
      "kind": "human",
      "files": {
        "src/auth.rs": {
          "added_lines": [[16, 18]],
          "deleted_lines": [[12, 13]]
        }
      },
      "line_stats": { "additions": 3, "deletions": 2, "additions_sloc": 3, "deletions_sloc": 2 }
    }
  ]
}
```

The text attestation section above `---` remains unchanged for backward compatibility.

## Risks and Considerations

- **Git note size**: Each checkpoint adds ~200-500 bytes to the metadata JSON. A commit with 20 checkpoints would add ~4-10KB. This is acceptable for research purposes.
- **Schema version bump**: Bumping to `authorship/4.0.0` means older versions of git-ai won't parse the new fields, but they'll ignore unknown JSON keys via serde defaults. The existing attestation text section is unchanged.
- **Test snapshots**: Many snapshot tests will need `cargo insta review` after these changes since the serialized format changes.
- **Human checkpoints**: These have no transcript or agent_id, so `prompt_id` and `author_id` will be null. They still appear in change_history with their line changes.

