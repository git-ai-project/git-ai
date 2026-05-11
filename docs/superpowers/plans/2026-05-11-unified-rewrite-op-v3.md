# Unified Rewrite Op v3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ALL existing rewrite/rebase authorship logic (~5000 lines across 5+ separate code paths) with a single unified `handle_rewrite_op_v3()` function that uses `git range-diff` for commit mapping and `diff_based_line_attribution_transfer` for hunk-level attribution transfer.

**Architecture:** All rewrite operations (rebase, cherry-pick, amend, squash, reset, ref-update) produce a single `(onto, original_head, new_head)` triple. `git range-diff onto..original onto..new` maps commits (handling squash, split, delete, identical, modified). For each modified pair, `git diff-tree -p` provides file diffs and `diff_based_line_attribution_transfer()` (already implemented in imara-diff) transfers line attributions through hunks. The rewrite_log JSONL file is eliminated entirely.

**Tech Stack:** Rust, git CLI plumbing (`range-diff`, `diff-tree`, `cat-file`), existing imara-diff integration

---

## File Structure

### New Files
- `src/authorship/rewrite_op_v3.rs` — The single unified rewrite handler (~400 lines)
- `src/git/range_diff.rs` — git range-diff output parser (~200 lines)

### Modified Files
- `src/authorship/mod.rs` — Add `pub mod rewrite_op_v3;`, remove `pub mod rebase_authorship;`
- `src/git/mod.rs` — Add `pub mod range_diff;`, remove `pub mod rewrite_log;`
- `src/daemon.rs` — Replace all `apply_rewrite_side_effect` / event synthesis with single call to v3
- `src/git/repo_storage.rs` — Remove rewrite_log file management, keep working_log
- `src/commands/hooks/rebase_hooks.rs` — Delete entirely (move `build_rebase_commit_mappings` removal)

### Deleted Files
- `src/authorship/rebase_authorship.rs` (4782 lines) — Replaced by rewrite_op_v3.rs
- `src/git/rewrite_log.rs` (710 lines) — Eliminated entirely
- `src/commands/hooks/rebase_hooks.rs` (166 lines) — No longer needed

### Test Files (Modified — assertions stay, internals change)
- All 15 integration test files in `tests/integration/` — Should continue to pass as-is since they test end-to-end behavior
- `tests/integration/repo_storage_unit.rs` — Remove rewrite_log-specific tests
- Snapshot files may need `cargo insta accept` if note format changes slightly

---

## Task 1: Implement `git range-diff` Parser

**Files:**
- Create: `src/git/range_diff.rs`
- Modify: `src/git/mod.rs`

- [ ] **Step 1: Write the range-diff data model and parser**

```rust
// src/git/range_diff.rs

use crate::error::GitAiError;
use crate::git::repository::Repository;

/// How a commit mapped through the rewrite operation.
#[derive(Debug, Clone, PartialEq)]
pub enum MappingKind {
    /// Commit is unchanged (patch-identical). Note can be copied as-is.
    Identical,
    /// Commit was modified (content changed). Needs hunk-level attribution transfer.
    Modified,
    /// Commit was deleted (dropped during rebase/squash). Attribution lost.
    Deleted,
    /// Commit was added (new commit not in original range — e.g., conflict resolution commit).
    Added,
}

/// A single commit mapping produced by `git range-diff`.
#[derive(Debug, Clone)]
pub struct CommitMapping {
    pub kind: MappingKind,
    /// Original commit SHA (None for Added commits).
    pub original: Option<String>,
    /// New commit SHA (None for Deleted commits).
    pub new: Option<String>,
}

/// Run `git range-diff` and parse the output into commit mappings.
///
/// The three-dot form: `git range-diff onto..original_head onto..new_head`
/// This compares the commit ranges and produces a 1:1 (or N:M) mapping.
pub fn run_range_diff(
    repo: &Repository,
    onto: &str,
    original_head: &str,
    new_head: &str,
) -> Result<Vec<CommitMapping>, GitAiError> {
    // Handle degenerate case: if original == new, no rewrite happened
    if original_head == new_head {
        return Ok(Vec::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("range-diff".to_string());
    args.push("--no-color".to_string());
    args.push("--no-notes".to_string());
    args.push(format!("{}..{}", onto, original_head));
    args.push(format!("{}..{}", onto, new_head));

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    parse_range_diff_output(&stdout)
}

/// Parse the raw output of `git range-diff --no-color`.
///
/// Format of each line:
///   `N:  <sha> = M:  <sha> <subject>`   — identical
///   `N:  <sha> ! M:  <sha> <subject>`   — modified
///   `N:  <sha> < -:  ------- <subject>` — deleted (only in original range)
///   `-:  ------- > M:  <sha> <subject>` — added (only in new range)
///
/// We only care about the mapping indicator (=, !, <, >) and the SHAs.
pub fn parse_range_diff_output(output: &str) -> Result<Vec<CommitMapping>, GitAiError> {
    let mut mappings = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("    ") {
            // Skip blank lines and indented diff content
            continue;
        }

        // Match the line structure: `<left> <op> <right> <subject>`
        // Left part: `N:  <sha>` or `-:  -------`
        // Op: =, !, <, >
        // Right part: `M:  <sha>` or `-:  -------`
        if let Some(mapping) = parse_range_diff_line(line) {
            mappings.push(mapping);
        }
    }

    Ok(mappings)
}

fn parse_range_diff_line(line: &str) -> Option<CommitMapping> {
    // The format is:  `<num>:  <sha> <op> <num>:  <sha> <subject>`
    // or with dashes:  `-:  ------- <op> <num>:  <sha> <subject>`
    //
    // We split on whitespace and look for the operator token.
    let parts: Vec<&str> = line.splitn(6, ' ').collect();
    if parts.len() < 4 {
        return None;
    }

    // Find the operator by scanning for =, !, <, > as standalone tokens
    // The structure is: [left_num, left_sha, operator, right_num, right_sha, ...]
    // But spacing varies, so let's use a regex-like approach.
    
    // More robust: find first occurrence of ` = `, ` ! `, ` < `, ` > ` as word boundaries
    let (op, left_part, right_part) = if let Some(idx) = line.find(" = ") {
        ('=', &line[..idx], &line[idx + 3..])
    } else if let Some(idx) = line.find(" ! ") {
        ('!', &line[..idx], &line[idx + 3..])
    } else if let Some(idx) = line.find(" < ") {
        ('<', &line[..idx], &line[idx + 3..])
    } else if let Some(idx) = line.find(" > ") {
        ('>', &line[..idx], &line[idx + 3..])
    } else {
        return None;
    };

    let left_sha = extract_sha_from_range_diff_part(left_part);
    let right_sha = extract_sha_from_range_diff_part(right_part);

    let mapping = match op {
        '=' => CommitMapping {
            kind: MappingKind::Identical,
            original: left_sha,
            new: right_sha,
        },
        '!' => CommitMapping {
            kind: MappingKind::Modified,
            original: left_sha,
            new: right_sha,
        },
        '<' => CommitMapping {
            kind: MappingKind::Deleted,
            original: left_sha,
            new: None,
        },
        '>' => CommitMapping {
            kind: MappingKind::Added,
            original: None,
            new: right_sha,
        },
        _ => return None,
    };

    Some(mapping)
}

/// Extract a 40-char hex SHA from a range-diff part like `1:  abc123def...`
fn extract_sha_from_range_diff_part(part: &str) -> Option<String> {
    // Skip the `N:  ` prefix and find the SHA
    for word in part.split_whitespace() {
        if word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(word.to_string());
        }
    }
    None
}
```

