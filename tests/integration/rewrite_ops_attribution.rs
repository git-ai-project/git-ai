/// Deterministic regression tests for attribution bugs found by the fuzzer
/// on the rewrite-ops branch. Each test models a specific fuzzer failure pattern
/// using explicit file writes and checkpoint calls.
use std::fs;

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

// =============================================================================
// Category A: Secondary file missing from authorship note
//
// Reproduction of fuzz_checkpoint_heavy_0:
// A multi-file commit includes fuzz_main.txt, fuzz_secondary_2.txt, and
// fuzz_secondary_3.txt — all with checkpointed edits — but the resulting
// authorship note only contains entries for some files, dropping others.
// =============================================================================

/// Multi-file commit where secondary file has AI checkpoint but is missing from note.
///
/// Models the fuzz_checkpoint_heavy_0 failure:
/// 1. Initial commit with AI on main file
/// 2. Selective commit of main file only (secondary stays dirty)
/// 3. Edit secondary files with checkpoints
/// 4. Commit all files together
/// 5. Note should include ALL files with attributed edits
#[test]
fn test_multifile_commit_secondary_file_missing_from_note() {
    let repo = TestRepo::new();
    let main_path = repo.path().join("main.txt");
    let sec_path = repo.path().join("secondary.txt");

    // Initial commit: AI edits on main file
    fs::write(&main_path, "AAA\nAAA\nAAA\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Edit both files, but only commit main
    fs::write(&main_path, "AAA\nAAA\nAAA\nBBB\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    fs::write(&sec_path, "CCC\nCCC\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "secondary.txt"])
        .unwrap();

    // Only stage and commit main.txt — secondary stays dirty
    repo.git(&["add", "main.txt"]).unwrap();
    repo.commit("commit main only").unwrap();

    // Now commit everything (secondary.txt is still dirty from before)
    fs::write(&sec_path, "CCC\nCCC\nDDD\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "secondary.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("commit all files").unwrap();

    // Both files should have attribution
    let mut main_file = repo.filename("main.txt");
    main_file.assert_committed_lines(crate::lines![
        "AAA".ai(),
        "AAA".ai(),
        "AAA".ai(),
        "BBB".ai(),
    ]);

    let mut sec_file = repo.filename("secondary.txt");
    sec_file.assert_committed_lines(crate::lines![
        "CCC".ai(),
        "CCC".ai(),
        "DDD".ai(),
    ]);
}

/// Simpler multi-file case: both files edited and committed in one shot.
#[test]
fn test_multifile_commit_both_files_attributed() {
    let repo = TestRepo::new();
    let main_path = repo.path().join("main.txt");
    let sec_path = repo.path().join("other.txt");

    // Initial commit
    fs::write(&main_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Edit both files with AI checkpoints
    fs::write(&main_path, "base\nnew-main\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    fs::write(&sec_path, "new-other\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "other.txt"]).unwrap();

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("multi-file commit").unwrap();

    let mut main_file = repo.filename("main.txt");
    main_file.assert_committed_lines(crate::lines!["base".ai(), "new-main".ai()]);

    let mut sec_file = repo.filename("other.txt");
    sec_file.assert_committed_lines(crate::lines!["new-other".ai()]);
}

// =============================================================================
// Category B (human attributed as AI): Cherry-pick conflict + abort
//
// Reproduction of fuzz_combined_0:
// After a cherry-pick that conflicts and is aborted, the commit that follows
// has a note claiming ALL lines as AI, even though some were KnownHuman.
// The note's session range (1-5) doesn't distinguish human from AI lines.
// =============================================================================

/// Exact reproduction of fuzz_combined_0 failure sequence.
///
/// The critical sequence is:
/// 1. Delete-recreate file (8 lines: H=Ai×4, I=Human×1, J=Ai×3)
/// 2. checkpoint-storm (many rapid edits, 22 lines total), commit
/// 3. hard-reset HEAD~1 (back to 8 lines)
/// 4. overwrite-and-rollback: Y=Ai OverwriteAll 2, Z=Human Append 2, commit
/// 5. cherry-pick-conflict: feature branch prepends a=Human×4, main prepends b=Ai×1
///    cherry-pick conflicts, aborts
/// 6. verify: the "main commit" from step 5 has b(line1) + Y,Y,Z,Z
///    note should say line 1 = AI, lines 2-3 = AI (Y), lines 4-5 = Human (Z)
#[test]
fn test_cherry_pick_abort_main_commit_note_accuracy() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Step 1: Initial commit (simulates delete-recreate result)
    fs::write(&file_path, "HHHH\nHHHH\nHHHH\nHHHH\nIIII\nJJJJ\nJJJJ\nJJJJ\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    // Checkpoint the human line separately
    fs::write(&file_path, "HHHH\nHHHH\nHHHH\nHHHH\nIIII\nJJJJ\nJJJJ\nJJJJ\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.stage_all_and_commit("delete-recreate commit").unwrap();

    // Step 2: checkpoint-storm with many edits, then commit
    fs::write(
        &file_path,
        "storm1\nstorm2\nstorm3\nHHHH\nHHHH\nHHHH\nHHHH\nIIII\nJJJJ\nJJJJ\nJJJJ\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("storm commit").unwrap();

    // Step 3: hard-reset to the delete-recreate commit
    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();

    // Step 4: overwrite-and-rollback: overwrite entire file with AI, then append human
    fs::write(&file_path, "YYYY\nYYYY\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    fs::write(&file_path, "YYYY\nYYYY\nZZZZ\nZZZZ\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("overwrite-and-rollback").unwrap();

    // Step 5: cherry-pick-conflict
    // Create feature branch from HEAD~1 (the delete-recreate state)
    repo.git(&["checkout", "-b", "cp-feature", "HEAD~1"]).unwrap();
    // Feature: prepend human lines
    fs::write(
        &file_path,
        "aaaa\naaaa\naaaa\naaaa\nHHHH\nHHHH\nHHHH\nHHHH\nIIII\nJJJJ\nJJJJ\nJJJJ\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature: prepend human").unwrap();
    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main (overwrite-and-rollback commit)
    repo.git(&["checkout", "-"]).unwrap();
    // Prepend AI line on main to create conflict
    fs::write(&file_path, "bbbb\nYYYY\nYYYY\nZZZZ\nZZZZ\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("main: prepend ai").unwrap();

    // Cherry-pick feature commit — should conflict (both prepend)
    let cp_result = repo.git(&["cherry-pick", &feature_sha]);
    if cp_result.is_err() {
        repo.git(&["cherry-pick", "--abort"]).ok();
    }

    // After abort: file should be in "main: prepend ai" state
    // = bbbb, YYYY, YYYY, ZZZZ, ZZZZ
    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "bbbb".ai(),
        "YYYY".ai(),
        "YYYY".ai(),
        "ZZZZ".human(),
        "ZZZZ".human(),
    ]);
}

/// Simpler version: interleaved human and AI edits, note must not lump them together.
#[test]
fn test_interleaved_human_ai_edits_not_lumped() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "init\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Human edits: prepend 3 lines
    fs::write(&file_path, "human1\nhuman2\nhuman3\ninit\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();

    // AI edits: prepend 1 line
    fs::write(&file_path, "ai-top\nhuman1\nhuman2\nhuman3\ninit\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("mixed commit").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "ai-top".ai(),
        "human1".human(),
        "human2".human(),
        "human3".human(),
        "init".ai(),
    ]);
}

// =============================================================================
// Category B (AI attributed as human): Multi-squash produces incomplete note
//
// Reproduction of fuzz_destructive_0:
// After squashing 3 commits, the resulting note only covers some lines,
// leaving gaps where AI lines have no attestation (default to human).
// =============================================================================

/// Multi-squash: squash 3 commits with AI content, note must cover all AI lines.
///
/// Models the fuzz_destructive_0 failure:
/// 1. Make 3 commits on a feature branch with AI edits
/// 2. Squash merge them into main
/// 3. The squashed commit's note must attribute ALL AI lines
#[test]
fn test_multi_squash_incomplete_note() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch: 3 commits with AI edits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    fs::write(&file_path, "base\nline-c\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature 1").unwrap();

    fs::write(&file_path, "base\nline-c\nline-d\nline-d\nline-d\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature 2").unwrap();

    // Third commit has a human DeleteAndInsert
    fs::write(&file_path, "base\nline-c\nhuman-e\nhuman-e\nline-d\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature 3").unwrap();

    // Switch to main and squash merge
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("squash all").unwrap();

    // Verify: all lines must have correct attribution
    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "base".ai(),
        "line-c".ai(),
        "human-e".human(),
        "human-e".human(),
        "line-d".ai(),
    ]);
}

