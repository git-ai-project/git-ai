# Transcript Secret Redaction

## Problem

Transcript data (raw JSON events from Claude JSONL, Gemini JSONL, Cursor JSONL, etc.) flows through the metrics pipeline and is uploaded to our telemetry service via HTTP (`/worker/metrics/upload`). This data can contain secrets (API keys, tokens, credentials) that users paste into conversations or that appear in tool output. Currently there is zero redaction in this path — secrets leave the machine unfiltered.

## Solution

Apply secret redaction to every raw transcript event at read time in `transcript_worker.rs`, before the event enters the metrics pipeline. Use the existing `redact_secrets_in_text()` function from `src/authorship/secrets.rs`. Only redact string values under keys that are NOT on a denylist of known-safe metadata keys.

## Design

### Integration Point

In `src/daemon/transcript_worker.rs`, within the `.map()` iterator (around line 512). The ordering is:

1. Extract event IDs and timestamp from the **unredacted** raw event (these are metadata fields that must not be corrupted)
2. Apply `redact_json_secrets()` to the raw event
3. Pass the redacted event to `SessionEventValues::with_ids()`

This ensures IDs/timestamps are extracted cleanly while secrets are stripped before the event enters the metrics pipeline or SQLite retry queue.

### Redaction Strategy: Recursive with Key-Based Denylist

Walk the JSON tree recursively. For every `Value::String` leaf, check whether its parent key is on the denylist. If not, apply `redact_secrets_in_text()`. Object keys themselves are never modified.

```rust
use crate::authorship::secrets::redact_secrets_in_text;
use serde_json::Value;

fn is_denied_key(key: &str) -> bool {
    // Pattern-based: skip any key that looks like an identifier field
    if key == "id" || key.ends_with("_id") || key.ends_with("Id") || key.ends_with("ID") {
        return true;
    }
    let lower = key.to_ascii_lowercase();
    if lower.ends_with("uuid") {
        return true;
    }
    // Exact matches for other metadata keys
    matches!(
        lower.as_str(),
        "timestamp" | "type" | "role" | "model" | "version"
    )
}

pub fn redact_json_secrets(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let redacted_v = if is_denied_key(&k) {
                        v // skip redaction for this value entirely
                    } else {
                        redact_json_secrets(v)
                    };
                    (k, redacted_v)
                })
                .collect(),
        ),
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(redact_json_secrets).collect())
        }
        Value::String(s) => {
            let (redacted, _) = redact_secrets_in_text(&s);
            Value::String(redacted)
        }
        other => other, // numbers, bools, null pass through
    }
}
```

### How It Works

1. **Recursive walk**: Traverse the JSON value tree depth-first.
2. **Key-based denylist**: When visiting an object, check each key against `is_denied_key()`. If the key matches (it's a known metadata/ID field), skip the entire subtree — don't redact the value or anything nested under it.
3. **String leaf redaction**: For string values not under a denied key, apply the existing entropy-based `redact_secrets_in_text()`. If no secrets are detected, the original string is cloned and returned unchanged.
4. **Arrays**: Recurse into each element (array elements have no key context, so they're always candidates for redaction).

### Denylist Rules

Pattern-based, minimal, expandable:

| Rule | Rationale |
|------|-----------|
| Key == `id` or ends with `_id` | Catches `id`, `session_id`, `event_id`, `message_id`, `parent_id`, etc. |
| Key ends with `Id` or `ID` (case-sensitive) | Catches `callId`, `parentId`, `modelId`, `parentID`, etc. without false-matching words like `valid`, `grid` |
| Key ends with `uuid` (case-insensitive) | Catches `uuid`, `parentUuid`, etc. |
| Key == `timestamp` | Unix/ISO timestamps |
| Key == `type` | Event type discriminators |
| Key == `role` | User/assistant/system role labels |
| Key == `model` | Model identifiers |
| Key == `version` | Version strings |

This covers all 10+ transcript formats' metadata fields without format-specific logic. The denylist is intentionally minimal — when in doubt, redact. IDs that are missed won't be corrupted by redaction anyway unless they happen to be high-entropy AND 15-90 chars (the entropy detector's threshold).

### Properties

- Secrets never enter the metrics pipeline or SQLite retry queue unredacted
- JSON structure is always preserved (we only modify string leaf values)
- IDs and timestamps are protected by the denylist — no corruption risk
- No new secret detection logic — uses existing entropy-based `redact_secrets_in_text()`
- Format-agnostic — works across all 10+ transcript formats without format-specific code
- Silent — no logging of redaction counts
- Object keys are never modified

### Performance

- Most transcript events are small (a few KB). Recursive walk is negligible.
- `redact_secrets_in_text()` is O(n) in string length with low constant factor (byte scan + bigram check).
- Denied keys short-circuit entire subtrees, avoiding unnecessary work on ID/metadata fields.
- No extra serialization/deserialization step (unlike the serialize-redact-reparse approach).

### File Changes

| File | Change |
|------|--------|
| `src/daemon/transcript_redaction.rs` | New module containing `redact_json_secrets()` and `is_denied_key()` |
| `src/daemon.rs` | Add `pub mod transcript_redaction;` declaration |
| `src/daemon/transcript_worker.rs` | Import and call `redact_json_secrets()` on `raw_event` after extracting IDs/timestamp, before `SessionEventValues::with_ids()` |
| `src/authorship/secrets.rs` | No changes needed (`redact_secrets_in_text` is already `pub`) |

### Testing Plan

1. Unit test: JSON object with a high-entropy string in a `"content"` field → verify it's redacted and output is valid JSON
2. Unit test: JSON with secrets in a denied key (e.g., `"session_id"`) → verify it's NOT redacted
3. Unit test: Deeply nested object with secrets at various depths → verify correct redaction
4. Unit test: JSON with no secrets → verify all string values returned unchanged
5. Unit test: `is_denied_key` covers expected patterns (case variations, suffixes)
6. Unit test: Array of mixed values → verify strings in arrays are redacted (no parent key context)

## Non-Goals

- Redacting working log data or git notes content (separate concern)
- Adding pattern-based detection (regex for AWS keys, GitHub tokens, etc.)
- Logging or counting redactions
- Redacting at upload time (belt-and-suspenders approach)
- Format-specific field targeting