- [ ] **Step 2: Register the module**

In `src/git/mod.rs`, add:
```rust
pub mod range_diff;
```

- [ ] **Step 3: Write unit tests for the parser**

Add to the bottom of `src/git/range_diff.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_identical_mapping() {
        let output = "1:  abc1234 = 1:  def5678 Some commit message\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_modified_mapping() {
        let output = "1:  abc1234 ! 1:  def5678 Modified commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
    }

    #[test]
    fn parse_deleted_mapping() {
        let output = "1:  abc1234 < -:  ------- Dropped commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Deleted);
        assert!(mappings[0].original.is_some());
        assert!(mappings[0].new.is_none());
    }

    #[test]
    fn parse_added_mapping() {
        let output = "-:  ------- > 1:  def5678 New commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Added);
        assert!(mappings[0].original.is_none());
        assert!(mappings[0].new.is_some());
    }

    #[test]
    fn parse_multi_commit_rebase() {
        let output = "\
1:  aaa1111 = 1:  bbb1111 First commit
2:  aaa2222 ! 2:  bbb2222 Second commit (modified)
3:  aaa3333 < -:  ------- Third commit (dropped)
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[1].kind, MappingKind::Modified);
        assert_eq!(mappings[2].kind, MappingKind::Deleted);
    }

    #[test]
    fn parse_skips_indented_diff_content() {
        let output = "\
1:  abc1234 ! 1:  def5678 Modified commit
    diff --git a/file.txt b/file.txt
    --- a/file.txt
    +++ b/file.txt
    @@ -1,3 +1,3 @@
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
    }

    #[test]
    fn parse_empty_output() {
        let mappings = parse_range_diff_output("").unwrap();
        assert!(mappings.is_empty());
    }
}
```

- [ ] **Step 4: Run unit tests**

Run: `task test TEST_FILTER=range_diff`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/git/range_diff.rs src/git/mod.rs
git commit -m "feat: add git range-diff parser for unified rewrite op v3"
```

---

## Task 2: Implement the Unified Rewrite Handler

**Files:**
- Create: `src/authorship/rewrite_op_v3.rs`
- Modify: `src/authorship/mod.rs`

This is the core function that replaces `rewrite_authorship_after_rebase_v2`, `rewrite_authorship_after_cherry_pick`, `rewrite_authorship_after_commit_amend`, and `prepare_working_log_after_squash`.

- [ ] **Step 1: Write the unified handler**

```rust
// src/authorship/rewrite_op_v3.rs

use std::collections::HashMap;

use crate::error::GitAiError;
use crate::git::range_diff::{CommitMapping, MappingKind, run_range_diff};
use crate::git::repository::Repository;

/// The only input a rewrite operation needs.
#[derive(Debug, Clone)]
pub struct RewriteTriple {
    /// The common ancestor / base of both ranges.
    pub onto: String,
    /// The tip of the original (pre-rewrite) commit range.
    pub original_head: String,
    /// The tip of the new (post-rewrite) commit range.
    pub new_head: String,
}

