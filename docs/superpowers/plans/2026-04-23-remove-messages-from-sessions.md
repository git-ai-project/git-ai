# Remove messages/messages_url from SessionRecord Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `messages` and `messages_url` fields from `SessionRecord` to phase out CAS (Content-Addressable Storage) dependency, while maintaining backward compatibility for deserialization of old notes.

**Architecture:** SessionRecord is being introduced as a lightweight replacement for PromptRecord. PromptRecord has stats fields (total_additions, accepted_lines, etc.) and messages/messages_url. SessionRecord was meant to be lighter-weight but currently still carries the messages/messages_url fields. This plan removes those fields from SessionRecord while maintaining the ability to deserialize old notes that contain them.

**Tech Stack:** Rust, serde for serialization, SQLite for internal DB (CAS), git notes for storage

---

## Investigation Summary

### Current State

**SessionRecord definition** (`src/authorship/authorship_log.rs:221-230`):
```rust
pub struct SessionRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    pub messages: Vec<Message>,  // ← TO BE REMOVED
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_url: Option<String>,  // ← TO BE REMOVED
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}
```

**Key usages identified:**

1. **Creation sites** (3 locations in `src/authorship/virtual_attribution.rs`):
   - Lines ~468-474: Creates SessionRecord from checkpoint transcript
   - Lines ~677-703: Creates SessionRecord from checkpoint transcript  
   - Lines ~872-898: Creates SessionRecord from checkpoint transcript
   - All set `messages` from `checkpoint.transcript.as_ref().map(|t| t.messages().to_vec()).unwrap_or_default()`
   - All set `messages_url: None`

2. **CAS upload** (1 location in `src/authorship/post_commit.rs`):
   - Function `enqueue_session_messages_to_cas()` at lines 592-663
   - Reads `session.messages`, uploads to CAS, sets `session.messages_url`, clears `session.messages`
   - This entire function can be removed

3. **Rebase preservation** (`src/authorship/rebase_authorship.rs`):
   - Creates empty SessionRecord with `messages: Vec::new(), messages_url: None`

4. **Conversion helper** (`src/authorship/authorship_log.rs:233-246`):
   - `SessionRecord::to_prompt_record()` - clones messages and messages_url
   - Used for backwards-compatible lookup

5. **Test files affected**: 24 test files reference SessionRecord, .messages, or messages_url
   - Most common pattern: `messages: vec![]` (18 occurrences)
   - Most common pattern: `messages_url: None` (30 occurrences)
   - Key test file: `tests/integration/sessions_backwards_compat.rs` (432 lines) - explicitly tests old format deserialization

### Impact Analysis

**Source files to modify:**
- `src/authorship/authorship_log.rs` - struct definition, to_prompt_record()
- `src/authorship/virtual_attribution.rs` - 3 SessionRecord creation sites
- `src/authorship/post_commit.rs` - remove enqueue_session_messages_to_cas(), remove call site
- `src/authorship/rebase_authorship.rs` - SessionRecord creation
- `src/authorship/authorship_log_serialization.rs` - may need serde adjustments for backwards compat

**Test files to update:** 24 files
- `tests/integration/sessions_backwards_compat.rs` - core backward compat tests
- `tests/integration/agent_presets_comprehensive.rs` (1081 lines)
- `tests/daemon_mode.rs` (4810 lines) - has specific assertion on messages_url
- 21 other integration test files with SessionRecord/messages references

**Backwards compatibility requirement:**
- Old notes with sessions containing messages/messages_url fields MUST still deserialize correctly
- Use `#[serde(default, skip_serializing)]` to accept old fields on read but never write them
- Tests in `sessions_backwards_compat.rs` verify this works

---

## Task Breakdown

### Task 1: Update SessionRecord struct for backward-compatible deserialization

**Files:**
- Modify: `src/authorship/authorship_log.rs:221-230`

- [ ] **Step 1: Remove messages and messages_url fields from SessionRecord**

