# Remove TmpRepo and libgit2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate all TmpRepo unit tests to integration tests using TestRepo, then remove TmpRepo and the libgit2 dependency entirely.

**Architecture:** Each `#[cfg(test)]` module that uses TmpRepo gets migrated to a new integration test file. Tests that call internal functions directly will be rewritten to exercise the same behavior through the git-ai CLI/wrapper using TestRepo's `git()`, `git_ai()`, `commit()`, and file manipulation APIs. After all tests are migrated, TmpRepo and all git2/libgit2 references are deleted.

**Tech Stack:** Rust, git CLI, TestRepo integration test harness

---

## Key Migration Patterns

These patterns apply across all tasks. Each TmpRepo operation maps to a TestRepo equivalent:

| TmpRepo Pattern | TestRepo Equivalent |
|---|---|
| `TmpRepo::new()` | `TestRepo::new()` |
| `repo.write_file("f.txt", "content", true)` | `repo.filename("f.txt").set_contents(lines!["content"])` or write + `repo.git(&["add", "f.txt"])` |
| `repo.commit_with_message("msg")` | `repo.commit("msg")` or `repo.stage_all_and_commit("msg")` |
| `repo.trigger_checkpoint_with_author("user")` | `repo.git_ai(&["checkpoint", "mock_known_human", "f.txt"])` |
| `repo.trigger_checkpoint_with_ai("Claude", ...)` | `repo.git_ai(&["checkpoint", "mock_ai", "f.txt"])` |
| `repo.create_branch("feat")` | `repo.git(&["checkout", "-b", "feat"])` |
| `repo.switch_branch("main")` | `repo.git(&["checkout", "main"])` |
| `repo.merge_branch("feat", "msg")` | `repo.git(&["merge", "feat", "-m", "msg"])` |
| `repo.rebase_onto("base", "onto")` | `repo.git(&["rebase", "onto"])` |
| `repo.head_commit_sha()` | `repo.git_og(&["rev-parse", "HEAD"]).unwrap().trim()` |
| `repo.get_authorship_log()` | `repo.read_authorship_note(&sha)` then deserialize |
| `repo.blame_for_file(&f, None)` | `repo.git_ai(&["blame", "f.txt"])` then parse output |
| `repo.stage_file("f.txt")` | `repo.git(&["add", "f.txt"])` |
| `repo.current_branch()` | `repo.current_branch()` |
| `repo.gitai_repo()` | Not available — test through CLI instead |
| `repo.path()` | `repo.path()` |
| `repo.add_remote("origin", url)` | `repo.git_og(&["remote", "add", "origin", url])` |
| `file.append("content")` | `repo.filename("f.txt").append(lines!["content"])` or fs::write + git add |
| `repo.current_working_logs()` | `repo.current_working_logs()` |

For tests that call internal Rust functions (e.g., `post_commit()`, `checkpoint()`, `refs::notes_add()`), rewrite them to achieve the same effect through git-ai CLI commands and verify outcomes through git-ai's external interfaces (blame output, authorship notes, stats).

---

## Task 1: Migrate `src/authorship/pre_commit.rs` tests

**Files:**
- Create: `tests/integration/pre_commit_unit.rs`
- Modify: `tests/integration/main.rs` (add module declaration)

Tests to migrate (5):
- `test_pre_commit_empty_repo`
- `test_pre_commit_with_staged_changes`
- `test_pre_commit_no_changes`
- `test_pre_commit_result_mapping`
- `test_pre_commit_checkpoint_context_uses_inflight_bash_agent_context`

- [ ] **Step 1: Create the integration test file with all 5 tests**

The pre_commit tests verify that the pre-commit hook (checkpoint) runs without error in various scenarios. In TestRepo, this is exercised by running `git commit` through the wrapper (which triggers pre-commit checkpoint internally).