/// Unified handler for ALL rewrite operations.
///
/// This single function replaces:
/// - `rewrite_authorship_after_rebase_v2`
/// - `rewrite_authorship_after_cherry_pick`
/// - `rewrite_authorship_after_commit_amend`
/// - `prepare_working_log_after_squash` + `prepare_working_log_after_squash_from_final_state`
/// - `build_rebase_commit_mappings` + `pair_commits_for_rewrite`
/// - `try_fast_path_rebase_note_remap_cached`
/// - `stable_rebase_heads_from_worktree`
///
/// Algorithm:
/// 1. `git range-diff onto..original onto..new` → commit mappings
/// 2. For each mapping:
///    - Identical: copy note from original SHA to new SHA
///    - Modified: load old note → diff-tree → transfer attributions via hunks → write new note
///    - Deleted: no-op (attribution dropped with the commit)
///    - Added: no-op (new commits get attribution via normal post-commit flow)
/// 3. Migrate working log from original_head to new_head
pub fn handle_rewrite_op_v3(
    repo: &Repository,
    triple: &RewriteTriple,
) -> Result<(), GitAiError> {
    if triple.original_head == triple.new_head {
        return Ok(());
    }

    // Step 1: Get commit mappings via range-diff
    let mappings = run_range_diff(repo, &triple.onto, &triple.original_head, &triple.new_head)?;

    if mappings.is_empty() {
        tracing::debug!(
            "rewrite_v3: range-diff produced no mappings for {}..{} vs {}..{}",
            triple.onto, triple.original_head, triple.onto, triple.new_head
        );
        return Ok(());
    }

    // Step 2: Fetch any missing remote notes for original commits before processing
    let original_commits: Vec<String> = mappings
        .iter()
        .filter_map(|m| m.original.clone())
        .collect();
    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &original_commits);

    // Step 3: Detect squash pattern (N Deleted pointing to originals + 1 Modified/Added new)
    // and process each mapping accordingly
    let squash_groups = detect_squash_groups(&mappings);

    for mapping in &mappings {
        // Skip Deleted commits that are part of a squash group — handled by squash transfer
        if mapping.kind == MappingKind::Deleted {
            if let Some(original) = &mapping.original {
                if squash_groups.values().any(|group| group.deleted_originals.contains(original)) {
                    continue;
                }
            }
            // Standalone delete (commit dropped, not squashed) — attribution lost
            continue;
        }

        match mapping.kind {
            MappingKind::Identical => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    copy_note(repo, original, new)?;
                }
            }
            MappingKind::Modified => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    if let Some(group) = squash_groups.get(new.as_str()) {
                        // Squash: diff against ALL originals and union attributions
                        transfer_attribution_squash(repo, &group.all_originals(), new)?;
                    } else {
                        transfer_attribution_via_diff(repo, original, new)?;
                    }
                }
            }
            MappingKind::Added => {
                if let Some(new) = &mapping.new {
                    if let Some(group) = squash_groups.get(new.as_str()) {
                        // Squash result appears as Added when all originals show as Deleted
                        transfer_attribution_squash(repo, &group.all_originals(), new)?;
                    }
                    // Otherwise: new commit, attribution from post-commit flow
                }
            }
            MappingKind::Deleted => unreachable!(), // handled above
        }
    }

    // Step 4: Migrate working log
    migrate_working_log(&repo, &triple.original_head, &triple.new_head)?;

    tracing::debug!(
        "rewrite_v3: processed {} commit mappings ({} identical, {} modified, {} deleted, {} added)",
        mappings.len(),
        mappings.iter().filter(|m| m.kind == MappingKind::Identical).count(),
        mappings.iter().filter(|m| m.kind == MappingKind::Modified).count(),
        mappings.iter().filter(|m| m.kind == MappingKind::Deleted).count(),
        mappings.iter().filter(|m| m.kind == MappingKind::Added).count(),
    );

    Ok(())
}

/// Copy an authorship note from one commit to another (for identical commits).
fn copy_note(repo: &Repository, from_sha: &str, to_sha: &str) -> Result<(), GitAiError> {
    let note_content = match repo.notes_show("refs/notes/ai", from_sha) {
        Ok(content) => content,
        Err(_) => return Ok(()), // No note to copy
    };
    repo.notes_add("refs/notes/ai", to_sha, &note_content, true)?;
    Ok(())
}