Change the struct from:
```rust
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

To:
```rust
pub struct SessionRecord {
    pub agent_id: AgentId,
    pub human_author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<HashMap<String, String>>,
}
```

- [ ] **Step 2: Update SessionRecord::to_prompt_record() to remove messages fields**

Change from:
```rust
pub fn to_prompt_record(&self) -> PromptRecord {
    PromptRecord {
        agent_id: self.agent_id.clone(),
        human_author: self.human_author.clone(),
        messages: self.messages.clone(),
        total_additions: 0,
        total_deletions: 0,
        accepted_lines: 0,
        overriden_lines: 0,
        messages_url: self.messages_url.clone(),
        custom_attributes: self.custom_attributes.clone(),
    }
}
```

To:
```rust
pub fn to_prompt_record(&self) -> PromptRecord {
    PromptRecord {
        agent_id: self.agent_id.clone(),
        human_author: self.human_author.clone(),
        messages: vec![],  // Sessions no longer store messages
        total_additions: 0,
        total_deletions: 0,
        accepted_lines: 0,
        overriden_lines: 0,
        messages_url: None,  // Sessions no longer use CAS
        custom_attributes: self.custom_attributes.clone(),
    }
}
```

- [ ] **Step 3: Verify the file compiles**

Run: `task build`
Expected: Compilation errors in files that reference the removed fields (expected and good - we'll fix these next)

- [ ] **Step 4: Commit struct changes**

```bash
git add src/authorship/authorship_log.rs
git commit -m "refactor: remove messages/messages_url from SessionRecord struct

SessionRecord is meant to be lightweight and not store transcript data.
Messages are no longer needed as we're phasing out CAS.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 2: Remove SessionRecord creation references to messages fields

**Files:**
- Modify: `src/authorship/virtual_attribution.rs:465-474`
- Modify: `src/authorship/virtual_attribution.rs:677-703`
- Modify: `src/authorship/virtual_attribution.rs:872-898`

- [ ] **Step 1: Update first SessionRecord creation (around line 465)**

Find the code block:
```rust
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
```

Replace with:
```rust
let session_record = SessionRecord {
    agent_id: agent_id.clone(),
    human_author: human_author.clone(),
    custom_attributes: None,
};
```

- [ ] **Step 2: Update second SessionRecord creation (around line 677)**

Find similar code block and apply the same transformation as Step 1.

- [ ] **Step 3: Update third SessionRecord creation (around line 872)**

Find similar code block and apply the same transformation as Step 1.

- [ ] **Step 4: Verify file compiles**

Run: `task build`
Expected: Should compile without errors for virtual_attribution.rs

- [ ] **Step 5: Commit virtual_attribution changes**

```bash
git add src/authorship/virtual_attribution.rs
git commit -m "refactor: remove messages from SessionRecord creation in virtual_attribution

Sessions no longer store transcript messages. The checkpoint.transcript
data is not needed for SessionRecord.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 3: Update rebase_authorship SessionRecord creation

**Files:**
- Modify: `src/authorship/rebase_authorship.rs`

- [ ] **Step 1: Find SessionRecord creation in rebase_authorship.rs**

Run: `grep -n "SessionRecord {" src/authorship/rebase_authorship.rs`
Expected: Should show line number(s) where SessionRecord is created

- [ ] **Step 2: Update SessionRecord instantiation**

Find code like:
```rust
.or_insert_with(|| crate::authorship::authorship_log::SessionRecord {
    agent_id: agent_id.clone(),
    human_author: None,
    messages: Vec::new(),
    messages_url: None,
    custom_attributes: None,
});
```

Replace with:
```rust
.or_insert_with(|| crate::authorship::authorship_log::SessionRecord {
    agent_id: agent_id.clone(),
    human_author: None,
    custom_attributes: None,
});
```

- [ ] **Step 3: Verify file compiles**

Run: `task build`
Expected: Should compile without errors for rebase_authorship.rs

- [ ] **Step 4: Commit rebase_authorship changes**

```bash
git add src/authorship/rebase_authorship.rs
git commit -m "refactor: remove messages fields from SessionRecord in rebase_authorship