/// Reset then re-edit and squash: AI lines in the middle must not fall into gaps.
#[test]
fn test_reset_reedit_squash_no_attribution_gaps() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit with mixed content
    fs::write(&file_path, "aaa\nbbb\nccc\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Second commit: add more AI lines
    fs::write(&file_path, "aaa\nbbb\nccc\nddd\neee\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("add more").unwrap();

    // Reset to initial
    repo.git(&["reset", "--mixed", "HEAD~1"]).unwrap();

    // Re-edit: human prepends, then AI appends
    fs::write(&file_path, "human-top\naaa\nbbb\nccc\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();

    fs::write(&file_path, "human-top\naaa\nbbb\nccc\nai-bot\nai-bot\nai-bot\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("re-edit after reset").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "human-top".human(),
        "aaa".ai(),
        "bbb".ai(),
        "ccc".ai(),
        "ai-bot".ai(),
        "ai-bot".ai(),
        "ai-bot".ai(),
    ]);
}

/// Rebase then commit: notes should transfer through rebase for rebased commits.
#[test]
fn test_rebase_preserves_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch: AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&file_path, "base\nfeature-ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature").unwrap();

    // Advance main with a non-conflicting change
    repo.git(&["checkout", &main_branch]).unwrap();
    let other_path = repo.path().join("other.txt");
    fs::write(&other_path, "main-work\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "other.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("advance main").unwrap();

    // Rebase feature onto main (through daemon)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &main_branch]).unwrap();

    // Merge back (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "feature"]).unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines!["base".ai(), "feature-ai".ai()]);
}

