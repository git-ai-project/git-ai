# Missing AI Attribution Investigation Tests

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Write integration tests that expose scenarios where AI-authored commits produce notes with 100% human attribution, verifying each of five identified failure hypotheses.

**Architecture:** Each test creates a `TestRepo`, simulates a specific failure scenario by directly writing checkpoint data (to control timing/content precisely), commits, and asserts that the `AuthorshipLog` contains AI attestation entries. Tests that currently fail demonstrate the bug; tests that pass eliminate hypotheses.

**Tech Stack:** Rust integration tests using existing `TestRepo`/`TmpRepo` infrastructure, `checkpoints.jsonl` direct-write pattern (as in `test_ai_generated_file_then_human_full_rewrite`), `AuthorshipLog` assertion APIs.

---

## File Structure

- **Create:** `tests/integration/missing_attribution.rs` — All five hypothesis tests
- **Modify:** `tests/integration/main.rs` — Add `mod missing_attribution;` declaration

The existing test pattern in `simple_additions.rs:1670` (`test_ai_generated_file_then_human_full_rewrite`) is our template: directly write `checkpoints.jsonl` with controlled checkpoint data, then commit and inspect the resulting `AuthorshipLog`.

---

## Chunk 1: Scaffolding and H1 (Empty Working Log)

### Task 1: Register the new test module

**Files:**
- Modify: `tests/integration/main.rs`

- [ ] **Step 1: Add module declaration**

Add `mod missing_attribution;` to `tests/integration/main.rs` alongside the other module declarations.

- [ ] **Step 2: Verify it compiles**

Run: `cargo test --test integration missing_attribution --no-run 2>&1 | tail -5`
Expected: Compiles (may warn about empty module)

- [ ] **Step 3: Commit**

```bash
git add tests/integration/main.rs tests/integration/missing_attribution.rs
git commit -m "test: scaffold missing_attribution integration test module"
```

### Task 2: H1 — No AI checkpoints written to working log

**Hypothesis:** If the AI agent (e.g., Claude Code) writes code but never calls `git-ai checkpoint`, the working log contains only human checkpoints. The pre-commit hook runs `checkpoint::run` with `kind=Human`, which writes a Human checkpoint. `from_just_working_log` processes only Human entries → no AI `line_attributions` → `to_authorship_log_and_initial_working_log` produces an `AuthorshipLog` with empty `files` and `prompts`.