Empty messages/messages_url no longer needed in SessionRecord.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 4: Remove CAS upload function for sessions

**Files:**
- Modify: `src/authorship/post_commit.rs:592-663`
- Modify: `src/authorship/post_commit.rs` (remove call site)

- [ ] **Step 1: Find the call site for enqueue_session_messages_to_cas**

Run: `grep -n "enqueue_session_messages_to_cas" src/authorship/post_commit.rs`
Expected: Should show function definition (line 592) and call site(s)

- [ ] **Step 2: Remove the function call**

Find code like:
```rust
enqueue_session_messages_to_cas(&repo, &mut authorship_log.metadata.sessions)?;
```

Delete this line entirely.

- [ ] **Step 3: Remove the enqueue_session_messages_to_cas function definition**

Delete the entire function from lines 592-663:
```rust
fn enqueue_session_messages_to_cas(
    repo: &Repository,
    sessions: &mut std::collections::BTreeMap<
        String,
        crate::authorship::authorship_log::SessionRecord,
    >,
) -> Result<(), GitAiError> {
    // ... entire function body ...
}
```

- [ ] **Step 4: Verify file compiles**

Run: `task build`
Expected: Should compile - no more references to removed function

- [ ] **Step 5: Run relevant tests**

Run: `task test TEST_FILTER=post_commit`
Expected: All post_commit tests pass

- [ ] **Step 6: Commit post_commit changes**

```bash
git add src/authorship/post_commit.rs
git commit -m "refactor: remove CAS upload function for sessions

Sessions no longer store messages, so enqueue_session_messages_to_cas
is no longer needed. This begins phasing out CAS for sessions.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 5: Update sessions_backwards_compat tests

**Files:**
- Modify: `tests/integration/sessions_backwards_compat.rs`

- [ ] **Step 1: Add backward compatibility deserialization test**

Add a new test after the existing tests:

```rust
#[test]
fn test_old_session_with_messages_deserializes_without_them() {
    // Construct a note with old-format session containing messages and messages_url
    let note = r#"test.txt
  s_1234567890abcd::t_fedcba0987654321 1-5
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.0.0",
  "base_commit_sha": "abc123",
  "sessions": {
    "s_1234567890abcd": {
      "agent_id": {
        "tool": "test_tool",
        "id": "test_agent",
        "model": "test_model"
      },
      "human_author": null,
      "messages": [{"role": "user", "content": "test message"}],
      "messages_url": "https://api.example.com/cas/abc123"
    }
  }
}"#;

    let log = AuthorshipLog::deserialize_from_string(note)
        .expect("should deserialize old format session with messages");

    assert_eq!(log.metadata.sessions.len(), 1, "should have 1 session");
    
    // The old messages/messages_url fields should be silently ignored (backward compat)
    let session = log.metadata.sessions.values().next().unwrap();
    assert_eq!(session.agent_id.tool, "test_tool");
    assert_eq!(session.agent_id.id, "test_agent");
    
    // Verify serialization does NOT include messages or messages_url
    let serialized = log.serialize_to_string().expect("should serialize");
    assert!(
        !serialized.contains("\"messages\""),
        "re-serialized note should not contain messages field"
    );
    assert!(
        !serialized.contains("\"messages_url\""),
        "re-serialized note should not contain messages_url field"
    );
}
```

- [ ] **Step 2: Update test_mixed_prompts_and_sessions_note_deserializes**

Find the test at line ~142. The note definition includes:
```rust
"sessions": {
  "s_1234567890abcd": {
    "agent_id": {
      "tool": "new_tool",
      "id": "new_agent",
      "model": "new_model"
    },
    "human_author": null,
    "messages": []
  }
}
```

Update to remove the messages field:
```rust
"sessions": {
  "s_1234567890abcd": {
    "agent_id": {
      "tool": "new_tool",
      "id": "new_agent",
      "model": "new_model"
    },
    "human_author": null
  }
}
```

Also remove the assertion about session having no stats - that's now obvious:
```rust
// Verify session has no stats fields (they're not in SessionRecord)
let session = log.metadata.sessions.values().next().unwrap();
assert_eq!(session.agent_id.tool, "new_tool");
assert_eq!(session.agent_id.id, "new_agent");
```

becomes:
```rust
// Verify session parsed correctly
let session = log.metadata.sessions.values().next().unwrap();
assert_eq!(session.agent_id.tool, "new_tool");
assert_eq!(session.agent_id.id, "new_agent");
```

- [ ] **Step 3: Update test_mixed_format_both_count_as_ai_in_blame**

Find the test at line ~202. Update the sessions section in the note from:
```rust
"sessions": {
  "s_1234567890abcd": {
    "agent_id": {
      "tool": "new_tool",
      "id": "new_agent",
      "model": "new_model"
    },
    "human_author": null,
    "messages": []
  }
}
```

To:
```rust
"sessions": {
  "s_1234567890abcd": {
    "agent_id": {
      "tool": "new_tool",
      "id": "new_agent",
      "model": "new_model"
    },
    "human_author": null
  }
}
```

- [ ] **Step 4: Run sessions_backwards_compat tests**

Run: `task test TEST_FILTER=sessions_backwards_compat`
Expected: All tests pass, including the new backward compat test

- [ ] **Step 5: Commit test updates**

```bash
git add tests/integration/sessions_backwards_compat.rs
git commit -m "test: update sessions_backwards_compat for removed messages fields

