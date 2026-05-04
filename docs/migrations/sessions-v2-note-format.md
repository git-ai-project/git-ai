# Attribution Note Format: Sessions & Trace IDs

**Date:** 2026-05-04

This document describes several major, though strictly non-breaking, changes to the `refs/notes/ai` attribution note format for the Git AI community landing in v1.4.0.

## Preamble

We intend for the notes format to be as stable as possible and we do not take these changes lightly. We understand that many in the community rely on Git AI data, and stability is a huge part of our contract with the community.

This is the first major change to the notes format since Git AI v1.0 was released in Oct 2025 (over 6 months ago) and we have no further major changes planned.

We sincerely thank the community for your partnership and appreciate any and all input. Please join us on [Discord](https://discord.gg/XJStYvkb5U) to join the discussion.

## Major changes, but not breaking changes

Existing parsers will NOT error out and the Git AI CLI will continue to support the old format indefinitely.

However, existing parsers should be updated as soon as practical to ensure that they are able to understand the new properties and data that Git AI will report.

## Note format recap

Attribution notes are stored in `refs/notes/ai`. Each note has two sections separated by `---`: attestation lines (which lines were written by whom) followed by a JSON metadata block.

## Change 1: New `sessions` metadata type

A new `"sessions"` map appears in the metadata JSON alongside `"prompts"` and `"humans"`. Session records are lightweight -- they carry agent identity but no stats and no messages.

**Before:**
```json
{
  "schema_version": "authorship/3.0.0",
  "prompts": {
    "c9883b05a2487d6d": {
      "agent_id": {"tool": "cursor", "id": "session_123", "model": "gpt-4"},
      "human_author": "alice@example.com",
      "messages": [ ... ],
      "total_additions": 15,
      "total_deletions": 3,
      "accepted_lines": 11,
      "overriden_lines": 0
    }
  },
  "humans": { ... }
}
```

**After:**
```json
{
  "schema_version": "authorship/3.0.0",
  "prompts": { },
  "humans": { ... },
  "sessions": {
    "s_c9883b05a2487d": {
      "agent_id": {"tool": "cursor", "id": "session_123", "model": "gpt-4"},
      "human_author": "alice@example.com"
    }
  }
}
```

**Parsing note:** `"sessions"` may be absent on older notes. Treat it as an empty map when missing.

## Change 2: New attestation key format with trace IDs

Attestation lines in the note header now use a composite `session_id::trace_id` format for new-format checkpoints. This gives per-checkpoint granularity -- the same session can have multiple distinct attestation entries, one per tool invocation/edit.

**Before:**
```
src/main.rs
  c9883b05a2487d6d 1-10
  c9883b05a2487d6d 15-20
```

**After:**
```
src/main.rs
  s_c9883b05a2487d::t_9f8e7d6c5b4a32 1-10
  s_c9883b05a2487d::t_a1b2c3d4e5f678 15-20
```

**Key format breakdown:**
- `s_` + 14 hex chars = **session ID** (deterministic, same for all checkpoints from one agent session)
- `::` = separator
- `t_` + 14 hex chars = **trace ID** (random, unique per checkpoint call)

**To resolve an `s_`-prefixed attestation key to its metadata:** split on `::`, take the first part (the session ID), and look it up in `metadata.sessions`.

The length of the IDs is subject to change in the future, however, the prefixes and separators are part of the stable, public API.

## Change 3: Deprecated `messages` field removed from `PromptRecord`

The deprecated `"messages"` array is removed from prompt records. It will no longer appear in new notes. The optional `"messages_url"` field (a URL pointer to externally-stored transcripts) remains in the `"prompts"` object, however, it is not being tracked in `"sessions"` as the session IDs now map directly to agent conversation data (vastly simplifying the format and removing the need for extra IDs/hashes).

If you were parsing `messages` from prompt records, that data will no longer be present in notes produced as of `1.3.4` (already released as of mid-April).

## Change 4: Mixed-format notes

During the transition period, a single note can contain both old-format and new-format attestations. Old prompts use bare 16-char hex keys looked up in `"prompts"`; new sessions use `s_...::t_...` keys looked up in `"sessions"`.

**How to route an attestation hash:**

| Hash prefix | Lookup map | Example |
|---|---|---|
| `s_` | `metadata.sessions` (split on `::`, use first part) | `s_c9883b05a2487d::t_9f8e7d6c5b4a32` |
| `h_` | `metadata.humans` | `h_a1b2c3d4e5f678` |
| _(other)_ | `metadata.prompts` | `c9883b05a2487d6d` |

## What hasn't changed

- Schema version remains `authorship/3.0.0`
- The `"humans"` map and `h_`-prefixed attestation keys are unchanged
- The overall note structure (attestation lines + `---` + JSON metadata) is unchanged
- Old-format notes continue to work and will be correctly handled during rebases, cherry-picks, and other history rewrites

## What may change in the future

- We may add trace-level data to the `"humans"` attribution (ex: `h_a1b2c3d4e5f678::t_9f8e7d6c5b4a32`)
- We are still prototyping ideas for how to track the provenance of deletions with minimal foramt changes. Due to it's complexity, this is unlikely to land soon and it is also likely to live in the JSON so as to avoid adding more data above the attestations `---` fold.