/// Transfer attribution from an original commit to a modified commit using hunk-level diff.
fn transfer_attribution_via_diff(
    repo: &Repository,
    original_sha: &str,
    new_sha: &str,
) -> Result<(), GitAiError> {
    // Load the authorship note for the original commit
    let note_content = match repo.notes_show("refs/notes/ai", original_sha) {
        Ok(content) => content,
        Err(_) => {
            // No authorship note on the original — nothing to transfer.
            // This is normal for commits that had no AI involvement.
            return Ok(());
        }
    };

    // Parse the authorship log to extract file attestations and metadata
    let authorship_log = match crate::authorship::authorship_log::AuthorshipLog::deserialize(
        &note_content,
    ) {
        Ok(log) => log,
        Err(_) => return Ok(()), // Malformed note, skip
    };

    // Get the list of files that have attestations (AI-touched files)
    let attested_files: Vec<String> = authorship_log.file_paths_with_attestations();
    if attested_files.is_empty() {
        // Metadata-only note (prompts but no file attestations) — just copy it
        repo.notes_add("refs/notes/ai", new_sha, &note_content, true)?;
        return Ok(());
    }

    // For each attested file, get the content before and after, then transfer attributions
    let mut new_attestations: HashMap<String, Vec<crate::authorship::attribution_tracker::LineAttribution>> = HashMap::new();
    let mut any_transferred = false;

    for file_path in &attested_files {
        // Get file content at original commit and new commit
        let old_content = blob_content_at_commit(repo, original_sha, file_path);
        let new_content = blob_content_at_commit(repo, new_sha, file_path);

        match (old_content, new_content) {
            (Some(old), Some(new)) => {
                // File exists in both — transfer attributions through diff
                let old_attrs = authorship_log.line_attributions_for_file(file_path);
                if old_attrs.is_empty() {
                    continue;
                }
                let new_attrs = crate::authorship::rebase_authorship::diff_based_line_attribution_transfer(
                    &old,
                    &new,
                    &old_attrs,
                );
                if !new_attrs.is_empty() {
                    new_attestations.insert(file_path.clone(), new_attrs);
                    any_transferred = true;
                }
            }
            (Some(_), None) => {
                // File deleted in new commit — attribution dropped
            }
            (None, Some(_)) => {
                // File added in new commit — no old attribution to transfer
            }
            (None, None) => {
                // File doesn't exist in either — skip
            }
        }
    }

    if !any_transferred && attested_files.is_empty() {
        return Ok(());
    }

    // Build the new authorship note with transferred attestations + preserved metadata
    let new_note = authorship_log.rebuild_with_new_attestations(
        new_sha,
        &new_attestations,
    );
    let serialized = new_note.serialize();
    repo.notes_add("refs/notes/ai", new_sha, &serialized, true)?;

    Ok(())
}

/// Get file content at a specific commit SHA.
fn blob_content_at_commit(repo: &Repository, commit_sha: &str, file_path: &str) -> Option<String> {
    let spec = format!("{}:{}", commit_sha, file_path);
    match repo.cat_file_content(&spec) {
        Ok(content) => Some(content),
        Err(_) => None,
    }
}

/// A group of commits that were squashed into a single new commit.
struct SquashGroup {
    /// The commit that range-diff matched as Modified (most similar original)
    matched_original: Option<String>,
    /// The remaining originals that appear as Deleted
    deleted_originals: Vec<String>,
}

impl SquashGroup {
    fn all_originals(&self) -> Vec<String> {
        let mut all = self.deleted_originals.clone();
        if let Some(matched) = &self.matched_original {
            all.push(matched.clone());
        }
        all
    }
}

/// Detect squash patterns: consecutive Deleted commits followed by a Modified/Added
/// that maps to a single new commit. Returns map of new_sha → SquashGroup.
fn detect_squash_groups(mappings: &[CommitMapping]) -> HashMap<&str, SquashGroup> {
    // Heuristic: if there are more originals than news, it's likely a squash.
    // Group consecutive Deleted commits that precede a Modified commit.
    let mut groups: HashMap<&str, SquashGroup> = HashMap::new();
    let mut pending_deleted: Vec<String> = Vec::new();

    for mapping in mappings {
        match mapping.kind {
            MappingKind::Deleted => {
                if let Some(original) = &mapping.original {
                    pending_deleted.push(original.clone());
                }
            }
            MappingKind::Modified | MappingKind::Added => {
                if !pending_deleted.is_empty() {
                    if let Some(new) = &mapping.new {
                        groups.insert(new.as_str(), SquashGroup {
                            matched_original: mapping.original.clone(),
                            deleted_originals: std::mem::take(&mut pending_deleted),
                        });
                    }
                }
                pending_deleted.clear();
            }
            MappingKind::Identical => {
                pending_deleted.clear();
            }
        }
    }
    groups
}

/// Transfer attribution from multiple original commits into a single squash result.
/// Diffs the squash result against each original and unions the transferred attributions.
fn transfer_attribution_squash(
    repo: &Repository,
    originals: &[String],
    new_sha: &str,
) -> Result<(), GitAiError> {
    let mut all_file_attrs: HashMap<String, Vec<crate::authorship::attribution_tracker::LineAttribution>> = HashMap::new();

    for original_sha in originals {
        let note_content = match repo.notes_show("refs/notes/ai", original_sha) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let authorship_log = match crate::authorship::authorship_log::AuthorshipLog::deserialize(&note_content) {
            Ok(log) => log,
            Err(_) => continue,
        };

        for file_path in authorship_log.file_paths_with_attestations() {
            let old_content = blob_content_at_commit(repo, original_sha, &file_path);
            let new_content = blob_content_at_commit(repo, new_sha, &file_path);
            if let (Some(old), Some(new)) = (old_content, new_content) {
                let old_attrs = authorship_log.line_attributions_for_file(&file_path);
                if old_attrs.is_empty() {
                    continue;
                }
                let transferred = diff_based_line_attribution_transfer(&old, &new, &old_attrs);
                // Union: merge with existing attributions for this file
                let entry = all_file_attrs.entry(file_path).or_default();
                for attr in transferred {
                    // Only add if no existing attribution covers this line range
                    if !entry.iter().any(|existing| existing.start_line == attr.start_line) {
                        entry.push(attr);
                    }
                }
            }
        }
    }

    if all_file_attrs.is_empty() {
        return Ok(());
    }

    // Build and write the combined note
    // (Reuse metadata from the first original that has a note)
    let base_note = originals.iter()
        .find_map(|sha| repo.notes_show("refs/notes/ai", sha).ok())
        .and_then(|content| crate::authorship::authorship_log::AuthorshipLog::deserialize(&content).ok());

    if let Some(base) = base_note {
        let new_note = base.rebuild_with_new_attestations(new_sha, &all_file_attrs);
        repo.notes_add("refs/notes/ai", new_sha, &new_note.serialize(), true)?;
    }

    Ok(())
}