Add test verifying old notes with messages/messages_url deserialize
correctly (fields ignored). Update existing tests to remove messages
from test data.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 6: Update daemon_mode test

**Files:**
- Modify: `tests/daemon_mode.rs`

- [ ] **Step 1: Find the assertion on messages_url in daemon_mode.rs**

Run: `grep -n "messages_url" tests/daemon_mode.rs`
Expected: Should show line number (around line 4808 based on earlier grep)

- [ ] **Step 2: Read context around the assertion**

Run: `sed -n '4800,4820p' tests/daemon_mode.rs`
Expected: See the test context and what it's checking

- [ ] **Step 3: Remove or update the assertion**

The assertion is:
```rust
assert!(
    session.messages_url.is_some(),
    "session should retain a CAS URL after upload handoff"
);
```

Since sessions no longer have messages_url, remove this entire assertion block (3 lines).

- [ ] **Step 4: Run daemon_mode tests**

Run: `task test TEST_FILTER=daemon_mode`
Expected: All tests pass

- [ ] **Step 5: Commit daemon_mode test update**

```bash
git add tests/daemon_mode.rs
git commit -m "test: remove messages_url assertion from daemon_mode test

Sessions no longer upload to CAS or have messages_url field.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 7: Update remaining integration tests (bulk update)

**Files:**
- Modify: `tests/integration/agent_presets_comprehensive.rs`
- Modify: `tests/integration/agent_v1.rs`
- Modify: `tests/integration/amp.rs`
- Modify: `tests/integration/claude_code.rs`
- Modify: `tests/integration/codex.rs`
- Modify: `tests/integration/continue_cli.rs`
- Modify: `tests/integration/cursor.rs`
- Modify: `tests/integration/droid.rs`
- Modify: `tests/integration/gemini.rs`
- Modify: `tests/integration/github_copilot.rs`
- Modify: `tests/integration/opencode.rs`
- Modify: `tests/integration/pi.rs`
- Modify: `tests/integration/windsurf.rs`
- Modify: `tests/integration/initial_attributions.rs`
- Modify: `tests/integration/worktrees.rs`
- Modify: Additional files as identified by compilation errors

- [ ] **Step 1: Attempt full test build to collect all errors**

Run: `task build 2>&1 | tee /tmp/build_errors.txt`
Expected: Compilation errors for all test files that reference messages/messages_url

- [ ] **Step 2: Review compilation errors**

Run: `grep "messages\|messages_url" /tmp/build_errors.txt | head -50`
Expected: List of files and line numbers with field reference errors

- [ ] **Step 3: Create a script to find all SessionRecord instantiations in tests**

Run:
```bash
grep -rn "SessionRecord {" tests/ --include="*.rs" -A 6 | grep -E "(messages:|messages_url:)" > /tmp/session_record_refs.txt
cat /tmp/session_record_refs.txt
```
Expected: List of all test locations creating SessionRecord with messages fields

- [ ] **Step 4: Update agent_presets_comprehensive.rs**

For each SessionRecord instantiation, remove the `messages:` and `messages_url:` lines.

Pattern to find:
```rust
SessionRecord {
    agent_id: /* ... */,
    human_author: /* ... */,
    messages: vec![],
    messages_url: None,
    custom_attributes: /* ... */,
}
```

Replace with:
```rust
SessionRecord {
    agent_id: /* ... */,
    human_author: /* ... */,
    custom_attributes: /* ... */,
}
```

Note: Use editor find-and-replace or manual updates for each occurrence.

- [ ] **Step 5: Update remaining test files with same pattern**

Apply the same transformation from Step 4 to these files:
- `tests/integration/agent_v1.rs`
- `tests/integration/amp.rs`
- `tests/integration/claude_code.rs`
- `tests/integration/codex.rs`
- `tests/integration/continue_cli.rs`
- `tests/integration/cursor.rs`
- `tests/integration/droid.rs`
- `tests/integration/gemini.rs`
- `tests/integration/github_copilot.rs`
- `tests/integration/opencode.rs`
- `tests/integration/pi.rs`
- `tests/integration/windsurf.rs`

- [ ] **Step 6: Update initial_attributions.rs**

This file has PromptRecord definitions with messages_url, not SessionRecord. 
Check if any SessionRecord references exist:

Run: `grep -n "SessionRecord" tests/integration/initial_attributions.rs`

If none found, skip this file. If found, apply the same pattern as Step 4.

- [ ] **Step 7: Update worktrees.rs**

This file has PromptRecord with messages_url. Check for SessionRecord:

Run: `grep -n "SessionRecord" tests/integration/worktrees.rs`

If none found, skip. If found, apply the same transformation.

- [ ] **Step 8: Verify all tests compile**

Run: `task build`
Expected: Clean compilation with no errors

- [ ] **Step 9: Run integration test suite**

Run: `task test`
Expected: All tests pass (this will take ~15-20 minutes)

Note: If any tests fail, investigate the failure. Most likely causes:
1. Missed a SessionRecord instantiation
2. Test logic depends on messages being present (needs logic update)

- [ ] **Step 10: Commit all test file updates**

```bash
git add tests/integration/
git commit -m "test: remove messages/messages_url from SessionRecord test fixtures

