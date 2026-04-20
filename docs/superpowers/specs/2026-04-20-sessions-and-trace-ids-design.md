# Sessions & Trace IDs: Authorship Notes V2

## Summary

Replace the `prompts` system with a new `sessions` system for all new checkpoints. Sessions drop the problematic stats fields (`total_additions`, `total_deletions`, `accepted_lines`, `overriden_lines`) and add per-checkpoint trace IDs for higher-granularity attribution. Old `prompts` remain fully supported for backwards compatibility.

## Motivation

- Stats fields in `PromptRecord` add significant complexity to rewrite operations (rebase, cherry-pick, amend) and are unreliable approximations.
- No per-checkpoint granularity in attributions — all lines from the same agent session collapse into one attestation key.
- Trace IDs enable future telemetry correlation and finer attribution.

## Design Decisions

- **Schema version unchanged** (`authorship/3.0.0`) — changes are purely additive.
- **Approach A: Parallel Track** — sessions exist alongside prompts, routed by prefix (`s_` vs bare hex vs `h_`).
- **No migration** — old working logs produce prompts, new working logs produce sessions. Both coexist.
- **Checkpointing scripts unchanged** — no changes to hook invocations or agent presets' external interface.

---

## ID Formats

| Type | Format | Length | Generation |
|------|--------|--------|------------|
| Prompt (old) | `<16 hex chars>` | 16 | Deterministic: `SHA256(tool:agent_id)[0:16]` |
| Session (new) | `s_<14 hex chars>` | 16 | Deterministic: `"s_" + SHA256(tool:agent_id)[0:14]` |
| Human | `h_<14 hex chars>` | 16 | Deterministic: `"h_" + SHA256(author_identity)[0:14]` |
| Trace | `t_<14 hex chars>` | 16 | Random: `"t_" + 14 random hex chars` |
| Attestation key (new) | `s_<14hex>::t_<14hex>` | 34 | Composite of session + trace |

---

## New Data Structures

### `SessionRecord` (parallel to `PromptRecord`, without stats)

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

### Updated `AuthorshipMetadata`

```rust
pub struct AuthorshipMetadata {
    pub schema_version: String,
    pub git_ai_version: Option<String>,
    pub base_commit_sha: String,
    pub prompts: BTreeMap<String, PromptRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub humans: BTreeMap<String, HumanRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, SessionRecord>,  // keyed by "s_<14hex>"
}
```

### Updated `Checkpoint` (working log)

```rust
pub struct Checkpoint {
    // ... all existing fields unchanged ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,  // NEW: "t_<14hex>", present on ALL checkpoint kinds
}
```

---

## ID Generation Functions

```rust
/// Generate a session ID: "s_" + first 14 hex chars of SHA256(tool:agent_id)
pub fn generate_session_id(agent_id: &str, tool: &str) -> String {
    let combined = format!("{}:{}", tool, agent_id);
    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("s_{}", &hex[..14])
}

/// Generate a trace ID: "t_" + 14 random hex chars
pub fn generate_trace_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let hex: String = (0..14)
        .map(|_| char::from_digit(rng.random_range(0..16u8) as u32, 16).unwrap())
        .collect();
    format!("t_{}", hex)
}
```

---

## Checkpoint Flow Changes

### Trace ID Generation

Every checkpoint call (Human, KnownHuman, AI) generates a trace ID at the top of `execute_resolved_checkpoint()`:

```rust
let trace_id = generate_trace_id();
```

This is stored on the `Checkpoint` record for all kinds (future telemetry). For AI kinds, it's also composed into the author_id.

### Author ID Construction (AI Checkpoints Only)

```rust
let author_id = match kind {
    CheckpointKind::Human => "human".to_string(),
    CheckpointKind::KnownHuman => generate_human_short_hash(author),
    _ => {
        let session_id = generate_session_id(&agent_id.id, &agent_id.tool);
        format!("{}::{}", session_id, trace_id)
    }
};
```

The composite `s_<id>::t_<id>` string is stored directly in `WorkingLogEntry.attributions[].author_id` and `line_attributions[].author_id`.

### Working Log Backwards Compatibility

- Struct layout unchanged — only `Checkpoint` gains `trace_id: Option<String>`.
- Old working logs without `trace_id` deserialize as `None`.
- Old entries with bare 16-hex `author_id` route to prompts during post-commit.
- New entries with `s_` prefix route to sessions during post-commit.

---

## Post-Commit: AuthorshipLog Generation

### Prefix-Based Routing

In `VirtualAttributions::to_authorship_log_and_initial_working_log()`:

- `author_id == "human"` → skip (not attested)
- `author_id.starts_with("s_")` → attestation key is full `s_<id>::t_<id>`, session lookup by portion before `::`
- `author_id.starts_with("h_")` → humans map (unchanged)
- Otherwise → prompts map (unchanged, old format)

### Populating `metadata.sessions`

When processing checkpoints in `from_just_working_log()`:

- If checkpoint produces `s_`-prefixed author_ids, extract session key (before `::`), build `SessionRecord` from checkpoint's `agent_id`, `transcript`, `human_author`.
- Multiple traces with the same session key share one `SessionRecord` — latest transcript wins.
- Old-format checkpoints populate `prompts` map as before.

### INITIAL Attributions

Uncommitted lines preserve their full `s_<id>::t_<id>` author_id. The INITIAL sessions map is populated alongside INITIAL prompts, routed by prefix.

---

## Rewrite Operations

### Core Principle