/// Migrate working log from original_head to new_head.
/// Rebase rewrites commit SHAs, but working logs are keyed by SHA.
/// Without migration, uncommitted attributions are orphaned.
fn migrate_working_log(
    repo: &Repository,
    original_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    if original_head == new_head {
        return Ok(());
    }

    if !repo.storage.has_working_log(original_head) {
        return Ok(());
    }

    if !repo.storage.has_working_log(new_head) {
        repo.storage.rename_working_log(original_head, new_head)?;
    } else {
        // Both exist: merge INITIAL attributions only, drop old checkpoints
        let old_wl = repo.storage.working_log_for_base_commit(original_head)?;
        let initial = old_wl.read_initial_attributions();
        if !initial.files.is_empty() {
            let new_wl = repo.storage.working_log_for_base_commit(new_head)?;
            new_wl.write_initial(initial)?;
        }
        repo.storage.delete_working_log_for_base_commit(original_head)?;
    }

    Ok(())
}
```

- [ ] **Step 2: Register the module**

In `src/authorship/mod.rs`, add:
```rust
pub mod rewrite_op_v3;
```

- [ ] **Step 3: Verify it compiles**

Run: `task build`
Expected: Compilation succeeds (the function references some types from the old module that we'll need to adapt — see Step 4).

- [ ] **Step 4: Fix compilation — adapt to actual AuthorshipLog API**

The `AuthorshipLog` type likely doesn't have `file_paths_with_attestations()`, `line_attributions_for_file()`, or `rebuild_with_new_attestations()` methods yet. We need to check the actual API and adapt. Read `src/authorship/authorship_log.rs` and `src/authorship/authorship_log_serialization.rs` to find the real API, then update `transfer_attribution_via_diff()` to use the actual deserialization and serialization methods that `rebase_authorship.rs` currently uses.

The existing code at `rebase_authorship.rs:1280-1375` shows how it currently loads attributions from notes. Port that approach:
- Parse the note JSON to get attestation entries
- Extract line_attributions per file
- After transfer, rebuild the note JSON with updated attestations

- [ ] **Step 5: Also move `diff_based_line_attribution_transfer` into this module**

Move the function from `src/authorship/rebase_authorship.rs:4175-4234` into `src/authorship/rewrite_op_v3.rs` (or a shared utils module). This is the core hunk transfer algorithm that we keep. It uses `imara_diff_utils::capture_diff_slices` which stays.

- [ ] **Step 6: Run build**

Run: `task build`
Expected: Compiles cleanly.

- [ ] **Step 7: Commit**

```bash
git add src/authorship/rewrite_op_v3.rs src/authorship/mod.rs
git commit -m "feat: implement unified handle_rewrite_op_v3 using range-diff"
```

---

## Task 3: Wire v3 into the Daemon Side-Effect Pipeline

**Files:**
- Modify: `src/daemon.rs`

The daemon currently:
1. Detects semantic events (RebaseComplete, CherryPickComplete, CommitAmend, etc.)
2. Synthesizes `RewriteLogEvent` variants
3. Calls `apply_rewrite_side_effect()` which calls `rewrite_authorship_if_needed()`
4. Each event type dispatches to a different function

We replace ALL of this with:
1. Detect the rewrite triple (onto, original_head, new_head) — same detection logic
2. Call `handle_rewrite_op_v3(repo, &triple)` directly
3. No more `RewriteLogEvent` synthesis, no more event-type dispatch

- [ ] **Step 1: Create a `detect_rewrite_triple()` helper in the daemon**

Replace the existing side-effect synthesis (lines ~6470-6880 in `maybe_apply_side_effects_for_applied_command`) with a function that produces `Option<RewriteTriple>` from semantic events:

```rust
use crate::authorship::rewrite_op_v3::RewriteTriple;