Update all integration tests to remove messages and messages_url field
initializations from SessionRecord instances. These fields no longer
exist on SessionRecord.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

---

### Task 8: Update any remaining source files (if any)

**Files:**
- Investigate: `src/authorship/secrets.rs`
- Investigate: `src/commands/blame.rs`
- Investigate: `src/commands/diff.rs`
- Investigate: Other files revealed by grep

- [ ] **Step 1: Check if secrets.rs references SessionRecord**

Run: `grep -n "SessionRecord\|session\.messages" src/authorship/secrets.rs`
Expected: May show references to session.messages for secret redaction

- [ ] **Step 2: Update secrets.rs if needed**

If secrets.rs has code like:
```rust
for message in &mut record.messages {
    // redact secrets
}
record.messages.clear();
```

For SessionRecord, this code can be removed entirely (sessions don't have messages).
Keep the PromptRecord version (prompts still have messages for now).

Look for two separate implementations - one for PromptRecord, one for SessionRecord.
Remove only the SessionRecord version.

- [ ] **Step 3: Check blame.rs and diff.rs**

Run: `grep -n "SessionRecord" src/commands/blame.rs src/commands/diff.rs`
Expected: These files may look up sessions but shouldn't directly access messages field

If they do access `.messages`, the access is likely through the SessionRecord → PromptRecord 
conversion (`to_prompt_record()`), which we already updated to return empty messages.

No changes should be needed, but verify compilation succeeds.

- [ ] **Step 4: Check git/repo_storage.rs**

Run: `grep -n "SessionRecord" src/git/repo_storage.rs`
Expected: May reference SessionRecord for storage/retrieval

If it accesses `.messages` or `.messages_url`, remove those accesses.

- [ ] **Step 5: Verify all source files compile**

Run: `task build`
Expected: Clean compilation

- [ ] **Step 6: Run full test suite**

Run: `task test`
Expected: All tests pass

- [ ] **Step 7: Commit any source file updates**

If changes were made:
```bash
git add src/
git commit -m "refactor: update remaining source files for SessionRecord changes

Remove any remaining references to SessionRecord.messages or
SessionRecord.messages_url fields.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

If no changes needed, skip this commit.

---

### Task 9: Verify backward compatibility with real git notes

**Files:**
- Test: Manual verification with real repository data

- [ ] **Step 1: Create a test scenario with old-format session notes**

Run in a test repo:
```bash
cd /tmp
mkdir test-compat && cd test-compat
git init
echo "test" > test.txt
git add test.txt
git commit -m "initial"
```

- [ ] **Step 2: Manually create an old-format note with sessions containing messages**

Run:
```bash
cat > /tmp/old_session_note.txt << 'EOF'
test.txt
  s_1234567890abcd::t_fedcba0987654321 1
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.0.0",
  "base_commit_sha": "HEAD",
  "sessions": {
    "s_1234567890abcd": {
      "agent_id": {
        "tool": "test_tool",
        "id": "test_session",
        "model": "test_model"
      },
      "human_author": null,
      "messages": [{"role": "user", "content": "old message"}],
      "messages_url": "https://api.example.com/cas/old123"
    }
  }
}
EOF