Minimal changes — prefix-based routing handles everything naturally:

1. **Attestations are always recomputed** from VirtualAttributions — composite keys flow through as strings.
2. **Metadata copied as-is** via `remap_note_content_for_target_commit()` — `sessions` is just another JSON field.

### VirtualAttributions Changes

```rust
pub struct VirtualAttributions {
    // ... existing fields ...
    pub sessions: BTreeMap<String, SessionRecord>,       // NEW
    pub initial_only_session_ids: HashSet<String>,       // NEW
}
```

### Routing Pattern

Everywhere the code checks `if author_id.starts_with("h_")` to route to humans, add parallel check:

```rust
if author_id.starts_with("s_") {
    let session_key = author_id.split("::").next().unwrap();
    // route to sessions map
} else if author_id.starts_with("h_") {
    // route to humans map
} else {
    // route to prompts map
}
```

### What Stays Unchanged in Rewrites

- Stats recalculation — only touches `prompts`
- Rewrite log format and events
- Fast path metadata remaps (serde handles new field transparently)

---

## Stats & Blame

### Stats (`stats.rs`)

- `accepted_lines_from_attestations()`: When attestation hash starts with `s_`, extract session key, look up `SessionRecord` in `metadata.sessions` for `agent_id.tool`/`agent_id.model` to populate tool::model breakdown.
- Session-attested lines count toward `ai_accepted` and `ai_additions`.
- Sessions don't contribute to `total_ai_additions`/`total_ai_deletions` (no stats fields) — those metrics only come from old prompts.
- **Old notes produce identical stats.**

### Blame (`blame.rs`)

- `get_line_attribution()`: If hash starts with `s_`, split on `::`, look up session key in `metadata.sessions`, return `agent_id` for display.
- Trace ID available for future display but ignored in blame output for now.

### Diff (`diff.rs`)

- Same routing pattern. Minimal change since it uses lenient `get_authorship()`.

---

## Serialization Format

### Attestation Section (text, before `---`)

```
src/main.rs
  5a1b2c3d4e5f6789 1-15,20-30
  s_abc123def456ab::t_99ff00ee11dd22 16-19,31-45
  h_31dce776f88375 46-50
---
```

No structural changes to parser — hash is everything before the first space on an indented line.

### JSON Metadata Section (after `---`)

```json
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "abc123...",
  "prompts": {
    "5a1b2c3d4e5f6789": {
      "agent_id": { "tool": "cursor", "id": "sess_old", "model": "gpt-4o" },
      "human_author": "Alice <alice@co.com>",
      "messages": [],
      "total_additions": 15,
      "total_deletions": 3,
      "accepted_lines": 12,
      "overriden_lines": 1
    }
  },
  "humans": {
    "h_31dce776f88375": { "author": "Alice <alice@co.com>" }
  },
  "sessions": {
    "s_abc123def456ab": {
      "agent_id": { "tool": "claude", "id": "session_xyz", "model": "opus-4" },
      "human_author": "Alice <alice@co.com>",
      "messages": [],
      "messages_url": null
    }
  }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `src/authorship/authorship_log_serialization.rs` | `generate_session_id()`, `generate_trace_id()`, `sessions` field on `AuthorshipMetadata` |
| `src/authorship/authorship_log.rs` | `SessionRecord` struct |
| `src/authorship/working_log.rs` | `trace_id: Option<String>` on `Checkpoint` |
| `src/commands/checkpoint.rs` | Generate trace_id unconditionally, compose `s_::t_` author_id for AI kinds |
| `src/authorship/virtual_attribution.rs` | `sessions` + `initial_only_session_ids` on VirtualAttributions, prefix routing |
| `src/authorship/post_commit.rs` | Pass sessions through (flows via VirtualAttributions) |
| `src/authorship/rebase_authorship.rs` | Sessions field in VA construction/merging |
| `src/authorship/stats.rs` | Look up `s_`-prefixed attestations in sessions for tool::model |
| `src/commands/blame.rs` | Route `s_`-prefixed hashes to sessions |
| `src/commands/diff.rs` | Same routing (minimal) |

## Files NOT Changed

- Agent presets / checkpoint hook scripts
- Working log struct layout (`WorkingLogEntry`, `Attribution`, `LineAttribution`)
- Git proxy dispatch / signal handling
- Config / feature flags
- Schema version string
- `PromptRecord` struct and all existing prompts logic
- `HumanRecord` struct and humans logic
- Rewrite log events / format

---

## Testing Strategy

### Migrate Existing Tests

All current tests that exercise checkpointing/attribution migrate to the new `s_<id>::t_<id>` system. The `mock_ai` test checkpoint produces session-format author_ids. This ensures the new system is the primary, well-exercised path.

### Regression: Old Format

Dedicated tests that:
- Manually construct working logs / authorship notes with old 16-char prompt hashes
- Verify deserialization, stats computation, and blame resolution are identical
- Verify rewrites on old-format notes preserve prompts without introducing sessions

### Regression: Mixed Records

Dedicated tests that:
- Simulate commits with both old-format (16-char) and new-format (`s_::t_`) working log entries
- Verify resulting authorship note has both `prompts` and `sessions` populated
- Verify attestations reference the correct map based on prefix
- Verify stats aggregate correctly across both
- Verify rebase/amend of mixed-format notes preserves both sections

### Regression: Reading Old Notes

Tests that:
- Feed old-format serialized notes (no `sessions` key) through the new deserializer
- Confirm identical behavior to current code