/// Minimal reproduction: hard reset erases working logs, subsequent AI checkpoints
/// produce incomplete authorship notes.
#[test]
fn test_hard_reset_then_ai_checkpoint_loses_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Second commit
    fs::write(&file_path, "base\nextra\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    // Hard reset back to initial
    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();

    // New AI edits after hard reset
    fs::write(&file_path, "new-ai-1\nnew-ai-2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("after hard reset").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "new-ai-1".ai(),
        "new-ai-2".ai(),
    ]);
}

// =============================================================================
// Category C: Race condition — checkpoint arrives before daemon processes reset
//
// Root cause: `git reset` fires a trace2 event that the daemon processes
// asynchronously (via family sequencer). If a `git-ai checkpoint` arrives
// before the daemon has updated working log state from the reset, the
// checkpoint diff is computed against stale (pre-reset) state, producing
// incomplete attribution (first line(s) missing from note).
//
// The race is between the trace2 ingest path (PendingRoot → ReadyCommand)
// and the checkpoint path (FamilyMsg::ApplyCheckpoint).
// =============================================================================

/// Demonstrates the race: no delay after reset → first AI line dropped.
#[test]
fn test_hard_reset_race_condition_no_delay() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    fs::write(&file_path, "base\nextra\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();

    fs::write(&file_path, "new-1\nnew-2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("after reset").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "new-1".ai(),
        "new-2".ai(),
    ]);
}

/// Same test but with 200ms delay after reset — passes because daemon has time
/// to process the trace2 event. Confirms the race condition diagnosis.
#[test]
fn test_hard_reset_race_condition_with_delay() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    fs::write(&file_path, "base\nextra\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));

    fs::write(&file_path, "new-1\nnew-2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("after reset").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "new-1".ai(),
        "new-2".ai(),
    ]);
}

/// Same as above but with --mixed reset to see if bug is --hard specific.
#[test]
fn test_mixed_reset_then_ai_checkpoint() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Second commit
    fs::write(&file_path, "base\nextra\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    // Mixed reset back to initial
    repo.git(&["reset", "--mixed", "HEAD~1"]).unwrap();

    // New AI edits after mixed reset (same content as hard reset test)
    fs::write(&file_path, "new-ai-1\nnew-ai-2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("after mixed reset").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "new-ai-1".ai(),
        "new-ai-2".ai(),
    ]);
}

/// Hard reset then mixed AI and human checkpoints — both must be correctly attributed.
#[test]
fn test_hard_reset_mixed_checkpoint_types() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "init\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Second commit to create something to reset
    fs::write(&file_path, "init\nmore\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    // Hard reset
    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();

    // Human edits first
    fs::write(&file_path, "human-line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();

    // Then AI appends
    fs::write(&file_path, "human-line\nai-line\nai-line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("post-reset mixed").unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "human-line".human(),
        "ai-line".ai(),
        "ai-line".ai(),
    ]);
}

/// Amend: amending a commit should preserve attribution for unchanged lines
/// and correctly attribute new lines.
#[test]
fn test_amend_preserves_existing_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Initial commit
    fs::write(&file_path, "first\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Second commit with AI
    fs::write(&file_path, "first\nsecond-ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("second").unwrap();

    // Amend: add a human line
    fs::write(&file_path, "first\nsecond-ai\nthird-human\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "second amended"]).unwrap();

    let mut file = repo.filename("main.txt");
    file.assert_committed_lines(crate::lines![
        "first".ai(),
        "second-ai".ai(),
        "third-human".human(),
    ]);
}