# Add the note to the commit
COMMIT_SHA=$(git rev-parse HEAD)
git notes --ref=ai add -F /tmp/old_session_note.txt $COMMIT_SHA
```

- [ ] **Step 3: Run git-ai blame on the test file**

Run: `git-ai blame test.txt`
Expected: Should show attribution without errors, despite old messages fields in note

- [ ] **Step 4: Verify the note can be read and re-written**

Run:
```bash
# Read the note
git notes --ref=ai show HEAD

# Make a change that triggers re-write
echo "new line" >> test.txt
git add test.txt
git commit -m "update"
```

Expected: Commit succeeds, new note is written without messages fields

- [ ] **Step 5: Verify re-written note doesn't have messages**

Run:
```bash
git notes --ref=ai show HEAD | grep -E "(messages|messages_url)"
```
Expected: No output (fields not present in new note)

- [ ] **Step 6: Clean up test repository**

Run: `rm -rf /tmp/test-compat`

- [ ] **Step 7: Document compatibility verification**

Create a note in the commit message for the final task:
"Verified backward compatibility: old notes with sessions.messages/messages_url 
deserialize correctly, new notes don't write these fields."

---

### Task 10: Run full test suite and verify

**Files:**
- Test: Full test suite

- [ ] **Step 1: Run full test suite in daemon mode (default)**

Run: `task test`
Expected: All tests pass (15-20 minute runtime)

Note: This is the primary test mode per CLAUDE.md

- [ ] **Step 2: Run lint checks**

Run: `task lint`
Expected: No lint errors

- [ ] **Step 3: Run format checks**

Run: `task fmt`
Expected: No formatting changes needed (or apply formatting if changes shown)

- [ ] **Step 4: If formatting changes were applied, commit them**

```bash
git add -A
git commit -m "style: apply formatting

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Review git log for commit sequence**

Run: `git log --oneline --no-decorate | head -15`
Expected: Should see ~8-10 commits with clear messages describing the changes

- [ ] **Step 6: Create summary of changes**

Create a summary document showing:
- Number of files changed
- Number of lines removed
- Number of tests updated
- Confirmation of backward compatibility

Example:
```bash
git diff $(git log --oneline | tail -1 | cut -d' ' -f1)..HEAD --stat
```

---

### Task 11: Final verification and documentation

**Files:**
- Verify: All changes work end-to-end

- [ ] **Step 1: Verify SessionRecord no longer has messages fields**