fn detect_rewrite_triple_from_event(
    event: &SemanticEvent,
    cmd: &NormalizedCommand,
    repository: &Repository,
) -> Result<Option<RewriteTriple>, GitAiError> {
    match event {
        SemanticEvent::RebaseComplete { old_head, new_head, .. } => {
            if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
                return Ok(None);
            }
            let onto = repository.merge_base(old_head.clone(), new_head.clone())
                .unwrap_or_else(|_| new_head.clone());
            Ok(Some(RewriteTriple {
                onto,
                original_head: old_head.clone(),
                new_head: new_head.clone(),
            }))
        }
        SemanticEvent::CherryPickComplete { original_head, new_head } => {
            if new_head.is_empty() || original_head == new_head {
                return Ok(None);
            }
            let onto = repository.merge_base(original_head.clone(), new_head.clone())
                .unwrap_or_else(|_| new_head.clone());
            Ok(Some(RewriteTriple {
                onto,
                original_head: original_head.clone(),
                new_head: new_head.clone(),
            }))
        }
        SemanticEvent::CommitAmended { old_head, new_head } => {
            if old_head.is_empty() || new_head.is_empty() {
                return Ok(None);
            }
            // For amend, the "range" is a single commit. onto = parent of old commit.
            let onto = repository.rev_parse(&format!("{}^", old_head))
                .unwrap_or_else(|_| old_head.clone());
            Ok(Some(RewriteTriple {
                onto,
                original_head: old_head.clone(),
                new_head: new_head.clone(),
            }))
        }
        SemanticEvent::Reset { old_head, new_head, .. } => {
            if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
                return Ok(None);
            }
            // Only process if this looks like a rebase-like reset (non-ancestor)
            if is_ancestor_commit(repository, new_head, old_head) {
                return Ok(None);
            }
            let onto = repository.merge_base(old_head.clone(), new_head.clone())
                .unwrap_or_else(|_| new_head.clone());
            Ok(Some(RewriteTriple {
                onto,
                original_head: old_head.clone(),
                new_head: new_head.clone(),
            }))
        }
        SemanticEvent::PullCompleted { strategy, .. } => {
            // Handled by detecting RebaseComplete event that pull emits
            Ok(None)
        }
        SemanticEvent::MergeSquash { base_head, source_head, .. } => {
            // Merge --squash: reconstruct source from git state if not in event.
            // The source_head is the feature branch tip; base_head is current HEAD.
            // The "onto" is base_head (we're squashing source range into base).
            if source_head.is_empty() || base_head.is_empty() {
                return Ok(None);
            }
            Ok(Some(RewriteTriple {
                onto: base_head.clone(),
                original_head: source_head.clone(),
                new_head: base_head.clone(), // will be updated to actual commit SHA post-commit
            }))
        }
        SemanticEvent::RefUpdated { old, new, .. } => {
            if old.is_empty() || new.is_empty() || old == new {
                return Ok(None);
            }
            if !is_valid_oid(old) || is_zero_oid(old) || !is_valid_oid(new) || is_zero_oid(new) {
                return Ok(None);
            }
            if is_ancestor_commit(repository, new, old) {
                return Ok(None);
            }
            let onto = repository.merge_base(old.clone(), new.clone())
                .unwrap_or_else(|_| new.clone());
            Ok(Some(RewriteTriple {
                onto,
                original_head: old.clone(),
                new_head: new.clone(),
            }))
        }
        _ => Ok(None),
    }
}
```

- [ ] **Step 2: Replace the rewrite side-effect dispatch**

In `maybe_apply_side_effects_for_applied_command()`, replace the block that iterates over semantic events and emits `RewriteLogEvent` variants. The new code:

```rust
// Process rewrite operations via unified v3 handler
for event in events {
    if let Some(triple) = detect_rewrite_triple_from_event(event, cmd, &repository)? {
        crate::authorship::rewrite_op_v3::handle_rewrite_op_v3(&repository, &triple)?;
    }
}
```

- [ ] **Step 3: Remove the pending rebase/cherry-pick state tracking**

Delete from `ActorDaemonCoordinator`:
- `pending_rebase_original_head_by_worktree` field and its Mutex
- `pending_cherry_pick_sources_by_worktree` field and its Mutex
- `set_pending_rebase_original_head_for_worktree()`
- `clear_pending_rebase_original_head_for_worktree()`
- `set_pending_cherry_pick_sources_for_worktree()`
- `take_pending_cherry_pick_sources_for_worktree()`
- `clear_pending_cherry_pick_sources_for_worktree()`

These are no longer needed because we don't track conflict state — we just process the final result.

- [ ] **Step 4: Remove `apply_rewrite_side_effect()` function entirely**

Delete the function at daemon.rs:2683-2830. It's replaced by the direct call to `handle_rewrite_op_v3`.

- [ ] **Step 5: Remove all helper functions that are now dead code**

Delete from daemon.rs:
- `maybe_rebase_mappings_from_repository()`
- `processed_rebase_new_heads()`
- `stable_rebase_heads_from_worktree()`
- `resolve_explicit_rebase_branch_ref()`
- `explicit_rebase_branch_ref_name()`
- `resolve_rebase_original_head_for_worktree()`
- `rebase_is_control_mode()`
- `rebase_start_target_hint_from_args()`
- `rebase_start_target_hint_from_command()`
- `strict_rebase_original_head_from_command()`
- `rewrite_event_needs_authorship_processing()`
- `rewrite_log_mentions_commit()`
- `preceding_merge_squash_for_pending_commit()`
- `latest_reset_for_base_commit()`
- `ensure_rewrite_prerequisites()`

- [ ] **Step 6: Simplify the non-zero exit code handling for rebase**

Remove the block at daemon.rs:7060-7086 that sets/clears pending rebase state on failure. With v3, we don't track conflict state at all — the final successful rebase result is what triggers attribution transfer.

- [ ] **Step 7: Verify compilation**

Run: `task build`
Expected: Many compilation errors from removed imports and dead references. Fix them iteratively. The key imports to remove are all references to `RewriteLogEvent`, `RebaseCompleteEvent`, `RebaseStartEvent`, `RebaseAbortEvent`, etc.

- [ ] **Step 8: Commit**

```bash
git add src/daemon.rs
git commit -m "refactor: wire unified rewrite_op_v3 into daemon, remove old dispatch"
```

---

## Task 4: Remove the Rewrite Log

**Files:**
- Delete: `src/git/rewrite_log.rs`
- Modify: `src/git/mod.rs` — remove `pub mod rewrite_log;`
- Modify: `src/git/repo_storage.rs` — remove rewrite_log file creation and methods
- Modify: `src/daemon.rs` — remove all remaining `RewriteLogEvent` references

- [ ] **Step 1: Delete `src/git/rewrite_log.rs`**

```bash
rm src/git/rewrite_log.rs
```

- [ ] **Step 2: Remove module declaration from `src/git/mod.rs`**

Remove:
```rust
pub mod rewrite_log;
```

- [ ] **Step 3: Remove rewrite_log from repo_storage.rs**

Remove from `ensure_config_directory()` the line that creates the rewrite_log file.
Remove `append_rewrite_event()` and `read_rewrite_events()` methods.
Remove the `rewrite_log_path` field if it exists.

- [ ] **Step 4: Remove stash rewrite log dependencies**

The stash system uses the rewrite log to track a stash stack. Replace this:
- `inferred_top_stash_sha_from_rewrite_history()` — delete entirely. The stash SHA at pop/apply time is available directly from trace2 ref-change events in the same daemon side-effect pass. No need to maintain a virtual stack.
- `apply_stash_rewrite_side_effect()` — keep the stash authorship save/restore (`save_stash_authorship_log`, `restore_stash_attributions`) but remove all rewrite_log event recording. These functions store data in git notes on stash SHAs, not the rewrite log, so they work without it.
- Remove `Stash` variant from `RewriteLogEvent` usage in daemon — stash side-effects use the stash SHA from the semantic event directly.

- [ ] **Step 5: Fix all compilation errors**

Run: `task build`
Iterate on remaining references to `RewriteLogEvent`, `RebaseCompleteEvent`, etc. Remove all imports and usages.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "refactor: remove rewrite_log system entirely"
```