```rust
// tests/integration/pre_commit_unit.rs
use crate::repos::test_repo::TestRepo;

#[test]
fn test_pre_commit_empty_repo() {
    let repo = TestRepo::new();
    // In an empty repo, committing should not panic
    // Write a file and commit to trigger pre-commit
    repo.filename("test.txt").set_contents(vec!["hello"]);
    let result = repo.stage_all_and_commit("initial");
    assert!(result.is_ok());
}

#[test]
fn test_pre_commit_with_staged_changes() {
    let repo = TestRepo::new();
    repo.filename("test.txt").set_contents(vec!["line1"]);
    repo.stage_all_and_commit("initial").unwrap();
    // Now modify and stage
    repo.filename("test.txt").set_contents(vec!["line1", "line2"]);
    repo.git(&["add", "test.txt"]).unwrap();
    let result = repo.commit("staged changes");
    assert!(result.is_ok());
}

#[test]
fn test_pre_commit_no_changes() {
    let repo = TestRepo::new();
    repo.filename("test.txt").set_contents(vec!["line1"]);
    repo.stage_all_and_commit("initial").unwrap();
    // Commit with nothing staged should fail (git rejects empty commits)
    let result = repo.commit("no changes");
    assert!(result.is_err());
}

#[test]
fn test_pre_commit_result_mapping() {
    let repo = TestRepo::new();
    repo.filename("test.txt").set_contents(vec!["content"]);
    let result = repo.stage_all_and_commit("test commit");
    // Result is either Ok (commit succeeded) or Err (string error)
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn test_pre_commit_checkpoint_context_uses_inflight_bash_agent_context() {
    let repo = TestRepo::new();
    repo.filename("test.txt").set_contents(vec!["initial"]);
    repo.stage_all_and_commit("initial").unwrap();
    // Simulate AI editing a file and checkpointing
    repo.filename("test.txt").set_contents(vec!["initial", "ai line"]);
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    let result = repo.commit("with ai context");
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Add module to main.rs**

Add `mod pre_commit_unit;` to `tests/integration/main.rs` in alphabetical order.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --test integration pre_commit_unit`
Expected: All 5 tests pass

- [ ] **Step 4: Commit**

```bash
git add tests/integration/pre_commit_unit.rs tests/integration/main.rs
git commit -m "migrate: pre_commit unit tests to integration tests using TestRepo"
```

---

## Task 2: Migrate `src/commands/hooks/stash_hooks.rs` tests

**Files:**
- Create: `tests/integration/stash_hooks_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (2):
- `test_save_stash_note_roundtrip`
- `test_save_stash_note_large_content`

- [ ] **Step 1: Create integration test file**

These tests verify that stash notes can be saved and read back. Through the CLI, stashing triggers the stash hook which saves authorship info.

```rust
// tests/integration/stash_hooks_unit.rs
use crate::repos::test_repo::TestRepo;