Run: `grep -A 10 "pub struct SessionRecord" src/authorship/authorship_log.rs`
Expected: Should show SessionRecord with only agent_id, human_author, custom_attributes

- [ ] **Step 2: Verify CAS upload function is gone**

Run: `grep -n "enqueue_session_messages_to_cas" src/authorship/post_commit.rs`
Expected: No results (function removed)

- [ ] **Step 3: Verify tests don't reference removed fields**

Run:
```bash
grep -r "session\.messages\b" tests/ --include="*.rs" | grep -v "prompt\.messages"
```
Expected: No results (or only comments/strings, not field access)

Run:
```bash
grep -r "session\.messages_url" tests/ --include="*.rs"
```
Expected: No results

- [ ] **Step 4: Create a migration note document**

Create `docs/migrations/sessions-remove-messages.md`:

```markdown
# SessionRecord messages/messages_url Removal

**Date:** 2026-04-23
**Status:** Complete

## Summary

Removed `messages` and `messages_url` fields from `SessionRecord` to begin phasing out 
CAS (Content-Addressable Storage) dependency for sessions.

## Backward Compatibility

Old authorship notes containing sessions with `messages` and `messages_url` fields will 
deserialize correctly - the fields are silently ignored. New notes will never write 
these fields.

## Changes

- `SessionRecord` struct: removed 2 fields
- `enqueue_session_messages_to_cas()`: removed function
- `SessionRecord::to_prompt_record()`: returns empty messages/url
- All SessionRecord creation sites: no longer populate messages
- 24 test files updated

## Migration Path

No migration needed. Old notes continue to work. New commits create sessions without 
messages fields.

## Next Steps

- Complete removal of CAS infrastructure (separate task)
- Remove messages from PromptRecord (future task, after sessions fully replace prompts)
```

- [ ] **Step 5: Add the migration doc to git**

Run:
```bash
mkdir -p docs/migrations
git add docs/migrations/sessions-remove-messages.md
git commit -m "docs: add migration note for SessionRecord messages removal

Document the removal of messages/messages_url fields and backward
compatibility approach.

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Create final summary for user**

Generate final summary:

```
SessionRecord messages/messages_url removal complete!

Summary:
- Removed 2 fields from SessionRecord struct
- Updated 3 creation sites in virtual_attribution.rs
- Removed CAS upload function for sessions
- Updated 24 test files
- Verified backward compatibility
- All tests passing

Backward compatibility: Old notes with messages fields deserialize correctly.

Ready to push to the sessions-v2-remove-messages branch.
```

---

## Testing Strategy

### Unit Tests
- `authorship_log.rs` tests: SessionRecord::to_prompt_record() returns empty messages
- `sessions_backwards_compat.rs`: Old notes with messages deserialize, new notes don't serialize them

### Integration Tests
- All 24 test files should pass without modification to test logic (only fixture updates)
- Agent preset tests verify sessions created without messages
- Rebase tests verify sessions preserved without messages

### Manual Verification
- Real repository with old notes should continue to work
- New commits should create sessions without messages fields
- Blame/diff commands should work with both old and new notes

### Backward Compatibility Tests
- Deserialize old note with sessions containing messages → succeeds
- Re-serialize → messages fields omitted
- Mixed old/new format notes → both deserialize correctly

---

## Rollback Plan

If critical issues are found:

1. Revert all commits in reverse order:
```bash
git log --oneline | head -11  # Review commits
git revert HEAD~10..HEAD      # Revert last 10 commits
```

2. Or reset to pre-change commit:
```bash
git reset --hard <commit-before-changes>
```

3. Alternative: Add messages fields back to SessionRecord with `#[serde(default)]` and `skip_serializing` to maintain current behavior while investigating issues.

---

## Notes

- **Do not remove messages from PromptRecord** - that's a separate future task
- **Do not remove CAS infrastructure** - that's a separate future task  
- **This task only touches SessionRecord** - the newer, lighter-weight session type
- All changes maintain backward compatibility for deserialization
- Forward compatibility: new code never writes messages fields to sessions