---

## Task 5: Delete Old Rebase Authorship Code

**Files:**
- Delete: `src/authorship/rebase_authorship.rs` (4782 lines)
- Delete: `src/commands/hooks/rebase_hooks.rs` (166 lines)
- Modify: `src/authorship/mod.rs` — remove `pub mod rebase_authorship;`
- Modify: `src/commands/hooks/mod.rs` — remove `pub mod rebase_hooks;`

- [ ] **Step 1: Move `diff_based_line_attribution_transfer` to rewrite_op_v3.rs if not already done**

Before deleting rebase_authorship.rs, ensure `diff_based_line_attribution_transfer` (lines 4175-4234) has been moved to the new module. Also move:
- `build_file_attestation_from_line_attributions` (line 3987) if needed by the new code
- `transform_attributions_to_final_state` (line 4559) if needed

These are the pure algorithmic functions that do the actual hunk transfer math.

- [ ] **Step 2: Delete `src/authorship/rebase_authorship.rs`**

```bash
rm src/authorship/rebase_authorship.rs
```

- [ ] **Step 3: Remove module declaration from `src/authorship/mod.rs`**

Remove:
```rust
pub mod rebase_authorship;
```

- [ ] **Step 4: Delete `src/commands/hooks/rebase_hooks.rs`**

```bash
rm src/commands/hooks/rebase_hooks.rs
```

- [ ] **Step 5: Remove module declaration from hooks mod.rs**

Remove from `src/commands/hooks/mod.rs`:
```rust
pub mod rebase_hooks;
```

- [ ] **Step 6: Remove all dead imports and references**

Grep for any remaining references to `rebase_authorship::` or `rebase_hooks::` across the codebase and remove them. Key locations:
- `src/daemon.rs` — imports
- `src/authorship/mod.rs` — re-exports
- Any test files that import these modules directly

- [ ] **Step 7: Fix compilation**

Run: `task build`
Expected: Clean compilation after removing all dead references.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "refactor: delete old rebase_authorship.rs and rebase_hooks.rs (4948 lines removed)"
```

---

## Task 6: Remove Conflict/Abort/Continue Tracking from Daemon

**Files:**
- Modify: `src/daemon.rs`
- Modify: `src/daemon/domain.rs` (if SemanticEvent variants need cleanup)

The daemon currently has special handling for:
- `RebaseAbort` events
- `CherryPickAbort` events  
- `RebaseStart` events (to record original head for --continue)
- `CherryPickStart` events
- Non-zero exit code tracking for multi-step rebases

All of this is eliminated because v3 only processes the FINAL state.

- [ ] **Step 1: Remove SemanticEvent::RebaseAbort handling**

In `maybe_apply_side_effects_for_applied_command()`, remove the `SemanticEvent::RebaseAbort` match arm entirely. We don't care about aborts — if the rebase was aborted, there's no new_head and no attribution to transfer.

- [ ] **Step 2: Remove SemanticEvent::CherryPickAbort handling**

Same as above — remove the match arm.

- [ ] **Step 3: Remove the exit_code != 0 rebase state management block**

The block at ~7060-7086 that detects failed rebases and stores pending state for --continue. Remove it entirely. When rebase --continue succeeds, we get a normal RebaseComplete event with the final (old_head, new_head) — that's all v3 needs.

- [ ] **Step 4: Consider removing RebaseAbort/CherryPickAbort from SemanticEvent enum**

If the normalizer still emits these events from trace2 data, we can keep the enum variants but just ignore them (no-op in the side-effect handler). Or if we can cleanly remove them from the normalizer too, do that.

- [ ] **Step 5: Run build**

Run: `task build`
Expected: Compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "refactor: remove conflict/abort/continue tracking from daemon"
```