**How this could happen in practice:** Claude Code starts generating code, the user commits before Claude finishes (or Claude's checkpoint call fails silently), so only the pre-commit Human checkpoint exists.

**Files:**
- Create: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// H1: AI writes code but no AI checkpoint is recorded — only the human
/// pre-commit checkpoint exists. The commit should still succeed but the
/// authorship log will have no AI attestation entries.
///
/// This test documents the expected behavior: if no AI checkpoint is written,
/// attribution is correctly 100% human. This is NOT a bug — it's the baseline.
/// The bug is when AI checkpoints ARE written but attribution is still 100% human.
#[test]
fn test_h1_no_ai_checkpoint_produces_human_only_attribution() {
    let repo = TestRepo::new();

    // Create a base commit so we have a parent SHA
    let mut file = repo.filename("base.txt");
    file.set_contents(crate::lines!["base line"]);
    repo.stage_all_and_commit("base commit").unwrap();

    // Now simulate: AI writes code but only a Human checkpoint is created
    // (no AI checkpoint was ever called)
    let ai_content = "fn ai_generated() {\n    println!(\"hello from AI\");\n}\n";
    let file_path = repo.path().join("ai_code.rs");
    fs::write(&file_path, ai_content).unwrap();

    // Only create a human checkpoint (simulating pre-commit hook)
    repo.git_ai(&["checkpoint", "--", "ai_code.rs"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    let commit = repo.stage_all_and_commit("add ai code").unwrap();

    // With no AI checkpoint, authorship log should have no AI attestation
    let has_ai_files = !commit.authorship_log.files.is_empty();
    let has_ai_prompts = !commit.authorship_log.metadata.prompts.is_empty();

    assert!(
        !has_ai_files && !has_ai_prompts,
        "H1 baseline: Without AI checkpoints, authorship should be 100% human.\n\
         files: {:?}\nprompts: {:?}",
        commit.authorship_log.files,
        commit.authorship_log.metadata.prompts
    );
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --test integration test_h1_no_ai_checkpoint -- --nocapture 2>&1 | tail -20`
Expected: PASS — this is the expected baseline behavior

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H1 baseline — no AI checkpoint produces human-only attribution"
```

### Task 3: H2 — Base commit SHA mismatch between checkpoint and post-commit

**Hypothesis:** The working log is keyed by `resolve_base_commit()` which uses HEAD. If HEAD changes between when AI checkpoints are written and when `post_commit` runs (e.g., due to `git commit --amend`, interactive rebase, or a race with another commit), `post_commit` looks up the wrong working log directory and finds no AI checkpoints.

**How this could happen in practice:**
1. AI writes code on commit ABC, checkpoints stored under `.git/ai/working_logs/ABC/`
2. User runs `git commit --amend` or `git rebase` which changes HEAD to DEF
3. Next commit's post-commit looks for working log under DEF, finds nothing or only human entries
4. Result: 100% human attribution despite AI having written the code

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H2: Base commit SHA mismatch — AI checkpoints are keyed under parent SHA X,
/// but by the time post-commit runs, HEAD has moved to SHA Y (e.g., after amend).
/// Post-commit looks up working_logs/Y/ which has no AI data.
///
/// This reproduces the exact scenario: AI checkpoint written under commit A,
/// then an amend changes HEAD, then a new commit's post-commit can't find the AI data.
#[test]
fn test_h2_base_commit_sha_mismatch_after_amend() {
    let repo = TestRepo::new();

    // Step 1: Create initial commit
    let mut file = repo.filename("code.rs");
    file.set_contents(crate::lines!["fn main() {}", "    // base"]);
    let base = repo.stage_all_and_commit("initial").unwrap();

    // Step 2: AI writes new code — checkpoint is keyed to base.commit_sha
    file.set_contents(crate::lines![
        "fn main() {}",
        "    // base",
        "    println!(\"AI wrote this\");".ai(),
        "    println!(\"AI also wrote this\");".ai(),
    ]);

    // Step 3: Amend the previous commit (changes HEAD SHA)
    // This is the critical step: HEAD moves from base.commit_sha to a NEW sha
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Step 4: Now the working log was keyed to the OLD base.commit_sha,
    // but HEAD (and thus the next commit's parent) is the NEW amended SHA.
    // Write more AI code and commit — post-commit will look up the wrong working log
    let new_content = "fn main() {}\n    // base\n    println!(\"AI wrote this\");\n    println!(\"AI also wrote this\");\n    println!(\"More AI code\");\n";
    let file_path = repo.path().join("code.rs");
    fs::write(&file_path, new_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "code.rs"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    let commit = repo.stage_all_and_commit("add more ai code").unwrap();

    // The AI checkpoint from step 2 was lost because it was keyed to the old SHA.
    // The checkpoint from step 4 should still be found (it was written after amend).
    // If this test fails with ZERO AI attestation, it means the step 4 checkpoint
    // was also lost — indicating a SHA mismatch bug.
    let has_ai_attestation = !commit.authorship_log.files.is_empty();

    assert!(
        has_ai_attestation,
        "H2: AI checkpoint written after amend should still produce AI attestation.\n\
         The working log for the new HEAD should contain the AI checkpoint.\n\
         authorship_log: {:?}",
        commit.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h2_base_commit_sha_mismatch -- --nocapture 2>&1 | tail -20`
Expected: If this FAILS, we've found a manifestation of hypothesis H2.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H2 — base commit SHA mismatch after amend loses AI attribution"
```

### Task 4: H2b — Direct working log manipulation to prove SHA mismatch

**Hypothesis:** Same as H2 but with more direct control — write AI checkpoints under one SHA, then ensure post-commit reads from a different SHA.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H2b: Directly demonstrate the SHA mismatch by writing AI checkpoint data
/// under a specific base commit SHA, then committing in a state where
/// post-commit resolves a DIFFERENT base commit.
///
/// This is the most direct test of the hypothesis: if the working log directory
/// is keyed by the wrong SHA, AI attribution is silently lost.
#[test]
fn test_h2b_direct_working_log_sha_mismatch() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    // Create base commit
    let mut file = repo.filename("app.py");
    file.set_contents(crate::lines!["print('hello')"]);
    let _base = repo.stage_all_and_commit("base").unwrap();

    // Get current HEAD (this is the correct base commit SHA)
    let correct_base = repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Write AI content to disk
    let ai_content = "print('hello')\nprint('AI generated line 1')\nprint('AI generated line 2')\n";
    let file_path = repo.path().join("app.py");
    fs::write(&file_path, ai_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // Write AI checkpoint under a WRONG base commit SHA (simulating the mismatch)
    let wrong_base = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let git_dir = repo
        .git(&["rev-parse", "--git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let wrong_dir = std::path::Path::new(&git_dir)
        .join("ai/working_logs")
        .join(wrong_base);
    fs::create_dir_all(&wrong_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let agent_author_id = "3bd30911a58cb074"; // SHA256("mock_ai:test_session")[..16]
    let checkpoint_data = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"app.py","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":3,"author_id":"{agent_author_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":2,"deletions":0,"additions_sloc":2,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(wrong_dir.join("checkpoints.jsonl"), &checkpoint_data).unwrap();

    // Now also ensure the CORRECT base SHA directory has NO AI checkpoints
    // (only what pre-commit would write)
    // The human checkpoint from `stage_all_and_commit` will be written under correct_base

    let commit = repo.stage_all_and_commit("add ai lines").unwrap();

    // The AI checkpoint is under wrong_base, but post-commit looks under correct_base.
    // This should produce ZERO AI attestation — proving H2.
    let has_ai_attestation = !commit.authorship_log.files.is_empty();

    // This assertion documents the expected FAILURE mode.
    // If has_ai_attestation is false, we've proven H2 causes lost attribution.
    if !has_ai_attestation {
        eprintln!(
            "H2b CONFIRMED: AI checkpoint under wrong SHA ({}) was not found.\n\
             Post-commit looked under correct SHA ({}).\n\
             AI attribution was silently lost.",
            wrong_base, correct_base
        );
    }

    // The real question: does the CORRECT working log also lack AI data?
    // If so, this proves the mismatch scenario causes 100% human attribution.
    assert!(
        !has_ai_attestation,
        "H2b: Expected NO AI attestation when checkpoint is under wrong SHA.\n\
         If this assertion fails, it means the system somehow found the misplaced data.\n\
         authorship_log files: {:?}",
        commit.authorship_log.files
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h2b_direct_working_log -- --nocapture 2>&1 | tail -20`
Expected: PASS — proves that SHA mismatch causes silent data loss

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H2b — direct proof that SHA mismatch loses AI attribution"
```

---

## Chunk 2: Committed Hunk Detection and Pre-Commit Skip Logic

### Task 5: H5 — Committed hunk detection discards AI lines

**Hypothesis:** In `to_authorship_log_and_initial_working_log` (virtual_attribution.rs:1292-1307), lines that are "neither unstaged nor in committed_hunks" are silently discarded. If the diff between parent and commit doesn't detect certain lines as "committed" (e.g., due to line-ending normalization, whitespace settings, or pathspec filtering), AI-attributed lines fall through the cracks and get discarded.

**How this could happen in practice:**
1. AI writes a file with specific content
2. Content gets normalized (line endings, trailing whitespace) between checkpoint and commit
3. `collect_committed_hunks` uses `diff_added_lines(parent, commit)` which may not match normalized lines
4. Lines that ARE in the commit but DON'T appear in the diff hunks → silently discarded

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test for line-ending mismatch**

```rust
/// H5: Committed hunk detection failure due to line-ending normalization.
///
/// When core.autocrlf or .gitattributes normalize line endings between
/// the working copy (where checkpoints were recorded) and the committed tree,
/// the diff between parent and commit may not contain the expected hunks.
/// This causes AI-attributed lines to be classified as "already existed in parent"
/// and silently discarded.
#[test]
fn test_h5_line_ending_normalization_drops_ai_attribution() {
    let repo = TestRepo::new();

    // Enable autocrlf to force line-ending normalization
    repo.git_og(&["config", "core.autocrlf", "true"]).unwrap();

    // Create base commit
    let mut file = repo.filename("normalized.txt");
    file.set_contents(crate::lines!["line 1"]);
    repo.stage_all_and_commit("base").unwrap();

    // AI writes content with explicit CRLF line endings
    // The checkpoint records these lines, but git may normalize them on commit
    let ai_content = "line 1\r\nAI line 2\r\nAI line 3\r\n";
    let file_path = repo.path().join("normalized.txt");
    fs::write(&file_path, ai_content).unwrap();

    // Create AI checkpoint with CRLF content
    repo.git_ai(&["checkpoint", "mock_ai", "normalized.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();

    let commit = repo.stage_all_and_commit("ai additions").unwrap();

    let has_ai_attestation = !commit.authorship_log.files.is_empty();

    assert!(
        has_ai_attestation,
        "H5: Line-ending normalization should NOT cause AI attribution loss.\n\
         AI wrote lines 2-3 but authorship log has no AI attestation.\n\
         This indicates committed hunk detection failed after normalization.\n\
         authorship_log: {:?}",
        commit.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h5_line_ending -- --nocapture 2>&1 | tail -20`
Expected: If this FAILS, we've found H5 manifesting via line-ending normalization.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H5 — line-ending normalization may drop AI attribution"
```

### Task 6: H5b — Pathspec filtering removes AI files from committed hunks

**Hypothesis:** The `pathspecs` parameter to `to_authorship_log_and_initial_working_log` filters which files are checked for committed hunks. If pathspec construction misses a file that has AI checkpoints, that file's committed hunks return empty → AI lines classified as "already existed" → discarded.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H5b: Pathspec filtering excludes AI-checkpointed file from committed hunks.
///
/// In post_commit.rs, pathspecs are built from checkpoint entries that pass
/// checkpoint_entry_requires_post_processing(). If a checkpoint entry has
/// kind=Human AND only human line_attributions (no overrode), it's filtered out.
/// But if the AI checkpoint for that file was lost (H2) or skipped, and only
/// the human pre-commit checkpoint remains, the file won't be in pathspecs.
///
/// This test directly writes checkpoints where the AI file is present in the
/// working log but the pathspec builder would exclude it.
#[test]
fn test_h5b_pathspec_filtering_excludes_ai_file() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    // Base commit
    let mut file = repo.filename("base.txt");
    file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Write AI content to disk
    let ai_content = "base\nAI line 1\nAI line 2\n";
    let file_path = repo.path().join("base.txt");
    fs::write(&file_path, ai_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // Write a carefully crafted checkpoints.jsonl where:
    // 1. AI checkpoint has line_attributions (correct)
    // 2. Human checkpoint has ONLY human line_attributions (no overrode, no AI)
    //    → checkpoint_entry_requires_post_processing returns FALSE
    //    → file excluded from pathspecs
    let git_dir = repo
        .git(&["rev-parse", "--git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let checkpoints_dir =
        std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&checkpoints_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let agent_id = "3bd30911a58cb074";

    // AI checkpoint: has AI line_attributions → would be included in pathspecs
    // Human checkpoint: has ONLY human line_attributions → excluded from pathspecs
    // If the pathspec builder only looks at the LAST checkpoint for a file,
    // the human checkpoint would exclude this file.
    let checkpoints = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"base.txt","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":3,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":2,"deletions":0,"additions_sloc":2,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}
{{"kind":"Human","diff":"fake2","author":"Test User","entries":[{{"file":"base.txt","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":1,"end_line":3,"author_id":"human","overrode":null}}]}}],"timestamp":2000,"transcript":null,"agent_id":null,"agent_metadata":null,"line_stats":{{"additions":0,"deletions":0,"additions_sloc":0,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(checkpoints_dir.join("checkpoints.jsonl"), &checkpoints).unwrap();

    let commit = repo.stage_all_and_commit("ai additions").unwrap();

    let has_ai_attestation = !commit.authorship_log.files.is_empty();

    // The post_commit pathspecs builder iterates ALL checkpoints (not just last).
    // The AI checkpoint's entry has AI line_attributions → file IS in pathspecs.
    // So this test SHOULD pass. If it doesn't, pathspec building is buggy.
    assert!(
        has_ai_attestation,
        "H5b: AI file should be in pathspecs because AI checkpoint has AI line_attributions.\n\
         If this fails, pathspec builder is only checking the last checkpoint per file.\n\
         authorship_log: {:?}",
        commit.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h5b_pathspec -- --nocapture 2>&1 | tail -20`
Expected: PASS if pathspec builder correctly iterates all checkpoints

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H5b — pathspec filtering with mixed AI/human checkpoints"
```

### Task 7: H3 — Pre-commit skip logic incorrectly skips when AI checkpoints exist

**Hypothesis:** In `checkpoint.rs:524-529`, the pre-commit checkpoint is skipped when `has_no_ai_edits && !has_initial_attributions`. The `has_no_ai_edits` check calls `working_log.all_ai_touched_files()` which returns files touched by AI checkpoints. If this check has a bug (e.g., returns empty for a valid AI working log), the Human checkpoint from pre-commit is never created, and `post_commit` may not have the final state snapshot it needs.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H3: Pre-commit skip logic — verify that when AI checkpoints exist,
/// the pre-commit checkpoint is NOT skipped.
///
/// The pre-commit hook runs checkpoint with kind=Human and should_skip_if_no_ai=true.
/// If all_ai_touched_files() incorrectly returns empty despite AI checkpoints existing,
/// the human checkpoint is skipped. This doesn't directly cause attribution loss
/// (AI checkpoints are still in the working log), but it could affect the
/// final-state snapshot that post-commit uses for hunk detection.
#[test]
fn test_h3_pre_commit_does_not_skip_when_ai_checkpoints_exist() {
    let repo = TestRepo::new();

    // Create base commit
    let mut file = repo.filename("code.rs");
    file.set_contents(crate::lines!["fn main() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // AI writes new code
    file.set_contents(crate::lines![
        "fn main() {}",
        "fn ai_function() {".ai(),
        "    println!(\"AI\");".ai(),
        "}".ai(),
    ]);

    // Read working log BEFORE commit to verify AI checkpoints exist
    let pre_commit_log = repo.current_working_logs();
    let pre_checkpoints = pre_commit_log.read_all_checkpoints().unwrap_or_default();
    let ai_checkpoint_count = pre_checkpoints
        .iter()
        .filter(|cp| cp.kind == git_ai::authorship::working_log::CheckpointKind::AiAgent)
        .count();

    assert!(
        ai_checkpoint_count > 0,
        "H3 precondition: AI checkpoints should exist before commit.\n\
         Found {} checkpoints total, {} AI.",
        pre_checkpoints.len(),
        ai_checkpoint_count
    );

    // Commit — this triggers pre-commit (Human checkpoint) then post-commit
    let commit = repo.stage_all_and_commit("ai code").unwrap();

    // Read working log AFTER commit to verify human checkpoint was added
    // (it should NOT have been skipped because AI checkpoints exist)
    // Note: after commit, working log may be cleaned up, so check the authorship log instead

    let has_ai_attestation = !commit.authorship_log.files.is_empty();

    assert!(
        has_ai_attestation,
        "H3: AI attribution should be present after commit.\n\
         AI checkpoints existed ({} found), pre-commit should not have skipped.\n\
         authorship_log: {:?}",
        ai_checkpoint_count,
        commit.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h3_pre_commit -- --nocapture 2>&1 | tail -20`
Expected: PASS — pre-commit skip logic should work correctly

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H3 — pre-commit skip logic with existing AI checkpoints"
```

---

## Chunk 3: Working Log Race Conditions and Rapid Commits

### Task 8: H4 — Working log premature deletion in rapid commit sequences

**Hypothesis:** After `post_commit` processes the working log, the data may be cleaned up. If a rapid sequence of commits occurs (commit A finishes, commit B starts before cleanup is complete), the working log for commit B's base could be corrupted or deleted.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H4: Rapid sequential commits — AI attribution survives across
/// back-to-back commits without the working log being corrupted.
///
/// This simulates: AI writes code → commit 1 → AI writes more code → commit 2
/// Both commits should have AI attribution, not just the first.
#[test]
fn test_h4_rapid_sequential_commits_preserve_ai_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut file = repo.filename("rapid.rs");
    file.set_contents(crate::lines!["// base"]);
    repo.stage_all_and_commit("base").unwrap();

    // First AI edit + commit
    file.set_contents(crate::lines![
        "// base",
        "fn first_ai() {}".ai(),
    ]);
    let commit1 = repo.stage_all_and_commit("first ai commit").unwrap();

    // Verify first commit has AI attribution
    let commit1_has_ai = !commit1.authorship_log.files.is_empty();
    assert!(
        commit1_has_ai,
        "H4 precondition: First commit should have AI attribution.\n\
         authorship_log: {:?}",
        commit1.authorship_log
    );

    // Second AI edit + commit (immediately after first)
    file.set_contents(crate::lines![
        "// base",
        "fn first_ai() {}".ai(),
        "fn second_ai() {}".ai(),
    ]);
    let commit2 = repo.stage_all_and_commit("second ai commit").unwrap();

    // Verify second commit ALSO has AI attribution
    let commit2_has_ai = !commit2.authorship_log.files.is_empty();
    assert!(
        commit2_has_ai,
        "H4: Second rapid commit should also have AI attribution.\n\
         If this fails, the working log from the first commit interfered.\n\
         commit1 authorship: {:?}\n\
         commit2 authorship: {:?}",
        commit1.authorship_log,
        commit2.authorship_log
    );
}

/// H4b: Three rapid commits with AI, verifying the middle one doesn't lose data.
#[test]
fn test_h4b_three_rapid_commits_all_have_ai_attribution() {
    let repo = TestRepo::new();

    let mut file = repo.filename("triple.rs");
    file.set_contents(crate::lines!["// base"]);
    repo.stage_all_and_commit("base").unwrap();

    // Commit 1
    file.set_contents(crate::lines!["// base", "fn one() {}".ai()]);
    let c1 = repo.stage_all_and_commit("commit 1").unwrap();
    assert!(!c1.authorship_log.files.is_empty(), "commit 1 has AI");

    // Commit 2
    file.set_contents(crate::lines![
        "// base",
        "fn one() {}",
        "fn two() {}".ai(),
    ]);
    let c2 = repo.stage_all_and_commit("commit 2").unwrap();
    assert!(!c2.authorship_log.files.is_empty(), "commit 2 has AI");

    // Commit 3
    file.set_contents(crate::lines![
        "// base",
        "fn one() {}",
        "fn two() {}",
        "fn three() {}".ai(),
    ]);
    let c3 = repo.stage_all_and_commit("commit 3").unwrap();
    assert!(
        !c3.authorship_log.files.is_empty(),
        "H4b: Third rapid commit should have AI attribution.\n\
         authorship_log: {:?}",
        c3.authorship_log
    );
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --test integration test_h4 -- --nocapture 2>&1 | tail -30`
Expected: Both PASS — rapid commits should preserve attribution

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H4 — rapid sequential commits preserve AI attribution"
```

### Task 9: H2c — Amend-then-commit loses ALL prior AI attribution

**Hypothesis:** Extension of H2 — after `git commit --amend`, the working log directory for the original commit is orphaned. If the user then makes a NEW commit (not another amend), the new commit's parent is the amended SHA, and the AI checkpoints from before the amend are under the old (now-orphaned) SHA.

This is the most likely real-world scenario for the reported bug: the user is working with Claude, Claude generates code, the user runs `git commit --amend` at some point, and from that point forward ALL AI attribution is lost until the working log happens to be re-keyed correctly.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H2c: The "amend poisoning" scenario — the most likely real-world cause.
///
/// Sequence:
/// 1. Commit A (base)
/// 2. AI writes code (checkpoints keyed to A)
/// 3. User commits → Commit B (AI attribution correct, working log keyed to A)
/// 4. AI writes more code (checkpoints keyed to B)
/// 5. User amends commit B → Commit B' (HEAD changes from B to B')
/// 6. AI writes even more code (checkpoints keyed to... B? B'?)
/// 7. User commits → Commit C
///
/// The question: does commit C have AI attribution from step 6?
/// If resolve_base_commit() returns B' (current HEAD), and the AI checkpoint
/// from step 6 was also keyed to B' (because HEAD was already B' when AI wrote),
/// then it should work. But if there's a timing issue...
#[test]
fn test_h2c_amend_then_new_commit_attribution_chain() {
    let repo = TestRepo::new();

    // Step 1: Base commit
    let mut file = repo.filename("chain.rs");
    file.set_contents(crate::lines!["// base"]);
    repo.stage_all_and_commit("base").unwrap();

    // Step 2-3: AI writes, commit normally
    file.set_contents(crate::lines!["// base", "fn ai_v1() {}".ai()]);
    let commit_b = repo.stage_all_and_commit("commit B").unwrap();
    assert!(
        !commit_b.authorship_log.files.is_empty(),
        "Precondition: commit B should have AI attribution"
    );

    // Step 4: AI writes more code (keyed to commit B's SHA)
    file.set_contents(crate::lines![
        "// base",
        "fn ai_v1() {}",
        "fn ai_v2() {}".ai(),
    ]);

    // Step 5: User amends (changes HEAD from B to B')
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Step 6: AI writes even more code (after amend, HEAD is now B')
    file.set_contents(crate::lines![
        "// base",
        "fn ai_v1() {}",
        "fn ai_v2() {}",
        "fn ai_v3() {}".ai(),
    ]);

    // Step 7: Commit C
    let commit_c = repo.stage_all_and_commit("commit C").unwrap();

    let has_ai = !commit_c.authorship_log.files.is_empty();
    assert!(
        has_ai,
        "H2c: Commit C (after amend of B) should have AI attribution from step 6.\n\
         If this fails, the amend operation 'poisoned' the working log keying.\n\
         authorship_log: {:?}",
        commit_c.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h2c_amend -- --nocapture 2>&1 | tail -20`
Expected: If this FAILS, we've found the most likely real-world manifestation of the bug.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H2c — amend-then-commit chain attribution preservation"
```

### Task 10: H5c — File content reads from working directory after commit

**Hypothesis:** `from_just_working_log` reads file content from the working directory (virtual_attribution.rs:403-406). After `git commit`, the working directory may differ from what was committed (e.g., if the user had unstaged changes). The line attribution coordinates from the checkpoint (which reference the pre-commit working directory state) may not match the post-commit file state, causing misalignment.

**Files:**
- Modify: `tests/integration/missing_attribution.rs`

- [ ] **Step 1: Write the test**

```rust
/// H5c: Working directory diverges from committed content — AI writes code,
/// user stages and commits only part of it, then the remaining unstaged changes
/// cause line number misalignment in the post-commit attribution.
#[test]
fn test_h5c_partial_stage_with_remaining_unstaged_changes() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("partial.rs");
    base.set_contents(crate::lines!["fn main() {}", "    // line 2", "    // line 3"]);
    repo.stage_all_and_commit("base").unwrap();

    // AI writes new lines throughout the file
    base.set_contents(crate::lines![
        "fn main() {}",
        "    // line 2",
        "    let x = 1;".ai(),
        "    // line 3",
        "    let y = 2;".ai(),
    ]);

    // Stage only the file (all changes)
    repo.git(&["add", "partial.rs"]).unwrap();

    // Now add MORE unstaged changes (modifying the file after staging)
    let post_stage_content =
        "fn main() {}\n    // line 2\n    let x = 1;\n    // line 3\n    let y = 2;\n    let z = 3; // unstaged\n";
    fs::write(repo.path().join("partial.rs"), post_stage_content).unwrap();

    // Commit (only staged content goes in, but working dir has extra line)
    let commit = repo.commit("partial commit").unwrap();

    let has_ai = !commit.authorship_log.files.is_empty();
    assert!(
        has_ai,
        "H5c: Partial staging with unstaged remainder should still attribute AI lines.\n\
         AI wrote lines 3 and 5 (let x, let y), both were staged and committed.\n\
         authorship_log: {:?}",
        commit.authorship_log
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration test_h5c_partial -- --nocapture 2>&1 | tail -20`
Expected: If this FAILS, working directory divergence causes attribution loss.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: H5c — partial staging with unstaged changes and AI attribution"
```

### Task 11: Final test run and summary

- [ ] **Step 1: Run all missing_attribution tests**

Run: `cargo test --test integration missing_attribution -- --nocapture 2>&1`
Expected: Document which tests pass and which fail.

- [ ] **Step 2: Commit final state**

```bash
git add tests/integration/missing_attribution.rs
git commit -m "test: complete missing AI attribution hypothesis test suite"
```

---

## Hypothesis Priority Summary

| # | Hypothesis | Test | Likelihood |
|---|-----------|------|------------|
| H1 | No AI checkpoint written | `test_h1_no_ai_checkpoint_*` | Baseline (not a bug) |
| H2 | Base commit SHA mismatch | `test_h2_*`, `test_h2b_*`, `test_h2c_*` | **HIGH** — most likely cause |
| H3 | Pre-commit skip logic | `test_h3_*` | Low |
| H4 | Rapid commit working log corruption | `test_h4_*`, `test_h4b_*` | Medium |
| H5 | Committed hunk detection failure | `test_h5_*`, `test_h5b_*`, `test_h5c_*` | Medium-High |

**H2 (SHA mismatch) is the primary suspect** because:
- It explains why the bug is intermittent (only happens after amend/rebase)
- It explains why "going back to past versions" didn't reproduce it (the working log was already orphaned)
- It explains why "Git AI will be going strong and then, all of a sudden, commits will be created that have no attribution at all" — a single amend operation could poison all subsequent commits until the user happens to create a fresh branch or the working log directory is re-keyed correctly.
