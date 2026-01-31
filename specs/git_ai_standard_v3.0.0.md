# Git AI Standard v3.0.0

This document defines the Git AI Authorship Log format for tracking AI-generated code contributions within Git repositories. 

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

---

The [Git AI project](https://github.com/git-ai-project/git-ai) is a full, production-ready implementation of this standard built as a Git extension. Another project would be considered compliant with this standard if it also attached AI Authorship Logs with Git Notes, even if it was implemented in another way. 

If you are trying to add support for your Coding Agent to Git AI format, that is best done by [integrating with published implementation](https://usegitai.com/docs/cli/add-your-agent), not implementing this spec. 

## 1. Authorship Logs

Authorship logs provide a record of which lines in a commit were authored by AI agents, along with the conversation threads that generated them. The line numbers are only accurate in the context of that commit, with the version of each file at the time of committing. 

### 1.1 Attaching Git Notes to Commits

Git AI uses [Git Notes](https://git-scm.com/docs/git-notes) to attach authorship metadata to commits without modifying commit history.

#### Notes Reference

- Authorship logs MUST be stored under the `refs/notes/ai` namespace
- Implementations MUST NOT use the default `refs/notes/commits` namespace to avoid conflicts with other tools
- Each commit SHA MAY have at most one authorship log attached

### 1.2 Log Format

The Authorship Log MUST consist of two sections separated by a divider line containing exactly `---`:

1. **Attestation Section** — Line-level attribution mapping
2. **Metadata Section** — JSON object containing prompt records and versioning

#### 1.2.1 Schema Version

The schema version for this specification is:

```
authorship/3.0.0
```

Implementations MUST include this version string in the `schema_version` field of the metadata section.

#### 1.2.2 Overall Structure

```
<attestation-section>
---
<metadata-section>
```

The divider `---` MUST appear on its own line with no leading or trailing whitespace. This allows a buffer to quickly read just the Attestation Section without loading the metadata (for very fast `git-ai blame` operations)

---

### 1.2.3 Attestation Section

The attestation section maps files to the AI sessions that authored specific lines.

#### File Path Lines

- File paths MUST appear at the start of a line (no leading whitespace)

```
src/main.rs
```

- File paths containing spaces, tabs, or newlines MUST be wrapped in double quotes (`"`)

```
"src/my file.rs"
```

- File paths SHOULD NOT contain the quote character (`"`)
- Files with no AI Attributions MUST NOT be included in the Attestation Section



#### Attestation Entry Lines

Each attestation entry MUST be indented with exactly two spaces and contain:
1. A **hash** pointing to a prompt in the Metadata Section (16 hexadecimal characters)
2. A single space
3. A **line range specification**

```
  d9978a8723e02b52 1-4,9-10,12,14,16
```

#### Line Range Specification

Line ranges MUST use one of the following formats:

| Format | Description | Example |
|--------|-------------|---------|
| Single line | A single line number | `42` |
| Range | Inclusive start and end, hyphen-separated | `19-222` |
| Multiple | Comma-separated combination of singles and ranges | `1,2,19-222,300` |

Line numbers MUST be:
- 1-indexed (first line is `1`, not `0`)
- Positive integers
- Sorted in ascending order within each entry

Line ranges:
- MUST NOT contain spaces
- SHOULD be sorted by their start position
- SHOULD use ranges for consecutive lines (e.g., `1-5` instead of `1,2,3,4,5`)

#### Attestation Section Example

```
tests/simple_additions.rs
  d9978a8723e02b52 1-4,9-10,12,14,16,21-22,24,26
  e5be5f8723e02b52 1011-1012,1014-1045,1047-1065
  967bda75801c3ee8 728-735,737-888,890,892-1010
src/authorship/attribution_tracker.rs
  e5be5f8723e02b52 829-838,1509-1512
  866dabf162e96bcb 6,257,358,376-377,521
```

The above example can be read as:

In `tests/simple_additions.rs`, 3 prompts `d9978a8723e02b52`, `e5be5f8723e02b52`, and `967bda75801c3ee8`, generated the lines above

#### Session Hash Semantics

Each session hash in the attestation section MUST correspond to a key in the `prompts` object of the metadata section. Session hashes:
- MUST be hexadecimal characters only
- MUST be generated using SHA-256 of `{tool}:{conversation_id}`, taking the first 16 hex characters. Ie `cursor-${coversation_id}` or `claude-code-${coversation_id}` or `amp-${thread_id}`
- SHOULD remain stable for the same AI session across commits

**Hash Length:**
- New implementations MUST generate 16-character hashes
- Implementations SHOULD accept 7-character hashes for backward compatibility with earlier versions

---

### 1.2.4 Metadata Section

The metadata section MUST be a valid JSON object containing the following fields:

#### Required Fields

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | string | MUST be `"authorship/3.0.0"` |
| `base_commit_sha` | string | The commit SHA this authorship log was computed against |
| `prompts` | object | Map of session hashes to prompt records |

#### Optional Fields

| Field | Type | Description |
|-------|------|-------------|
| `git_ai_version` | string | Version of the git-ai tool that generated this log |

#### Prompt Record Object

Each entry in the `prompts` object MUST contain:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent_id` | object | REQUIRED | Identifies the AI agent |
| `human_author` | string | OPTIONAL | The human who prompted the AI (e.g., `"Name <email>"`) |
| `messages` | array | REQUIRED | The conversation transcript |
| `total_additions` | integer | REQUIRED | Total lines added by this session |
| `total_deletions` | integer | REQUIRED | Total lines deleted by this session |
| `accepted_lines` | integer | REQUIRED | Lines accepted in the final commit |
| `overridden_lines` | integer | REQUIRED | Lines that were later modified by human |

#### Agent ID Object

| Field | Type | Description |
|-------|------|-------------|
| `tool` | string | The AI tool/IDE (e.g., `"cursor"`, `"claude"`, `"copilot"`) |
| `id` | string | Unique session identifier (typically a UUID) |
| `model` | string | The AI model used (e.g., `"claude-4.5-opus-high-thinking"`) |

#### Message Object

Each message in the `messages` array MUST contain:

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | One of: `"user"`, `"assistant"`, `"tool_use"` |
| `text` | string | The message content (for `user` and `assistant` types) |
| `timestamp` | string | ISO 8601 timestamp (OPTIONAL) |
| `name` | string | Tool name (for `tool_use` type only) |
| `input` | object | Tool input parameters (for `tool_use` type only) |

#### Message Array Requirements

The `messages` array:
- MUST contain all human prompts (`type: "user"`)
- MUST contain all assistant responses (`type: "assistant"`)
- MUST contain all tool calls made by the assistant (`type: "tool_use"`)
- MUST NOT contain tool responses (the results returned from tool executions)

Tool responses are excluded because they often contain large amounts of file content, command output, or other verbose data that would bloat the authorship log without adding meaningful attribution context.

---

### 1.2.5 Complete Example

```
src/main.rs
  abcd1234abcd1234 1-10,15-20
  efgh5678efgh5678 25,30-35
src/lib.rs
  abcd1234abcd1234 1-50
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.0.23",
  "base_commit_sha": "7734793b756b3921c88db5375a8c156e9532447b",
  "prompts": {
    "abcd1234abcd1234": {
      "agent_id": {
        "tool": "cursor",
        "id": "6ef2299e-a67f-432b-aa80-3d2fb4d28999",
        "model": "claude-4.5-opus"
      },
      "human_author": "Developer <dev@example.com>",
      "messages": [
        {
          "type": "user",
          "text": "Add error handling to the main function",
          "timestamp": "2025-12-05T01:22:13.211Z"
        },
        {
          "type": "assistant",
          "text": "I'll add comprehensive error handling...",
          "timestamp": "2025-12-05T01:22:38.724Z"
        }
      ],
      "total_additions": 25,
      "total_deletions": 5,
      "accepted_lines": 20,
      "overriden_lines": 0
    },
    "efgh5678efgh5678": {
      "agent_id": {
        "tool": "cursor",
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "model": "claude-3-sonnet"
      },
      "human_author": "Developer <dev@example.com>",
      "messages": [
        {
          "type": "user",
          "text": "Add logging",
          "timestamp": "2025-12-05T02:00:00.000Z"
        }
      ],
      "total_additions": 6,
      "total_deletions": 0,
      "accepted_lines": 6,
      "overriden_lines": 0
    }
  }
}
```

---

## 2. History Rewriting Behaviors 

Authorship Logs can be attached to one, and only one commit SHA. When users do Git operations like `rebase`, `cherry-pick`, `reset`, `merge`, `stash`/`pop`, that rewrite the worktree and history, corresponding changes to Authorship Logs are required. 

### 2.1 Rebase

A rebase takes a range of commits and rewrites history, creating new commits with different SHAs. Implementations MUST preserve AI authorship attribution through all rebase scenarios.

#### Core Principles

1. **SHA Independence**: Authorship is attached to commit SHAs. When a commit's SHA changes, the authorship log MUST be copied to the new commit
2. **Content-Based Attribution**: Line attributions MUST reflect the actual content at each commit, not the original commit's state
3. **Prompt Preservation**: All prompt records from original commits MUST be preserved in the corresponding new commits

#### Standard Rebase (1:1 Mapping)

When commits are rebased without modification (e.g., `git rebase main`):

- For each original commit → new commit mapping, implementations MUST copy the authorship log
- The `base_commit_sha` field SHOULD be updated to reflect the new parent commit
- Line numbers in attestations remain valid because file content is unchanged

```
Original: A → B → C → D (feature)
                ↑
              main

After rebase onto main':
main' → B' → C' → D'

Authorship mapping:
  B → B' (copy authorship log)
  C → C' (copy authorship log)  
  D → D' (copy authorship log)
```

#### Interactive Rebase: Commit Reordering

When commits are reordered (e.g., `pick C` before `pick B`):

- Each new commit MUST have authorship reflecting its actual content at that point in history
- Line numbers MUST be recalculated based on the file state at each new commit
- Implementations MUST track content through the reordered sequence and adjust attributions accordingly

```
Original order: B → C → D
Reordered:      C' → B' → D'

For C' (now first):
  - Attributions based on C's changes applied to main'
  
For B' (now second):
  - Attributions based on B's changes applied after C'
  - Line numbers adjusted for C's prior changes
```

#### Interactive Rebase: Squash/Fixup (N → 1)

When multiple commits are squashed into one:

- The resulting commit's authorship log MUST contain prompt records from ALL squashed commits
- Line attributions MUST be calculated against the final file state
- Session hashes from all contributing commits MUST be preserved
- If the same lines were modified by different sessions, the LAST session's attribution wins

```
Squashing B, C, D into single commit S:

S's authorship log contains:
  - All prompts from B, C, D
  - Line attributions reflecting final state after all changes
  - Multiple session hashes if different AI sessions contributed
```

#### Interactive Rebase: Splitting Commits (1 → N)

When a single commit is split into multiple commits:

- The original commit's authorship data MUST be distributed across the new commits
- Each new commit MUST only contain attributions for lines present in THAT commit's diff
- Prompt records MAY be duplicated across commits if the same session contributed to multiple splits
- When content from the original commit reappears in a later split, implementations MUST restore its original attribution

```
Splitting D into D1, D2, D3:

D1's authorship: lines 1-10 from D's original authorship
D2's authorship: lines 11-20 from D's original authorship
D3's authorship: lines 21-30 from D's original authorship
```

#### Interactive Rebase: Dropping Commits

When commits are dropped (removed from the rebase):

- Authorship logs for dropped commits MUST NOT be attached to any new commits
- If dropped content reappears in later commits (via conflict resolution or manual edits), it SHOULD be attributed to the human author, not the original AI session
- Implementations MUST NOT create authorship notes for commits that no longer exist

#### Interactive Rebase: Editing Commits

When a commit is edited during interactive rebase (`edit`):

- If the edit modifies AI-attributed lines, those lines SHOULD be re-attributed to the human
- If the edit adds new content, that content follows normal attribution rules
- The original session's prompt record MUST be preserved (for audit trail)
- The `overriden_lines` counter SHOULD be incremented for lines modified by the human

#### Amending During Rebase

When `git commit --amend` is used during a rebase:

- The amended commit's authorship MUST reflect the combined changes
- If the amend includes new AI-generated content, that session MUST be added to prompts
- If the amend removes AI-generated lines, those lines MUST be removed from attestations
- The `base_commit_sha` MUST reference the amended commit's parent

#### Conflict Resolution

When conflicts occur during rebase:

- Implementations MUST wait until the conflict is resolved and the rebase continues
- Conflict resolution changes made by humans SHOULD NOT be attributed to AI
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

#### Abort and Failure Handling

When a rebase is aborted (`git rebase --abort`):

- Implementations MUST NOT create any new authorship notes
- The original commits retain their original authorship logs (unchanged)
- Any partial authorship state MUST be discarded

When a rebase fails mid-operation:

- Implementations SHOULD log the failure for debugging
- No authorship notes SHOULD be written for incomplete rebases
- Recovery is handled when the user either continues or aborts

#### Edge Cases

**Empty Commits**: If a rebase results in empty commits (no changes), those commits:
- MAY have empty authorship logs (no attestations)
- SHOULD still have the metadata section with `base_commit_sha`

**No AI Content**: If rebased commits contain no AI-attributed content:
- Implementations MAY skip authorship processing entirely
- No authorship notes are required for purely human-authored commits

**Commits Already Have Notes**: When processing new commits, if a commit already has an authorship log (from the target branch):
- Implementations MUST skip that commit
- Only newly created commits from the rebase need processing

**Merge Commits in Rebase**: If a rebase includes merge commits:
- The merge commit's authorship reflects the resolution, not the merged content
- Implementations SHOULD handle these as special cases with potentially empty attestations

---

### 2.2 Merge

A merge combines changes from one branch into another. Implementations MUST preserve AI authorship attribution through all merge scenarios.

#### Core Principles

1. **Working State Preservation**: For merge operations that leave changes uncommitted (e.g., `merge --squash`), AI attributions MUST be moved from committed authorship logs to the implementation's working state so they appear in Authorship Logs after the next commit
1. **Prompt Preservation**: All prompt records from merged commits MUST be preserved

#### Standard Merge

When a merge creates a merge commit:

- The merged commits retain their authorship logs in history (no action needed)
- The merge commit's authorship log MUST only contain attributions for conflict resolution changes
- If conflicts were resolved with AI assistance, that MUST be tracked as a new session
- If conflicts were resolved manually, those changes SHOULD be attributed to the human resolver
- If no conflicts occurred, the merge commit MAY have an empty authorship log (no attestations)
- The `base_commit_sha` field MUST reference the merge commit itself

#### Merge --squash

When `git merge --squash` is used, the merge leaves changes staged but uncommitted:

- **AI attributions MUST be moved from committed authorship logs to the implementation's working state**
- When the user commits, all accurate AI attributions from the source branch will appear in the new commit's authorship log. 
- Prompt records from all squashed commits MUST be preserved

```
Before merge --squash:
  main: A → B → C
  feature: D → E → F (with AI attributions)

After merge --squash (before commit):
  - Changes from D, E, F are staged
  - AI attributions from D, E, F are in working state (INITIAL)
  
After commit:
  - New commit G contains all changes
  - G's authorship log contains attributions from D, E, F
```

#### Conflict Resolution

When conflicts occur during merge:

- Implementations MUST wait until the conflict is resolved and the merge completes
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

---

### 2.3 Reset

A reset moves HEAD to a different commit, potentially discarding commits. Implementations MUST preserve AI authorship attribution by moving it to working state when commits are unwound.

#### Core Principles

1. **Working State Migration**: AI attributions from "unwound" commits MUST be moved from committed authorship logs to the implementation's working state

#### Reset --soft

When `git reset --soft` is used:

- HEAD moves to the target commit, but the index and working directory remain unchanged
- **AI attributions from unwound commits MUST be moved to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

#### Reset --mixed (Default)

When `git reset --mixed` (or `git reset`) is used:

- HEAD and the index move to the target commit, but the working directory remains unchanged
- **AI attributions from unwound commits MUST be moved to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

#### Reset --hard

When `git reset --hard` is used:

- HEAD, index, and working directory all move to the target commit
- AI Attributions in your implementation's working state MUST be cleared 
- AI Authorship Notes SHOULD NOT be deleted. 

#### Partial Reset

When reset is used with pathspecs (e.g., `git reset HEAD -- file.txt`):

- Only specified files are reset
- **AI attributions for reset files MUST be moved from committed authorship logs to the implementation's working state**
- Other files' attributions remain unchanged
- The working log MUST be updated accordingly

```
Before reset --soft:
  HEAD: A → B → C (with AI attributions in C)
  
After reset --soft to A:
  HEAD: A
  Index: Contains changes from B and C
  Working log: Contains INITIAL attributions from B and C
  
After commit:
  New commit D contains changes from B and C
  D's authorship log contains attributions from B and C
```

---

### 2.4 Cherry-pick

A cherry-pick applies changes from one or more commits to the current branch. Implementations MUST preserve AI authorship attribution through cherry-pick operations.

#### Core Principles

1. **SHA Independence**: When a commit is cherry-picked, it gets a new SHA. The authorship log MUST be copied to the new commit
2. **Content-Based Attribution**: Line attributions MUST reflect the actual content at the new commit location
3. **Working State for Uncommitted**: When cherry-pick is used with `--no-commit`, AI attributions MUST be moved to working state

#### Standard Cherry-pick (With Commit)

When `git cherry-pick` creates a new commit:

- The new commit's authorship log MUST contain attributions from the source commit
- Line numbers MUST be recalculated based on the file state at the new commit location
- Prompt records from the source commit MUST be preserved
- The `base_commit_sha` field MUST reference the new commit

#### Cherry-pick --no-commit

When `git cherry-pick --no-commit` is used:

- Changes are applied to the working directory and index but not committed
- **AI attributions from the source commit(s) MUST be moved from committed authorship logs to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

```
Before cherry-pick --no-commit:
  Current branch: A → B
  Source commit: C (with AI attributions)
  
After cherry-pick --no-commit:
  Changes from C are staged
  Working log: Contains INITIAL attributions from C
  
After commit:
  New commit D contains changes from C
  D's authorship log contains attributions from C
```

#### Multiple Cherry-picks

When multiple commits are cherry-picked:

- Each new commit MUST have its own authorship log
- Attributions MUST be calculated based on the sequential application of changes
- Prompt records from all source commits MUST be preserved

#### Conflict Resolution

When conflicts occur during cherry-pick:

- Implementations MUST wait until the conflict is resolved and the cherry-pick continues
- Conflict resolution changes made by humans SHOULD NOT be attributed to AI
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

---

### 2.5 Stash / Pop

Stash operations temporarily save working directory changes. Implementations MUST preserve AI authorship attribution through stash and pop operations.

#### Core Principles

1. **Working State Preservation**: When stashing, AI attributions from the working log MUST be saved with the stash
2. **Attribution Restoration**: When popping/applying a stash, AI attributions MUST be restored to the working state
3. **Working State Migration**: **AI attributions MUST be moved from committed authorship logs (if any) to the implementation's working state when stashing, and restored to working state when popping**

#### Stash Push / Save

When `git stash` (or `git stash push` / `git stash save`) is used:

- The current working log's AI attributions MUST be saved as an authorship log in git notes (under `refs/notes/ai-stash`)
- The authorship log MUST be associated with the stash commit SHA
- The working log entries for stashed files MUST be removed from the current working state
- If pathspecs are specified, only attributions for matching files are saved

#### Stash Pop

When `git stash pop` is used:

- The stash's authorship log MUST be read from git notes (`refs/notes/ai-stash`)
- **AI attributions from the stash MUST be moved to the implementation's working state**
- The working log MUST be updated with these attributions
- When the user commits, these attributions will appear in the new commit's authorship log
- The stash's authorship log note MAY be deleted after successful pop

#### Stash Apply

When `git stash apply` is used:

- The stash's authorship log MUST be read from git notes (`refs/notes/ai-stash`)
- **AI attributions from the stash MUST be moved to the implementation's working state **
- The working log MUST be updated with these attributions
- When the user commits, these attributions will appear in the new commit's authorship log
- The stash's authorship log note is preserved (unlike pop)

#### Stash with Pathspecs

When stashing specific files (e.g., `git stash push -- file.txt`):

- Only attributions for the specified files are saved
- Only those files' working log entries are removed
- When popping/applying, only those files' attributions are restored

```
Before stash:
  Working log: Contains INITIAL attributions for file1.txt and file2.txt
  
After stash:
  Stash commit created with SHA abc123
  Git note at refs/notes/ai-stash/abc123 contains authorship log
  Working log: Empty (files were stashed)
  
After stash pop:
  Changes from stash are applied
  Working log: Contains INITIAL attributions from stash
  Git note may be deleted
  
After commit:
  New commit contains changes from stash
  Commit's authorship log contains attributions from stash
```

---

### 2.6 Amend

An amend modifies the most recent commit, creating a new commit with a different SHA. Implementations MUST preserve AI authorship attribution through amend operations.

#### Core Principles

1. **SHA Independence**: When a commit is amended, it gets a new SHA. The authorship log MUST be moved to the new commit
2. **Working State Integration**: AI attributions from the original commit's authorship log and any uncommitted working state MUST be combined
3. **Content-Based Attribution**: Line attributions MUST reflect the actual content at the amended commit

#### Standard Amend

When `git commit --amend` is used:

- The original commit's authorship log MUST be read
- Any uncommitted AI attributions from the working log MUST be included
- The new commit's authorship log MUST reflect the combined state
- The `base_commit_sha` field MUST reference the amended commit (which is the new commit SHA)
- The original commit's authorship log note SHOULD be removed (since the commit no longer exists)

#### Amend with New AI Content

When amend includes new AI-generated content:

- The new AI session MUST be added to the prompts
- Attributions for new content MUST be added to the attestations
- Existing attributions MUST be preserved unless lines were modified

#### Amend Removing AI Content

When amend removes AI-generated lines:

- Those lines MUST be removed from attestations
- Prompt records SHOULD be preserved (for audit trail)
- The `accepted_lines` counter SHOULD be updated

#### Amend Modifying AI Content

When amend modifies AI-attributed lines:

- Those lines SHOULD be re-attributed to the human (if modified by human)
- The `overriden_lines` counter SHOULD be incremented
- Original prompt records MUST be preserved (for audit trail)

```
Before amend:
  Commit A (with authorship log)
  Working log: Contains INITIAL attributions for new changes
  
After amend:
  New commit A' (different SHA)
  A''s authorship log: Contains attributions from A + working log
  Original A's authorship log note is removed
```

#### Amend During Other Operations

When amend is used during a rebase or other operation:

- The amend operation MUST be processed after the base operation completes
- Attributions MUST reflect the state after both operations
- See section 2.1.7 for details on amending during rebase

---

## 3. Backwards Compatibility

- Implementations of 3.0.0 or later SHOULD NOT attempt to process earlier versions
- Implementations > 3.0.0 MUST process earlier versions, provided they are valid and match the schema they advertise

### Errata

E-001: The field name overriden_lines was introduced as a typographical error in v3.0.0 and shipped in the reference git-ai implementation, where it became canonical. In v4.x, this field WILL be renamed to overridden_lines.