---

## Task 7: Update and Fix Integration Tests

**Files:**
- Modify: Various test files in `tests/integration/`
- Possibly modify: `tests/repos/test_repo.rs` (if it has rewrite_log helpers)

The integration tests test END-TO-END behavior: "after rebase, the blame shows correct AI attribution." These tests should pass unchanged because we're replacing internals while preserving the external contract.

- [ ] **Step 1: Run the full test suite**

Run: `task test`
Expected: Some tests may fail due to:
1. Tests that directly assert on rewrite_log contents
2. Tests that depend on specific timing/ordering of daemon events
3. Tests that assert on conflict-state behavior we've removed
4. Snapshot tests that may have slightly different note content

- [ ] **Step 2: Fix `tests/integration/repo_storage_unit.rs`**

Remove any tests that assert on rewrite_log creation or event persistence. These are testing deleted infrastructure.

- [ ] **Step 3: Fix snapshot tests**

Run: `cargo insta review`
Accept any snapshots that changed due to note format differences. The attribution correctness should be the same, but metadata ordering or fields may differ.

- [ ] **Step 4: Fix stash tests if they reference rewrite_log**

`tests/integration/stash_attribution.rs` — if any tests assert on rewrite_log state, update them to only assert on the final attribution result (which is what matters).

- [ ] **Step 5: Address any `range-diff` compatibility issues in tests**

Some test scenarios create repos with very few commits where `range-diff` might behave differently (e.g., initial commits, orphan branches). If tests fail because range-diff can't find a common base, we may need to handle the edge case where `onto` is the null tree or the initial commit.

- [ ] **Step 6: Run full test suite again**

Run: `task test`
Expected: All tests pass.

- [ ] **Step 7: Run lint and format**

Run: `task lint && task fmt`
Expected: No issues.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "test: update integration tests for rewrite_op_v3"
```

---

## Task 8: Cleanup and Final Verification

**Files:**
- Various (dead code removal, unused imports)

- [ ] **Step 1: Grep for any remaining dead code**

```bash
grep -rn "rewrite_log\|RewriteLogEvent\|RebaseCompleteEvent\|rebase_authorship\|rebase_hooks" src/ tests/
```

Remove any remaining references.

- [ ] **Step 2: Remove unused imports throughout**

Run: `task lint`
Fix any unused import warnings.

- [ ] **Step 3: Run the full test suite one final time**

Run: `task test`
Expected: All green.

- [ ] **Step 4: Check line count reduction**

```bash
wc -l src/authorship/rewrite_op_v3.rs src/git/range_diff.rs
```

Expected: ~600 lines total vs the ~5658 lines removed. Net reduction of ~5000 lines.

- [ ] **Step 5: Final commit**

```bash
git add -u
git commit -m "chore: final cleanup after rewrite_op_v3 migration"
```

---

## Risks and Edge Cases

### git range-diff on initial commits
If `onto` is the initial commit (no parent), `git range-diff` may fail. Handle by detecting the case and using `git rev-list --max-parents=0` to find the root, then treating the entire range as a direct diff.

### git range-diff not available
Git 2.19+ required (released 2018). The CI and user environments should all have this. If paranoid, add a version check in `run_range_diff()` that falls back gracefully.

### Many-to-one (squash) in range-diff output
When multiple commits are squashed into one, `git range-diff` shows the squashed commits as Deleted and the squash result as Modified against ONE of them (by patch similarity). Attribution from the "deleted" originals that survived into the squash would be lost if we only diff against the matched original. **Decision: special-case squash.** Detect the N:1 pattern (N Deleted + 1 Modified pointing to a single new SHA), then diff the squash result against ALL originals and union the transferred attributions. This ensures lines from any original commit retain their attribution.

### Performance budget
The 50% budget is easily met:
- 1 `range-diff` call: O(n) commits, typically <100ms
- 1 `cat-file` per file per modified commit: batch-able
- 1 `notes add` per new commit: unavoidable
- No reflog parsing, no blame fallback, no ancestry validation

### Stash attribution (not a rewrite op)
Stash save/restore continues to work via its own mechanism (git notes on stash SHAs). The stash SHA at pop/apply time is available directly from trace2 ref-change events in the same daemon side-effect pass — no persistence or rewrite_log needed.

### Merge --squash source resolution
At commit time after `git merge --squash`, the squash source (feature branch tip) is reconstructed from git state (SQUASH_MSG, reflog, or MERGE_HEAD) rather than holding it in daemon memory or a persistent log. Fully stateless.