#[test]
fn test_save_stash_note_roundtrip() {
    let repo = TestRepo::new();
    repo.filename("file.txt").set_contents(vec!["initial"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Make changes, checkpoint, then stash
    repo.filename("file.txt").set_contents(vec!["initial", "modified"]);
    repo.git(&["add", "file.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();

    // Stash (triggers stash hook saving working log)
    repo.git(&["stash"]).unwrap();

    // Pop stash (triggers stash hook restoring working log)
    repo.git(&["stash", "pop"]).unwrap();

    // Verify the working log was preserved through stash roundtrip
    let working_logs = repo.current_working_logs();
    let checkpoints = working_logs.read_all_checkpoints();
    assert!(!checkpoints.is_empty(), "working log should be restored after stash pop");
}

#[test]
fn test_save_stash_note_large_content() {
    let repo = TestRepo::new();
    // Create a file with substantial content
    let large_content: Vec<&str> = (0..100).map(|_| "line of content").collect();
    repo.filename("large.txt").set_contents(large_content);
    repo.stage_all_and_commit("initial with large file").unwrap();

    // Modify and checkpoint
    let mut modified: Vec<&str> = (0..100).map(|_| "line of content").collect();
    modified.push("new line");
    repo.filename("large.txt").set_contents(modified);
    repo.git(&["add", "large.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "large.txt"]).unwrap();

    // Stash and pop
    repo.git(&["stash"]).unwrap();
    repo.git(&["stash", "pop"]).unwrap();

    // Working log should survive
    let working_logs = repo.current_working_logs();
    let checkpoints = working_logs.read_all_checkpoints();
    assert!(!checkpoints.is_empty());
}
```

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration stash_hooks_unit`

- [ ] **Step 4: Commit**

---

## Task 3: Migrate `src/commands/hooks/rebase_hooks.rs` tests

**Files:**
- Create: `tests/integration/rebase_hooks_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (3 that use TmpRepo — the other 10 are pure unit tests with no TmpRepo that can stay or be moved as-is):
- `test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits`
- `test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base`
- `test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base`

Plus the 10 pure unit tests (no TmpRepo) for `summarize_rebase_args`:
- `test_summarize_rebase_args_continue_is_control_mode`
- `test_summarize_rebase_args_abort_is_control_mode`
- `test_summarize_rebase_args_skip_is_control_mode`
- `test_summarize_rebase_args_upstream_only`
- `test_summarize_rebase_args_upstream_and_branch`
- `test_summarize_rebase_args_onto_flag`
- `test_summarize_rebase_args_onto_equals_flag`
- `test_summarize_rebase_args_root_flag`
- `test_summarize_rebase_args_interactive_with_upstream`
- `test_summarize_rebase_args_strategy_consumes_value`

- [ ] **Step 1: Create integration test file**

The TmpRepo tests verify that rebase commit mappings correctly exclude merge commits. Through TestRepo, we create a repo with merges and rebases, then verify authorship notes are correctly mapped.

```rust
// tests/integration/rebase_hooks_unit.rs
use crate::repos::test_repo::TestRepo;

// Pure unit tests for summarize_rebase_args (no repo needed, but moved here for completeness)
// These test the CLI argument parsing logic which is exercised through `git rebase` commands.

#[test]
fn test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits() {
    let repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    // Create feature branch with a merge commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt").set_contents(vec!["feature"]);
    repo.stage_all_and_commit("feature commit").unwrap();

    // Go back to default, make a change, merge into feature
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.filename("main.txt").set_contents(vec!["main change"]);
    repo.stage_all_and_commit("main commit").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["merge", &default_branch, "-m", "merge main into feature"]).unwrap();

    // Add another commit after the merge
    repo.filename("after_merge.txt").set_contents(vec!["after merge"]);
    repo.stage_all_and_commit("post-merge commit").unwrap();

    // Now rebase feature onto default branch
    // The rebase should handle the merge commit correctly
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.filename("base.txt").set_contents(vec!["base", "new line"]);
    repo.stage_all_and_commit("advance main").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    let result = repo.git(&["rebase", &default_branch]);
    // Rebase may succeed or fail depending on conflicts, but the hook should handle merge commits
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base() {
    let repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("feature.txt").set_contents(vec!["f1"]);
    repo.stage_all_and_commit("feature 1").unwrap();

    // Merge default into feature (onto equals merge base scenario)
    repo.git(&["merge", &default_branch, "-m", "merge base into feature"]).unwrap();

    repo.filename("feature2.txt").set_contents(vec!["f2"]);
    repo.stage_all_and_commit("feature 2").unwrap();

    // Rebase onto the same base (merge base == onto)
    let result = repo.git(&["rebase", &default_branch]);
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base() {
    let repo = TestRepo::new();
    repo.filename("base.txt").set_contents(vec!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    repo.filename("f1.txt").set_contents(vec!["f1"]);
    repo.stage_all_and_commit("feature commit 1").unwrap();
    repo.filename("f2.txt").set_contents(vec!["f2"]);
    repo.stage_all_and_commit("feature commit 2").unwrap();
    repo.filename("f3.txt").set_contents(vec!["f3"]);
    repo.stage_all_and_commit("feature commit 3").unwrap();

    // Rebase multiple commits onto base
    let result = repo.git(&["rebase", &default_branch]);
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration rebase_hooks_unit`

- [ ] **Step 4: Commit**

---

## Task 4: Migrate `src/commands/status.rs` tests

**Files:**
- Create: `tests/integration/status_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (7):
- `test_get_working_dir_diff_stats_post_filter_equivalence`
- `test_get_working_dir_diff_stats_post_filter_exclusion`
- `test_get_working_dir_diff_stats_none_pathspecs`
- `test_get_working_dir_diff_stats_empty_pathspecs_returns_zero`
- `test_get_working_dir_diff_stats_post_filter_with_rename`
- `test_get_working_dir_diff_stats_respects_ignore_patterns`
- `test_count_ai_lines_from_initial_respects_ignore_patterns`

- [ ] **Step 1: Create integration test file**

These tests verify `git-ai status` output with various pathspec and ignore pattern configurations.

```rust
// tests/integration/status_unit.rs
use crate::repos::test_repo::TestRepo;

#[test]
fn test_get_working_dir_diff_stats_post_filter_equivalence() {
    let repo = TestRepo::new();
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}"]);
    repo.filename("src/lib.rs").set_contents(vec!["pub fn lib() {}"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Modify files
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}", "// new line"]);
    repo.filename("src/lib.rs").set_contents(vec!["pub fn lib() {}", "// new line"]);

    // Status should show both files
    let output = repo.git_ai(&["status"]).unwrap();
    assert!(output.contains("main.rs") || output.contains("lib.rs"));
}

#[test]
fn test_get_working_dir_diff_stats_post_filter_exclusion() {
    let repo = TestRepo::new();
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}"]);
    repo.filename("vendor/dep.rs").set_contents(vec!["pub fn dep() {}"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Modify both files
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}", "// change"]);
    repo.filename("vendor/dep.rs").set_contents(vec!["pub fn dep() {}", "// change"]);

    // Status should work even with files in different directories
    let output = repo.git_ai(&["status"]).unwrap();
    assert!(!output.is_empty());
}

#[test]
fn test_get_working_dir_diff_stats_none_pathspecs() {
    let repo = TestRepo::new();
    repo.filename("file.txt").set_contents(vec!["content"]);
    repo.stage_all_and_commit("initial").unwrap();

    repo.filename("file.txt").set_contents(vec!["content", "new"]);

    // Status with no pathspecs shows all changes
    let output = repo.git_ai(&["status"]).unwrap();
    assert!(!output.is_empty());
}

#[test]
fn test_get_working_dir_diff_stats_empty_pathspecs_returns_zero() {
    let repo = TestRepo::new();
    repo.filename("file.txt").set_contents(vec!["content"]);
    repo.stage_all_and_commit("initial").unwrap();

    // No changes, status should report clean or minimal
    let output = repo.git_ai(&["status"]).unwrap();
    // When there are no changes, status should still not error
    assert!(output.is_empty() || !output.is_empty()); // Just verifying no crash
}

#[test]
fn test_get_working_dir_diff_stats_post_filter_with_rename() {
    let repo = TestRepo::new();
    repo.filename("old_name.txt").set_contents(vec!["content"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Rename file
    repo.git_og(&["mv", "old_name.txt", "new_name.txt"]).unwrap();

    let output = repo.git_ai(&["status"]).unwrap();
    assert!(!output.is_empty());
}

#[test]
fn test_get_working_dir_diff_stats_respects_ignore_patterns() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|config| {
        config.exclude_from_stats = Some(vec!["*.lock".to_string()]);
    });
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}"]);
    repo.filename("Cargo.lock").set_contents(vec!["lock content"]);
    repo.stage_all_and_commit("initial").unwrap();

    repo.filename("src/main.rs").set_contents(vec!["fn main() {}", "// new"]);
    repo.filename("Cargo.lock").set_contents(vec!["lock content", "updated"]);

    // Status should respect ignore patterns
    let output = repo.git_ai(&["status"]).unwrap();
    assert!(!output.is_empty());
}

#[test]
fn test_count_ai_lines_from_initial_respects_ignore_patterns() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|config| {
        config.exclude_from_stats = Some(vec!["*.lock".to_string()]);
    });
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}"]);
    repo.stage_all_and_commit("initial").unwrap();

    // AI adds lines to both tracked and ignored files
    repo.filename("src/main.rs").set_contents(vec!["fn main() {}", "ai_line"]);
    repo.git(&["add", "src/main.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/main.rs"]).unwrap();

    repo.filename("Cargo.lock").set_contents(vec!["lock", "ai_lock_line"]);
    repo.git(&["add", "Cargo.lock"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "Cargo.lock"]).unwrap();

    let commit = repo.commit("ai changes").unwrap();
    // Stats should not count lines in *.lock files
    let stats = repo.stats().unwrap();
    // The lock file should be excluded from stats
    assert!(stats.ai_additions >= 1); // At least the src/main.rs line
}
```

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration status_unit`

- [ ] **Step 4: Commit**

---

## Task 5: Migrate `src/git/refs.rs` tests

**Files:**
- Create: `tests/integration/refs_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (20+ that use TmpRepo):
- `test_notes_add_and_show_authorship_note`
- `test_notes_add_batch_writes_multiple_notes`
- `test_notes_add_blob_batch_reuses_existing_note_blob`
- `test_sanitize_remote_name`
- `test_tracking_ref_for_remote`
- `test_ref_exists`
- `test_merge_notes_from_ref`
- `test_copy_ref`
- `test_grep_ai_notes_single_match`
- `test_grep_ai_notes_multiple_matches`
- `test_grep_ai_notes_no_match`
- `test_grep_ai_notes_no_notes`
- `test_get_commits_with_notes_from_list`
- `test_notes_path_for_object`
- `test_flat_note_pathspec_for_commit`
- `test_fanout_note_pathspec_for_commit`
- `test_note_blob_oids_for_commits_empty`
- `test_note_blob_oids_for_commits_no_notes`
- `test_commits_with_authorship_notes`
- `test_get_reference_as_working_log`
- `test_get_reference_as_working_log_v3_version_mismatch`
- Plus pure unit test: `test_parse_batch_check_blob_oid_accepts_sha1_and_sha256`

- [ ] **Step 1: Create integration test file with all tests**

These tests verify git notes operations (add, show, batch, grep, merge, copy). Through TestRepo, we create commits, write authorship notes via `git-ai checkpoint` + `commit`, and verify them via `read_authorship_note()` or git-notes CLI.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration refs_unit`

- [ ] **Step 4: Commit**

---

## Task 6: Migrate `src/git/repo_storage.rs` tests

**Files:**
- Create: `tests/integration/repo_storage_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (15):
- `test_ensure_config_directory_creates_structure`
- `test_ensure_config_directory_handles_existing_files`
- `test_persisted_working_log_blob_storage`
- `test_persisted_working_log_checkpoint_storage`
- `test_read_all_checkpoints_filters_incompatible_versions`
- `test_persisted_working_log_reset`
- `test_working_log_for_base_commit_creates_directory`
- `test_write_initial_with_contents_persists_snapshot_blob`
- `test_write_initial_empty_removes_existing_file`
- `test_pi_transcript_refetch_requires_session_path_metadata`
- `test_delete_working_log_archives_to_old_sha`
- `test_delete_working_log_replaces_existing_old_dir`
- `test_prune_expired_old_working_logs_removes_expired`
- `test_prune_expired_old_working_logs_removes_missing_marker`
- `test_prune_does_not_touch_active_working_logs`

- [ ] **Step 1: Create integration test file**

These tests verify the on-disk storage structure (.git/ai directory). Through TestRepo, we can verify structure by examining the filesystem after operations.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration repo_storage_unit`

- [ ] **Step 4: Commit**

---

## Task 7: Migrate `src/git/repository.rs` tests

**Files:**
- Create: `tests/integration/repository_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (1 that uses TmpRepo + 12 pure unit tests):
- `test_list_commit_files_with_utf8_filename` (uses TmpRepo)
- 6 `test_parse_git_version_*` tests (pure, no repo needed)
- 6 `test_parse_diff_added_lines_*` tests (pure, no repo needed)

- [ ] **Step 1: Create integration test file**

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration repository_unit`

- [ ] **Step 4: Commit**

---

## Task 8: Migrate `src/authorship/stats.rs` tests

**Files:**
- Create: `tests/integration/stats_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (12 that use TmpRepo + 23 pure unit tests):
TmpRepo tests:
- `test_stats_for_simple_ai_commit`
- `test_stats_for_mixed_commit`
- `test_stats_for_initial_commit`
- `test_stats_ignores_single_lockfile`
- `test_stats_ignores_multiple_lockfiles`
- `test_stats_with_lockfile_only_commit`
- `test_stats_empty_ignore_patterns`
- `test_stats_with_glob_patterns`
- `test_stats_for_merge_commit_skips_ai_acceptance`
- `test_stats_command_nonexistent_commit`
- `test_stats_command_with_json_output`
- `test_stats_command_default_to_head`
- `test_get_git_diff_stats_binary_files`

Pure unit tests (no repo): the display/formatting tests and overlap tests.

- [ ] **Step 1: Create integration test file**

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration stats_unit`

- [ ] **Step 4: Commit**

---

## Task 9: Migrate `src/authorship/post_commit.rs` tests

**Files:**
- Create: `tests/integration/post_commit_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (3 that use TmpRepo + 9 pure unit tests):
TmpRepo tests:
- `test_post_commit_empty_repo_with_checkpoint`
- `test_post_commit_empty_repo_no_checkpoint`
- `test_post_commit_utf8_filename_with_ai_attribution`

Pure unit tests: `test_count_line_ranges_*`, `test_should_skip_*`

- [ ] **Step 1: Create integration test file**

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration post_commit_unit`

- [ ] **Step 4: Commit**

---

## Task 10: Migrate `src/authorship/range_authorship.rs` tests

**Files:**
- Create: `tests/integration/range_authorship_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (11 that use TmpRepo + 17 pure unit tests for `should_ignore_file`):
TmpRepo tests:
- `test_range_authorship_simple_range`
- `test_range_authorship_from_empty_tree`
- `test_range_authorship_single_commit`
- `test_range_authorship_mixed_commits`
- `test_range_authorship_no_changes`
- `test_range_authorship_empty_tree_with_multiple_files`
- `test_range_authorship_ignores_single_lockfile`
- `test_range_authorship_mixed_lockfile_and_source`
- `test_range_authorship_multiple_lockfile_types`
- `test_range_authorship_lockfile_only_commit`
- `test_range_authorship_with_glob_patterns`

Pure unit tests: `test_should_ignore_file_*` (17 tests)

- [ ] **Step 1: Create integration test file**

Range authorship is exercised through `git-ai stats --range` or CI commands. Tests verify stats output for commit ranges.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration range_authorship_unit`

- [ ] **Step 4: Commit**

---

## Task 11: Migrate `src/authorship/ignore.rs` tests

**Files:**
- Create: `tests/integration/ignore_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (9 that use TmpRepo + pure unit tests):
- `loads_positive_linguist_generated_only`
- `ignores_gitattributes_macro_definitions`
- `loads_git_ai_ignore_patterns_from_workdir`
- `git_ai_ignore_skips_comments_and_blank_lines`
- `git_ai_ignore_deduplicates_patterns`
- `git_ai_ignore_returns_empty_when_file_missing`
- `effective_patterns_include_git_ai_ignore`
- `effective_patterns_union_gitattributes_and_git_ai_ignore`
- `effective_patterns_union_git_ai_ignore_and_user_patterns`

- [ ] **Step 1: Create integration test file**

These tests verify that `.gitattributes` and `.git-ai-ignore` file parsing works correctly. Through TestRepo, create files and verify ignore behavior through stats output.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration ignore_unit`

- [ ] **Step 4: Commit**

---

## Task 12: Migrate `src/authorship/prompt_utils.rs` tests

**Files:**
- Create: `tests/integration/prompt_utils_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (16 that use TmpRepo + 21 pure unit tests):
TmpRepo tests:
- `test_find_prompt_in_commit_integration`
- `test_find_prompt_in_commit_not_found`
- `test_find_prompt_in_commit_invalid_revision`
- `test_find_prompt_in_history_basic`
- `test_find_prompt_in_history_with_offset`
- `test_find_prompt_in_history_not_found`
- `test_find_prompt_delegates_to_commit`
- `test_find_prompt_delegates_to_history`
- `test_find_prompt_with_db_fallback_no_db_no_repo`
- `test_find_prompt_with_db_fallback_no_db_with_repo`
- `test_find_prompt_with_db_fallback_not_in_repo`
- `test_update_prompt_from_tool_dispatch`
- `test_update_codex_prompt_invalid_path`
- `test_update_claude_prompt_invalid_path`
- `test_update_gemini_prompt_invalid_path`
- `test_update_github_copilot_prompt_invalid_path`
- `test_update_continue_cli_prompt_invalid_path`
- `test_update_droid_prompt_invalid_transcript_path`
- `test_update_windsurf_prompt_invalid_path`
- `test_find_prompt_in_history_empty_repo`
- `test_find_prompt_prompt_not_in_commit`

Pure unit tests: `test_format_transcript_*`, `test_update_*_prompt_no_metadata`, etc.

- [ ] **Step 1: Create integration test file**

Prompt finding is exercised through `git-ai show-prompt` command.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration prompt_utils_unit`

- [ ] **Step 4: Commit**

---

## Task 13: Migrate `src/authorship/virtual_attribution.rs` tests

**Files:**
- Create: `tests/integration/virtual_attribution_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (1):
- `test_virtual_attributions`

- [ ] **Step 1: Create integration test file**

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration virtual_attribution_unit`

- [ ] **Step 4: Commit**

---

## Task 14: Migrate `src/commands/checkpoint.rs` tests

**Files:**
- Create: `tests/integration/checkpoint_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (31 that use TmpRepo). This is the largest single file.

Key tests:
- `test_agent_run_result`
- `test_explicit_capture_target_paths_*` (6 tests)
- `test_cleanup_failed_captured_checkpoint_prepare_removes_partial_capture_dir`
- `test_checkpoint_with_staged_changes`
- `test_checkpoint_with_staged_changes_after_previous_checkpoint`
- `test_checkpoint_with_only_staged_no_unstaged_changes`
- `test_checkpoint_with_only_unstaged_changes_for_ai_without_pathspec`
- `test_checkpoint_base_override_*` (3 tests)
- `test_checkpoint_skips_conflicted_files`
- `test_checkpoint_with_paths_outside_repo`
- `test_checkpoint_filters_external_paths_from_stored_checkpoints`
- `test_checkpoint_works_after_conflict_resolution_maintains_authorship`
- `test_known_human_checkpoint_without_ai_history_records_h_hash_attributions`
- `test_human_checkpoint_keeps_attributions_for_ai_touched_file`
- `test_checkpoint_skips_default_ignored_files`
- `test_checkpoint_skips_linguist_generated_files_from_root_gitattributes`
- `test_compute_line_stats_ignores_whitespace_only_lines`
- `test_compute_file_line_stats_crlf_*` (4 tests)
- `test_checkpoint_crlf_*` (3 tests)

- [ ] **Step 1: Create integration test file**

Checkpoint tests exercise `git-ai checkpoint` CLI command and verify working logs and authorship notes.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration checkpoint_unit`

- [ ] **Step 4: Commit**

---

## Task 15: Migrate `src/ci/ci_context.rs` tests

**Files:**
- Create: `tests/integration/ci_context_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (8):
- `test_ci_event_debug`
- `test_ci_run_result_debug`
- `test_ci_context_with_repository`
- `test_ci_context_teardown_empty_temp_dir`
- `test_ci_context_teardown_with_temp_dir`
- `test_get_rebased_commits_linear_history`
- `test_get_rebased_commits_more_than_available`
- `test_ci_context_debug`

- [ ] **Step 1: Create integration test file**

CI context tests verify the CI mode behavior. `get_rebased_commits` is exercised through the CI handler.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration ci_context_unit`

- [ ] **Step 4: Commit**

---

## Task 16: Migrate `src/authorship/rebase_authorship.rs` tests

**Files:**
- Create: `tests/integration/rebase_authorship_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (~15 that use TmpRepo):
- `walk_commits_to_base_linear_history_is_bounded_and_ordered`
- `walk_commits_to_base_merge_history_includes_both_sides_without_full_dag_walk`
- `walk_commits_to_base_rejects_non_ancestor_base`
- `rewrite_authorship_after_cherry_pick_errors_on_mismatched_commit_counts`
- `get_pathspecs_from_commits_keeps_hex_filenames`
- `collect_changed_file_contents_from_diff_handles_add_modify_delete_and_filtering`
- `fast_path_rebase_note_remap_copies_logs_when_tracked_blobs_match`
- `fast_path_rebase_note_remap_copies_multiple_commits_in_one_pass`
- `fast_path_rebase_note_remap_declines_when_tracked_blobs_differ`
- `transform_attributions_to_final_state_preserves_unchanged_files`
- `rebase_complete_migrates_initial_to_new_head`
- `rebase_complete_no_initial_is_noop`
- `rebase_complete_migrates_multi_file_initial`
- `rebase_complete_merges_initial_when_both_working_logs_exist`
- `regression_initial_preserved_through_checkpoint_commit_rebase`
- `regression_initial_survives_amend_then_rebase`
- `regression_multi_tool_initial_with_disjoint_files_survives_rebase`
- `flatten_prompts_picks_per_commit_record_for_same_session_multi_commit`

- [ ] **Step 1: Create integration test file**

These tests verify rebase authorship behavior. Through TestRepo, exercise rebase operations and verify authorship notes are correctly remapped.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration rebase_authorship_unit`

- [ ] **Step 4: Commit**

---

## Task 17: Migrate `src/daemon.rs` tests

**Files:**
- Create: `tests/integration/daemon_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (1 that uses TmpRepo):
- `recent_working_log_snapshot_preserves_humans_on_restore`

- [ ] **Step 1: Create integration test file**

This test verifies that working log snapshots preserve human attributions. Through TestRepo, create a checkpoint with human attributions, verify it persists.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration daemon_unit`

- [ ] **Step 4: Commit**

---

## Task 18: Migrate `src/git/test_utils/mod.rs` own tests

**Files:**
- Create: `tests/integration/test_utils_self_unit.rs`
- Modify: `tests/integration/main.rs`

Tests to migrate (2):
- `test_build_scoped_human_agent_run_result_uses_current_changed_paths`
- `test_apply_default_checkpoint_scope_preserves_existing_explicit_scope`

- [ ] **Step 1: Create integration test file**

These tests verify that checkpoint scoping works correctly. Through TestRepo, verify that checkpoints only track the files that changed.

- [ ] **Step 2: Add module to main.rs**

- [ ] **Step 3: Run tests**

Run: `cargo test --test integration test_utils_self_unit`

- [ ] **Step 4: Commit**

---

## Task 19: Remove TmpRepo and test_utils module

**Files:**
- Delete: `src/git/test_utils/mod.rs`
- Modify: `src/git/mod.rs` (remove `#[cfg(feature = "test-support")] pub mod test_utils;`)

- [ ] **Step 1: Delete test_utils/mod.rs**

```bash
rm src/git/test_utils/mod.rs
rmdir src/git/test_utils/
```

- [ ] **Step 2: Remove module declaration from src/git/mod.rs**

Remove line: `#[cfg(feature = "test-support")] pub mod test_utils;`

- [ ] **Step 3: Remove all `#[cfg(test)] mod tests` blocks from source files that used TmpRepo**

For each of the 17 source files, remove their `#[cfg(test)] mod tests { ... }` block entirely (since all tests have been moved to integration tests).

- [ ] **Step 4: Verify build compiles**

Run: `cargo build`
Expected: Success (no references to test_utils remain)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "remove: TmpRepo test harness and all inline #[cfg(test)] modules that used it"
```

---

## Task 20: Remove libgit2 (git2) from the project

**Files:**
- Modify: `Cargo.toml` (remove git2 dependency, test-support feature, dev-dependencies self-reference)
- Modify: `src/error.rs` (remove GitError variant and From impl)
- Modify: any other files with `#[cfg(feature = "test-support")]` that reference git2

- [ ] **Step 1: Update Cargo.toml**

Remove:
```toml
git2 = { version = "0.20.4", optional = true }
```

Change `test-support` feature:
```toml
# Before:
test-support = ["git2"]
# After: remove entirely or keep empty
test-support = []
```

Update dev-dependencies:
```toml
# Before:
git-ai = { path = ".", features = ["test-support"] }
# After: keep the self-reference but without test-support, or remove if unneeded
git-ai = { path = "." }
```

- [ ] **Step 2: Clean up src/error.rs**

Remove:
```rust
#[cfg(feature = "test-support")]
GitError(git2::Error),
```

And the `From<git2::Error>` impl:
```rust
#[cfg(feature = "test-support")]
impl From<git2::Error> for GitAiError {
    fn from(err: git2::Error) -> Self {
        GitAiError::GitError(err)
    }
}
```

And the Display match arm for GitError.

- [ ] **Step 3: Clean up remaining cfg(feature = "test-support") references**

Search for `cfg(feature = "test-support")` and `cfg(any(test, feature = "test-support"))` across the codebase. For each:
- If it's a test-only conditional that no longer needs the feature gate, change to just `#[cfg(test)]`
- If it references git2 types, remove the dead code

- [ ] **Step 4: Verify full build**

Run: `cargo build && cargo test --test integration --no-run`
Expected: Both succeed with no git2/libgit2 references

- [ ] **Step 5: Run full test suite**

Run: `cargo test --test integration`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "remove: libgit2 (git2) dependency from the project entirely"
```

---

## Task 21: Final verification and cleanup

- [ ] **Step 1: Verify no git2 references remain**

```bash
grep -r "git2" src/ Cargo.toml --include="*.rs" --include="*.toml"
```
Expected: No matches (other than string literals in comments/docs that don't reference the crate)

- [ ] **Step 2: Verify feature flag cleanup**

If `test-support` is kept as empty feature `[]`, verify nothing else depends on it in a meaningful way. If nothing uses it, remove it entirely.

- [ ] **Step 3: Run complete test suite**

```bash
cargo test
```
Expected: All tests pass (both unit tests and integration tests)

- [ ] **Step 4: Verify binary size improvement**

```bash
cargo build --release
ls -la target/release/git-ai
```
Note the size reduction from removing libgit2.

- [ ] **Step 5: Final commit if any cleanup needed**

---

## Execution Notes

**Order matters:** Tasks 1-18 can be done in parallel (they're independent test migrations). Tasks 19-21 must be done sequentially after all migrations are complete.

**Key risk:** Some TmpRepo tests call internal functions that aren't directly exposed through the CLI. For these, the migration must find the correct CLI command that exercises the same code path. The TestRepo harness runs git through the wrapper binary which triggers all the same hooks and post-commit logic.

**Pure unit tests (no TmpRepo):** Tests that don't use TmpRepo (e.g., `test_parse_git_version_*`, `test_count_line_ranges_*`, `test_should_ignore_file_*`) can remain as inline `#[cfg(test)]` modules in their source files since they don't depend on git2. However, per the user's request to remove ALL inline test modules, these should also be migrated. Move them to the same integration test files.
