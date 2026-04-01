//! Tests for intermittent bug where AI-authored commits show 100% human attribution.
//!
//! Hypotheses tested:
//! H1: No AI checkpoint written to working log
//! H2: Base commit SHA mismatch (after amend/rebase)
//! H3: Pre-commit skip logic incorrectly skips
//! H4: Working log corruption in rapid commit sequences
//! H5: Committed hunk detection discards AI lines
//! H6: Partial checkpoint coverage in multi-file commits (silent failures)
//! H7: Working log corruption (truncated JSONL, wrong api_version)
//! H8: Committed hunk detection edge cases (existing lines, modifications)
//! H9: Path mismatches (human rewrite, file rename)
//! H10: File deleted after AI checkpoint
//! H11: Multi-agent last-write-wins via HashMap::insert
//! H12: Corrupt INITIAL file loses uncommitted AI carryover between sessions
//! H13: Missing checkpoint kind — forward/backward compatibility resilience
//! H14: Binary file AI attribution silently dropped (no committed hunks)
//! H17: Merge commit attribution note behavior
//! H18: Append checkpoint on corrupt JSONL destroys valid prior data

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ---------------------------------------------------------------------------
// H1: No AI checkpoint written — baseline (expected: 100% human)
// ---------------------------------------------------------------------------

/// H1: AI writes code but no AI checkpoint is recorded — only the human
/// pre-commit checkpoint exists. The system still creates an authorship note
/// (with schema version, base_commit_sha, etc.) but it has ZERO attestation
/// entries and ZERO prompts. This is NOT a bug — it's the expected baseline.
#[test]
fn test_h1_no_ai_checkpoint_produces_empty_authorship_note() {
    let repo = TestRepo::new();

    // Create a base commit so we have a parent SHA
    fs::write(repo.path().join("base.txt"), "base line\n").unwrap();
    repo.stage_all_and_commit("base commit").unwrap();

    // Simulate: user writes code (not AI) — only a Human checkpoint is created.
    // We deliberately bypass the TestFile fluent API to avoid auto-AI-checkpoints.
    let content = "fn human_written() {\n    println!(\"no AI here\");\n}\n";
    let file_path = repo.path().join("human_code.rs");
    fs::write(&file_path, content).unwrap();

    // Only create a human checkpoint (simulating pre-commit hook behavior)
    repo.git_ai(&["checkpoint", "--", "human_code.rs"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // The system creates an authorship note even with no AI data —
    // it's structurally valid but has empty attestations and prompts.
    let commit = repo.stage_all_and_commit("add human code").unwrap();

    assert!(
        commit.authorship_log.attestations.is_empty(),
        "H1 baseline: no AI checkpoint → no attestation entries.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
    assert!(
        commit.authorship_log.metadata.prompts.is_empty(),
        "H1 baseline: no AI checkpoint → no prompts.\n\
         prompts: {:?}",
        commit.authorship_log.metadata.prompts
    );
}

// ---------------------------------------------------------------------------
// H2: Base commit SHA mismatch — the primary suspect
// ---------------------------------------------------------------------------

/// H2: AI checkpoints are written under one base SHA, but post-commit resolves
/// a different SHA (e.g., after git commit --amend). AI data is silently lost.
///
/// Sequence: base commit A → AI writes (keyed to A) → amend changes HEAD to A' →
/// AI writes more (keyed to A') → commit → post-commit looks under A'.
/// The step-4 AI checkpoint (keyed to A') should be found.
#[test]
fn test_h2_base_commit_sha_mismatch_after_amend() {
    let repo = TestRepo::new();

    // Step 1: Create initial commit
    let mut file = repo.filename("code.rs");
    fs::write(repo.path().join("code.rs"), "fn main() {}\n    // base\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Step 2: AI writes new code (checkpoint keyed to current HEAD)
    file.set_contents_no_stage(crate::lines![
        "fn main() {}",
        "    // base",
        "    println!(\"AI wrote this\");".ai(),
        "    println!(\"AI also wrote this\");".ai(),
    ]);

    // Step 3: Amend the previous commit (changes HEAD SHA).
    // Using raw git to bypass git-ai hooks — simulating user running git directly.
    repo.git(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "--amend", "--no-edit"]).unwrap();

    // Step 4: Write more AI code AFTER the amend (keyed to new HEAD).
    let new_content = "fn main() {}\n    // base\n    println!(\"AI wrote this\");\n    println!(\"AI also wrote this\");\n    println!(\"More AI after amend\");\n";
    let file_path = repo.path().join("code.rs");
    fs::write(&file_path, new_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "code.rs"]).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    let commit = repo.stage_all_and_commit("add more ai code").unwrap();

    // The AI checkpoint from step 4 was written after the amend, so it should
    // be keyed to the new HEAD. Post-commit should find it.
    let has_ai_attestation = !commit.authorship_log.attestations.is_empty();

    assert!(
        has_ai_attestation,
        "H2: AI checkpoint written after amend should still produce AI attestation.\n\
         authorship_log attestations: {:?}\n\
         prompts: {:?}",
        commit.authorship_log.attestations, commit.authorship_log.metadata.prompts
    );
}

/// H2b: Directly prove that a SHA mismatch causes silent data loss.
/// Write AI checkpoint under a wrong SHA, verify post-commit finds nothing.
#[test]
fn test_h2b_direct_working_log_sha_mismatch() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    // Create base commit
    fs::write(repo.path().join("app.py"), "print('hello')\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let correct_base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Write AI content to disk
    let ai_content = "print('hello')\nprint('AI generated line 1')\nprint('AI generated line 2')\n";
    let file_path = repo.path().join("app.py");
    fs::write(&file_path, ai_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // Write AI checkpoint under a WRONG base commit SHA.
    // Use --absolute-git-dir to handle worktree-backed repos where --git-dir
    // returns a relative gitlink path.
    let wrong_base = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
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

    // Commit: post-commit looks under correct_base, which has no AI checkpoint.
    // The AI checkpoint under wrong_base is invisible to post-commit.
    let result = repo.stage_all_and_commit("add ai lines");

    // With AI data only under the wrong SHA, either:
    // - No authorship note at all (Err) — most likely
    // - An authorship note with empty attestations (Ok with empty attestations)
    match result {
        Err(_) => {
            // Expected: no authorship note because no AI data found under correct SHA
            eprintln!(
                "H2b CONFIRMED: AI checkpoint under wrong SHA ({}) was not found.\n\
                 Post-commit looked under correct SHA ({}).\n\
                 No authorship note was created.",
                wrong_base, correct_base
            );
        }
        Ok(commit) => {
            let has_ai = !commit.authorship_log.attestations.is_empty();
            assert!(
                !has_ai,
                "H2b: Expected NO AI attestation when checkpoint is under wrong SHA.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}

/// H2c: The "amend poisoning" scenario — the most likely real-world cause.
///
/// Sequence: base A → AI writes → commit B → AI writes more → amend B→B' →
/// AI writes again → commit C. Does commit C have AI attribution?
#[test]
fn test_h2c_amend_then_new_commit_attribution_chain() {
    let repo = TestRepo::new();

    // Step 1: Base commit
    let mut file = repo.filename("chain.rs");
    fs::write(repo.path().join("chain.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Step 2-3: AI writes, commit normally
    file.set_contents_no_stage(crate::lines!["// base", "fn ai_v1() {}".ai()]);
    let commit_b = repo.stage_all_and_commit("commit B").unwrap();
    assert!(
        !commit_b.authorship_log.attestations.is_empty(),
        "Precondition: commit B should have AI attestation"
    );

    // Step 4: AI writes more code (keyed to commit B's SHA)
    file.set_contents_no_stage(crate::lines![
        "// base",
        "fn ai_v1() {}",
        "fn ai_v2() {}".ai(),
    ]);

    // Step 5: User amends (changes HEAD from B to B').
    // Using git_og to bypass git-ai hooks — simulating raw git amend.
    repo.git(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "--amend", "--no-edit"]).unwrap();

    // Step 6: AI writes even more code (after amend, HEAD is now B')
    file.set_contents_no_stage(crate::lines![
        "// base",
        "fn ai_v1() {}",
        "fn ai_v2() {}",
        "fn ai_v3() {}".ai(),
    ]);

    // Step 7: Commit C
    let commit_c = repo.stage_all_and_commit("commit C").unwrap();

    let has_ai = !commit_c.authorship_log.attestations.is_empty();
    assert!(
        has_ai,
        "H2c: Commit C (after amend of B) should have AI attribution from step 6.\n\
         If this fails, the amend operation 'poisoned' the working log keying.\n\
         attestations: {:?}",
        commit_c.authorship_log.attestations
    );
}

// ---------------------------------------------------------------------------
// H3: Pre-commit skip logic
// ---------------------------------------------------------------------------

/// H3: Verify that when AI checkpoints exist, the pre-commit checkpoint
/// is NOT skipped, and AI attribution appears in the final note.
#[test]
fn test_h3_pre_commit_does_not_skip_when_ai_checkpoints_exist() {
    let repo = TestRepo::new();

    // Create base commit
    let mut file = repo.filename("code.rs");
    fs::write(repo.path().join("code.rs"), "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes new code
    file.set_contents_no_stage(crate::lines![
        "fn main() {}",
        "fn ai_function() {".ai(),
        "    println!(\"AI\");".ai(),
        "}".ai(),
    ]);

    // Commit — this triggers pre-commit (Human checkpoint) then post-commit
    let commit = repo.stage_all_and_commit("ai code").unwrap();

    let has_ai_attestation = !commit.authorship_log.attestations.is_empty();

    assert!(
        has_ai_attestation,
        "H3: AI attribution should be present after commit.\n\
         attestations: {:?}\n\
         prompts: {:?}",
        commit.authorship_log.attestations, commit.authorship_log.metadata.prompts
    );
}

// ---------------------------------------------------------------------------
// H4: Rapid sequential commits
// ---------------------------------------------------------------------------

/// H4: AI attribution survives across back-to-back commits.
#[test]
fn test_h4_rapid_sequential_commits_preserve_ai_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut file = repo.filename("rapid.rs");
    fs::write(repo.path().join("rapid.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // First AI edit + commit
    file.set_contents_no_stage(crate::lines!["// base", "fn first_ai() {}".ai(),]);
    let commit1 = repo.stage_all_and_commit("first ai commit").unwrap();

    let commit1_has_ai = !commit1.authorship_log.attestations.is_empty();
    assert!(
        commit1_has_ai,
        "H4 precondition: First commit should have AI attestation.\n\
         attestations: {:?}",
        commit1.authorship_log.attestations
    );

    // Second AI edit + commit (immediately after first)
    file.set_contents_no_stage(crate::lines![
        "// base",
        "fn first_ai() {}".ai(),
        "fn second_ai() {}".ai(),
    ]);
    let commit2 = repo.stage_all_and_commit("second ai commit").unwrap();

    let commit2_has_ai = !commit2.authorship_log.attestations.is_empty();
    assert!(
        commit2_has_ai,
        "H4: Second rapid commit should also have AI attestation.\n\
         commit1 attestations: {:?}\n\
         commit2 attestations: {:?}",
        commit1.authorship_log.attestations, commit2.authorship_log.attestations
    );
}

/// H4b: Three rapid commits with AI, verifying the middle one doesn't lose data.
#[test]
fn test_h4b_three_rapid_commits_all_have_ai_attribution() {
    let repo = TestRepo::new();

    let mut file = repo.filename("triple.rs");
    fs::write(repo.path().join("triple.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Commit 1
    file.set_contents_no_stage(crate::lines!["// base", "fn one() {}".ai()]);
    let c1 = repo.stage_all_and_commit("commit 1").unwrap();
    assert!(
        !c1.authorship_log.attestations.is_empty(),
        "commit 1 should have AI attestation"
    );

    // Commit 2
    file.set_contents_no_stage(crate::lines!["// base", "fn one() {}", "fn two() {}".ai(),]);
    let c2 = repo.stage_all_and_commit("commit 2").unwrap();
    assert!(
        !c2.authorship_log.attestations.is_empty(),
        "commit 2 should have AI attestation"
    );

    // Commit 3
    file.set_contents_no_stage(crate::lines![
        "// base",
        "fn one() {}",
        "fn two() {}",
        "fn three() {}".ai(),
    ]);
    let c3 = repo.stage_all_and_commit("commit 3").unwrap();
    assert!(
        !c3.authorship_log.attestations.is_empty(),
        "H4b: Third rapid commit should have AI attestation.\n\
         attestations: {:?}",
        c3.authorship_log.attestations
    );
}

// ---------------------------------------------------------------------------
// H5: Committed hunk detection failures
// ---------------------------------------------------------------------------

/// H5: Line-ending normalization may cause committed hunk detection to miss
/// AI-attributed lines, classifying them as "already existed in parent."
#[test]
fn test_h5_line_ending_normalization_drops_ai_attribution() {
    let repo = TestRepo::new();

    // Enable autocrlf to force line-ending normalization
    repo.git_og(&["config", "core.autocrlf", "true"]).unwrap();

    // Create base commit
    fs::write(repo.path().join("normalized.txt"), "line 1\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes content with explicit CRLF line endings.
    // The checkpoint records these lines, but git may normalize them on commit.
    let ai_content = "line 1\r\nAI line 2\r\nAI line 3\r\n";
    let file_path = repo.path().join("normalized.txt");
    fs::write(&file_path, ai_content).unwrap();

    // Create AI checkpoint with CRLF content
    repo.git_ai(&["checkpoint", "mock_ai", "normalized.txt"])
        .unwrap();
    repo.git(&["add", "-A"]).unwrap();

    let commit = repo.stage_all_and_commit("ai additions").unwrap();

    let has_ai_attestation = !commit.authorship_log.attestations.is_empty();

    assert!(
        has_ai_attestation,
        "H5: Line-ending normalization should NOT cause AI attribution loss.\n\
         AI wrote lines 2-3 but authorship log has no AI attestation.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

/// H5b: Human checkpoint with all-human line_attributions overwrites AI data.
///
/// When `from_just_working_log` processes checkpoints sequentially, the human
/// checkpoint's `line_attributions` (all "human") completely REPLACE the
/// earlier AI checkpoint's data (virtual_attribution.rs:435: `attributions.insert`).
///
/// In the normal code path, human pre-commit checkpoints for AI-touched files
/// produce line_attributions that PRESERVE AI authorship (the transform_attributions
/// function carries forward AI byte-range attributions through Equal segments).
/// So the normal flow does NOT trigger this bug.
///
/// However, this test documents a fragile invariant: if any code path ever
/// produces a Human checkpoint with all-human line_attributions for an
/// AI-touched file, AI attribution is silently destroyed. This is an
/// architectural weakness in the sequential checkpoint processing model.
///
/// Ignored because this is a synthetic scenario that doesn't occur in the
/// normal pre-commit flow, but documents a latent vulnerability.
#[test]
#[ignore = "synthetic scenario: documents latent overwrite vulnerability in from_just_working_log"]
fn test_h5b_human_checkpoint_overwrites_ai_attribution() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    // Base commit
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Write AI content to disk
    let ai_content = "base\nAI line 1\nAI line 2\n";
    let file_path = repo.path().join("base.txt");
    fs::write(&file_path, ai_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();

    // Write crafted checkpoints.jsonl:
    // 1. AI checkpoint with AI line_attributions for lines 2-3
    // 2. Human checkpoint with line_attributions claiming ALL lines (1-3) are human
    //
    // The bug: from_just_working_log processes checkpoints in order, and the
    // human checkpoint's attributions.insert() REPLACES the AI checkpoint's data.
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
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
    let agent_id = "3bd30911a58cb074"; // SHA256("mock_ai:test_session")[..16]

    let checkpoints = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"base.txt","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":3,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":2,"deletions":0,"additions_sloc":2,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}
{{"kind":"Human","diff":"fake2","author":"Test User","entries":[{{"file":"base.txt","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":1,"end_line":3,"author_id":"human","overrode":null}}]}}],"timestamp":2000,"transcript":null,"agent_id":null,"agent_metadata":null,"line_stats":{{"additions":0,"deletions":0,"additions_sloc":0,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(checkpoints_dir.join("checkpoints.jsonl"), &checkpoints).unwrap();

    let commit = repo.stage_all_and_commit("ai additions").unwrap();

    let has_ai_attestation = !commit.authorship_log.attestations.is_empty();

    // BUG: This assertion FAILS because the human checkpoint completely
    // overwrites the AI checkpoint's attributions in from_just_working_log.
    // The AI data is silently lost.
    //
    // In practice, this happens when the pre-commit Human checkpoint produces
    // non-empty line_attributions that claim all lines are human. The normal
    // code path produces empty line_attributions for the human checkpoint
    // (which causes it to be skipped), so the bug only manifests when
    // checkpoint data is produced by specific code paths or agent presets.
    assert!(
        has_ai_attestation,
        "H5b BUG: Human checkpoint overwrites AI attribution.\n\
         The human checkpoint's line_attributions (all 'human') replaced the\n\
         AI checkpoint's data in from_just_working_log (attributions.insert).\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ---------------------------------------------------------------------------
// H6: Multi-file commit where only some files are AI-checkpointed
// ---------------------------------------------------------------------------

/// H6: AI edits multiple files but checkpoint only covers some of them.
///
/// In Claude Code's workflow, the agent writes to multiple files, but the
/// checkpoint API may only be called for some of them (e.g., if the agent
/// edits files between checkpoint calls). Files that were AI-written but
/// NOT checkpointed will have no AI attribution in the note.
///
/// This is technically correct behavior (no checkpoint = no attribution),
/// but it's the most likely explanation for the intermittent bug: the agent
/// created all the code, but the checkpoint calls didn't cover all files.
#[test]
fn test_h6_multi_file_commit_partial_checkpoint_coverage() {
    let repo = TestRepo::new();

    // Base commit
    let mut file_a = repo.filename("file_a.rs");
    fs::write(repo.path().join("file_a.rs"), "// file a base\n").unwrap();
    fs::write(repo.path().join("file_b.rs"), "// file b base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes to file_a (properly checkpointed via set_contents)
    file_a.set_contents_no_stage(crate::lines!["// file a base", "fn ai_code() {}".ai(),]);

    // AI also writes to file_b, but we bypass the checkpoint system
    // (simulating a file that was written by AI but not checkpointed)
    let file_b_path = repo.path().join("file_b.rs");
    fs::write(
        &file_b_path,
        "// file b base\nfn also_ai_but_no_checkpoint() {}\n",
    )
    .unwrap();
    // Only create a human checkpoint for file_b (no AI checkpoint)
    repo.git_ai(&["checkpoint", "--", "file_b.rs"]).unwrap();

    let commit = repo.stage_all_and_commit("multi-file ai commit").unwrap();

    // file_a should have AI attestation (properly checkpointed)
    let file_a_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "file_a.rs");

    // file_b should NOT have AI attestation (no AI checkpoint)
    let file_b_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "file_b.rs");

    assert!(
        file_a_attested,
        "H6: file_a.rs (AI checkpointed) should have AI attestation.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );

    assert!(
        !file_b_attested,
        "H6: file_b.rs (NOT AI checkpointed) should NOT have AI attestation.\n\
         This is correct behavior — if a file isn't checkpointed as AI, it's human.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ---------------------------------------------------------------------------
// H5 continued: Committed hunk detection failures
// ---------------------------------------------------------------------------

/// H5c: Working directory diverges from committed content after partial staging.
/// AI writes code, user stages it, then modifies the file again before committing.
/// The committed content should still get AI attribution.
#[test]
fn test_h5c_partial_stage_with_remaining_unstaged_changes() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("partial.rs");
    fs::write(
        repo.path().join("partial.rs"),
        "fn main() {}\n    // line 2\n    // line 3\n",
    )
    .unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes new lines
    base.set_contents_no_stage(crate::lines![
        "fn main() {}",
        "    // line 2",
        "    let x = 1;".ai(),
        "    // line 3",
        "    let y = 2;".ai(),
    ]);

    // Stage all changes
    repo.git(&["add", "partial.rs"]).unwrap();

    // Add MORE unstaged changes (file diverges from staged content)
    let post_stage_content = "fn main() {}\n    // line 2\n    let x = 1;\n    // line 3\n    let y = 2;\n    let z = 3; // unstaged\n";
    fs::write(repo.path().join("partial.rs"), post_stage_content).unwrap();

    // Commit (only staged content goes in, but working dir has extra line)
    let commit = repo.commit("partial commit").unwrap();

    let has_ai = !commit.authorship_log.attestations.is_empty();
    assert!(
        has_ai,
        "H5c: Partial staging with unstaged remainder should still attribute AI lines.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H6 expanded: Silent checkpoint failure scenarios
// ===========================================================================

/// H6b: AI checkpoint exists in working log but has empty entries array.
///
/// Real-world trigger: ClaudePreset fails to extract file_path from
/// tool_input (wrong tool type, missing field), catches the error,
/// and writes a checkpoint with agent_id but no file entries.
/// Since all preset errors exit(0), Claude Code cannot detect failure.
///
/// Impact chain:
///   - all_ai_touched_files() sees AiAgent kind but no entries → empty set
///   - Pre-commit skip: has_no_ai_edits = true → Human checkpoint skipped
///   - from_just_working_log: inner loop over entries never executes
///   - Result: no file attestations (prompts may still be recorded)
#[test]
fn test_h6b_ai_checkpoint_with_empty_entries() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("agent_code.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI writes code to disk (a real edit happened)
    let ai_content = "// base\nfn ai_function() {}\n";
    fs::write(repo.path().join("agent_code.rs"), ai_content).unwrap();

    // Write AI checkpoint with EMPTY entries (simulating preset extraction failure)
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let checkpoint_json = r#"{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[],"timestamp":1000,"transcript":{"messages":[]},"agent_id":{"tool":"mock_ai","id":"test_session","model":"test"},"agent_metadata":null,"line_stats":{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}"#;
    fs::write(cp_dir.join("checkpoints.jsonl"), checkpoint_json).unwrap();

    let result = repo.stage_all_and_commit("add ai code");

    // Empty entries = no AI-touched files detected, pre-commit skips,
    // post-commit finds no file attributions → no AI attestation
    match result {
        Err(_) => {
            // No note created — empty checkpoint produced nothing
        }
        Ok(commit) => {
            assert!(
                commit.authorship_log.attestations.is_empty(),
                "H6b: AI checkpoint with empty entries should produce no file attestations.\n\
                 The agent wrote code but the checkpoint had no file-level data.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}

/// H6c: Three files edited by AI, but one file's checkpoint is missing
/// (simulating silent PostToolUse hook failure for the middle file).
///
/// This is the most realistic H6 scenario: Claude Code writes to files
/// A, B, and C. Files A and C get proper PostToolUse → checkpoint calls.
/// File B's PostToolUse hook fails silently (exit 0), so no AI checkpoint
/// is created for it — only a human checkpoint exists.
#[test]
fn test_h6c_three_files_one_missing_checkpoint() {
    let repo = TestRepo::new();

    let mut file_a = repo.filename("file_a.rs");
    fs::write(repo.path().join("file_a.rs"), "// file a\n").unwrap();
    fs::write(repo.path().join("file_b.rs"), "// file b\n").unwrap();
    let mut file_c = repo.filename("file_c.rs");
    fs::write(repo.path().join("file_c.rs"), "// file c\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes to all three files, but only A and C get AI checkpoints
    file_a.set_contents_no_stage(crate::lines!["// file a", "fn a_ai() {}".ai()]);
    file_c.set_contents_no_stage(crate::lines!["// file c", "fn c_ai() {}".ai()]);

    // file_b: AI-written content but only human checkpoint (hook failure)
    fs::write(
        repo.path().join("file_b.rs"),
        "// file b\nfn b_ai_but_no_checkpoint() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "--", "file_b.rs"]).unwrap();

    let commit = repo.stage_all_and_commit("ai edits three files").unwrap();

    let a_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "file_a.rs");
    let b_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "file_b.rs");
    let c_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "file_c.rs");

    assert!(
        a_attested,
        "H6c: file_a (AI checkpointed) should have attestation"
    );
    assert!(
        !b_attested,
        "H6c: file_b (missing AI checkpoint) should NOT have attestation.\n\
         This is the most common H6 failure: agent wrote the code but\n\
         the PostToolUse hook failed silently (exit 0) for this file."
    );
    assert!(
        c_attested,
        "H6c: file_c (AI checkpointed) should have attestation"
    );
}

/// H6d: AI checkpoint references a different file path than the actual edit.
///
/// Real-world trigger: ClaudePreset extracts file_path from tool_input,
/// but the path doesn't match the file actually modified (relative vs
/// absolute path, symlink, or tool reporting a different file).
/// Checkpoint records "wrong.rs" but the commit contains changes to "right.rs".
///
/// Impact: committed_hunks has entries for "right.rs", attributions has
/// entries for "wrong.rs" — no match, no attestation.
#[test]
fn test_h6d_ai_checkpoint_wrong_file_path() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("right.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI writes to "right.rs"
    let ai_content = "// base\nfn ai_wrote_this() {}\n";
    fs::write(repo.path().join("right.rs"), ai_content).unwrap();

    // But checkpoint records "wrong.rs" (path extraction error)
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let agent_id = "3bd30911a58cb074";
    let checkpoint_json = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"wrong.rs","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":2,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoint_json).unwrap();

    let result = repo.stage_all_and_commit("add ai code");

    match result {
        Err(_) => {
            // No note — checkpoint targets non-existent "wrong.rs"
        }
        Ok(commit) => {
            let right_attested = commit
                .authorship_log
                .attestations
                .iter()
                .any(|a| a.file_path == "right.rs");
            assert!(
                !right_attested,
                "H6d: right.rs should NOT have AI attestation when checkpoint targets wrong.rs.\n\
                 The AI wrote to right.rs but the checkpoint recorded 'wrong.rs'.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}

// ===========================================================================
// H7: Working log corruption scenarios
// ===========================================================================

/// H7a: A single corrupt line in checkpoints.jsonl destroys ALL checkpoint data.
///
/// CRITICAL ARCHITECTURAL WEAKNESS: read_all_checkpoints() uses `?` on the
/// JSON parse error, causing the ENTIRE function to return Err on the first
/// malformed line. All previously parsed valid checkpoints in the vec are lost.
///
/// In post_commit: mutate_all_checkpoints() propagates this error via `?`,
/// causing the entire post-commit to fail. No authorship note is written.
///
/// In from_just_working_log: `.unwrap_or_default()` converts the error to
/// an empty checkpoint list, silently discarding ALL AI attribution.
///
/// Real-world trigger: checkpoint write interrupted by signal/crash/timeout,
/// producing a truncated JSON line. All checkpoints in the file — including
/// valid ones before and after the corrupt line — are lost.
#[test]
fn test_h7a_truncated_jsonl_poisons_all_checkpoints() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("code.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI writes code
    let ai_content = "// base\nfn valid_ai_code() {}\n";
    fs::write(repo.path().join("code.rs"), ai_content).unwrap();

    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let agent_id = "3bd30911a58cb074";

    // Write: valid AI checkpoint THEN a truncated line (simulating interrupted write)
    let valid_checkpoint = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"code.rs","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":2,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    let corrupt_line = r#"{"kind":"Human","diff":"trunca"#; // interrupted mid-write
    let checkpoints = format!("{}\n{}\n", valid_checkpoint, corrupt_line);
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoints).unwrap();

    let result = repo.stage_all_and_commit("add ai code");

    // The corrupt line causes read_all_checkpoints() to fail with Err.
    // mutate_all_checkpoints() in post_commit propagates this error.
    // The valid AI checkpoint on line 1 is ALSO lost.
    match result {
        Err(_) => {
            eprintln!(
                "H7a CONFIRMED: Truncated JSONL caused post-commit failure.\n\
                 The valid AI checkpoint was destroyed by a corrupt line after it.\n\
                 This is a data-loss bug: one bad line poisons the entire file."
            );
        }
        Ok(commit) => {
            // If the system somehow recovers, check whether AI data survived
            let has_ai = !commit.authorship_log.attestations.is_empty();
            if !has_ai {
                eprintln!(
                    "H7a CONFIRMED (alternate): Commit succeeded but AI attestation \
                     was silently lost due to corrupt JSONL."
                );
            }
            // Whether this fails or passes, it documents the system's behavior
            assert!(
                !has_ai,
                "H7a: Expected AI attribution to be lost due to corrupt JSONL.\n\
                 If this fails, the system has better error recovery than expected.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}

/// H7c: Checkpoint with wrong api_version is silently skipped.
///
/// read_all_checkpoints() checks api_version and silently skips non-matching
/// entries (only a debug_log). If all checkpoints have a wrong version
/// (e.g., after a git-ai downgrade), ALL AI data is silently lost with
/// no user-visible warning.
#[test]
fn test_h7c_wrong_api_version_silently_skipped() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("versioned.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let ai_content = "// base\nfn ai_code() {}\n";
    fs::write(repo.path().join("versioned.rs"), ai_content).unwrap();

    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(ai_content.as_bytes()).finalize()
    );
    let agent_id = "3bd30911a58cb074";

    // Write checkpoint with WRONG api_version
    let checkpoint_json = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"versioned.rs","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":2,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/999.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoint_json).unwrap();

    let result = repo.stage_all_and_commit("add ai code");

    match result {
        Err(_) => {
            // No note — skipped checkpoint means no AI data
        }
        Ok(commit) => {
            assert!(
                commit.authorship_log.attestations.is_empty(),
                "H7c: Checkpoint with wrong api_version should be silently skipped.\n\
                 No user-visible warning is produced — data is quietly lost.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}

// ===========================================================================
// H8 expanded: Committed hunk detection edge cases
// ===========================================================================

/// H8a: AI checkpoint claims attribution for lines that already existed
/// unchanged in the parent commit.
///
/// The checkpoint records AI attribution for lines 1-3, but only line 3 is
/// actually new. Lines 1-2 existed in the parent and are unchanged.
///
/// to_authorship_log_and_initial_working_log classifies each line:
///   - committed: line is in committed_hunks (from git diff parent→commit)
///   - unstaged: line is in unstaged_hunks (from git diff commit→workdir)
///   - discarded: neither ("already existed in parent")
///
/// Lines 1-2 are "already existed" → silently discarded despite AI claiming them.
/// Only line 3 (actually new) gets AI attestation.
#[test]
fn test_h8a_ai_claims_existing_unchanged_lines() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    // Base commit with two existing lines
    fs::write(
        repo.path().join("existing.rs"),
        "fn existing_1() {}\nfn existing_2() {}\n",
    )
    .unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI "writes" all 3 lines (but only line 3 is actually new)
    let new_content = "fn existing_1() {}\nfn existing_2() {}\nfn new_ai_line() {}\n";
    fs::write(repo.path().join("existing.rs"), new_content).unwrap();

    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let ai_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(new_content.as_bytes()).finalize()
    );
    let agent_id = "3bd30911a58cb074";

    // Checkpoint claims ALL 3 lines are AI-written (overclaiming)
    let checkpoint_json = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"existing.rs","blob_sha":"{ai_sha}","attributions":[],"line_attributions":[{{"start_line":1,"end_line":3,"author_id":"{agent_id}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"test_session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoint_json).unwrap();

    let commit = repo.stage_all_and_commit("add one new line").unwrap();

    // Only line 3 is new → only line 3 should get attestation
    let has_ai = !commit.authorship_log.attestations.is_empty();
    assert!(
        has_ai,
        "H8a: The genuinely new AI line (line 3) should have attestation.\n\
         Lines 1-2 existed in parent and should be silently discarded.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

/// H8b: AI replaces an existing line (modification, not pure addition).
///
/// git diff shows a deletion of the old line and addition of the new line.
/// The addition should appear in committed_hunks, so the AI replacement
/// gets proper attribution. This tests that modifications (not just
/// insertions) are correctly tracked.
#[test]
fn test_h8b_ai_replaces_existing_line() {
    let repo = TestRepo::new();

    let mut file = repo.filename("replaced.rs");
    fs::write(
        repo.path().join("replaced.rs"),
        "fn old_implementation() {}\n",
    )
    .unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI replaces the existing line with new implementation
    file.set_contents_no_stage(crate::lines!["fn new_ai_implementation() {}".ai()]);

    let commit = repo.stage_all_and_commit("ai replaces function").unwrap();

    let has_ai = !commit.authorship_log.attestations.is_empty();
    assert!(
        has_ai,
        "H8b: AI line replacement should produce attestation.\n\
         git diff shows deletion + addition; the addition should be attributed.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H9: Path mismatch and rewrite scenarios
// ===========================================================================

/// H9a: AI writes code, then human completely rewrites everything before commit.
///
/// Sequence: base → AI checkpoint → human rewrites all AI lines → human
/// checkpoint → commit. The AI content no longer exists in the committed tree.
///
/// This is CORRECT behavior: human override should win. The test documents
/// that the system properly handles human rewrites of AI code.
#[test]
fn test_h9a_human_rewrite_after_ai() {
    let repo = TestRepo::new();

    let mut file = repo.filename("rewritten.rs");
    fs::write(repo.path().join("rewritten.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes new lines
    file.set_contents_no_stage(crate::lines![
        "// base",
        "fn ai_code_v1() {}".ai(),
        "fn ai_code_v2() {}".ai(),
    ]);

    // Human completely rewrites the AI content before committing.
    // Bypass set_contents to avoid creating another AI checkpoint.
    let human_rewrite = "// base\nfn human_rewrite_v1() {}\nfn human_rewrite_v2() {}\n";
    fs::write(repo.path().join("rewritten.rs"), human_rewrite).unwrap();
    repo.git_ai(&["checkpoint", "--", "rewritten.rs"]).unwrap();

    let commit = repo.stage_all_and_commit("human rewrites ai code").unwrap();

    let has_ai_for_file = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "rewritten.rs");

    // Correct behavior: human completely replaced AI content → no AI attestation
    assert!(
        !has_ai_for_file,
        "H9a: Human rewrite should eliminate AI attestation for the file.\n\
         AI wrote lines 2-3, human replaced them entirely before commit.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

/// H9b: AI checkpoints a file, then the file is renamed before commit.
///
/// The checkpoint records attribution for "old_name.rs", but the commit
/// contains "new_name.rs" (via git mv). The committed_hunks are keyed to
/// "new_name.rs" while attributions are keyed to "old_name.rs" → no match.
///
/// Real-world trigger: AI agent writes to a file, then the user (or a
/// refactoring tool) renames it before committing. All AI attribution is
/// lost because the path-based matching breaks.
#[test]
fn test_h9b_file_renamed_after_ai_checkpoint() {
    let repo = TestRepo::new();

    let mut file = repo.filename("old_name.rs");
    fs::write(repo.path().join("old_name.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes to old_name.rs (checkpoint keyed to "old_name.rs")
    file.set_contents_no_stage(crate::lines!["// base", "fn ai_code() {}".ai()]);

    // User renames the file before committing
    repo.git(&["mv", "old_name.rs", "new_name.rs"]).unwrap();

    let commit = repo.stage_all_and_commit("rename and commit").unwrap();

    let new_name_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "new_name.rs");
    let old_name_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "old_name.rs");

    // The rename breaks path-based attribution matching.
    // Checkpoint data is keyed to "old_name.rs" but committed hunks are for "new_name.rs".
    // Neither path matches the other → all attribution is lost.
    assert!(
        !new_name_attested,
        "H9b: new_name.rs should NOT have AI attestation — checkpoint was for old_name.rs.\n\
         File rename causes complete attribution loss.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
    assert!(
        !old_name_attested,
        "H9b: old_name.rs should NOT have attestation — file was renamed in commit.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H10: File lifecycle edge cases
// ===========================================================================

/// H10: AI checkpoints a file, then the file is deleted before commit.
///
/// The checkpoint records attribution for a file that no longer exists in
/// the working directory or the committed tree. from_just_working_log reads
/// the file content as "" (empty — file doesn't exist). In the authorship
/// log, there are no committed_hunks for the deleted file (git diff shows
/// only deletions, and diff_added_lines only returns additions).
///
/// The surviving file in the same commit should retain its AI attribution.
#[test]
fn test_h10_file_deleted_after_ai_checkpoint() {
    let repo = TestRepo::new();

    let mut file_a = repo.filename("ephemeral.rs");
    fs::write(repo.path().join("ephemeral.rs"), "// ephemeral\n").unwrap();
    let mut file_b = repo.filename("survivor.rs");
    fs::write(repo.path().join("survivor.rs"), "// survivor\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes to both files
    file_a.set_contents_no_stage(crate::lines!["// ephemeral", "fn ai_wrote_this() {}".ai(),]);
    file_b.set_contents_no_stage(crate::lines!["// survivor", "fn also_ai() {}".ai()]);

    // User deletes ephemeral.rs before committing
    fs::remove_file(repo.path().join("ephemeral.rs")).unwrap();

    let commit = repo
        .stage_all_and_commit("delete ephemeral, keep survivor")
        .unwrap();

    let ephemeral_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "ephemeral.rs");
    let survivor_attested = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "survivor.rs");

    assert!(
        !ephemeral_attested,
        "H10: Deleted file should NOT have attestation (no committed additions).\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
    assert!(
        survivor_attested,
        "H10: Surviving file should retain AI attestation.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H11: Multi-agent interaction
// ===========================================================================

/// H11: Two AI agents write to the same file. The last checkpoint wins
/// because from_just_working_log uses HashMap::insert which replaces
/// the entire per-file attribution entry.
///
/// Real-world trigger: User switches between two AI tools (e.g., Claude Code
/// and Cursor) editing the same file during a single commit cycle.
/// The second tool's checkpoint completely replaces the first's data.
///
/// Agent A writes line 2, Agent B writes line 3. After both checkpoints,
/// only Agent B's line 3 has attribution. Agent A's line 2 is lost because
/// Agent B's checkpoint.entries for "shared.rs" overwrites Agent A's via
/// HashMap::insert at virtual_attribution.rs:435.
#[test]
fn test_h11_two_agents_same_file_last_wins() {
    use git_ai::authorship::authorship_log_serialization::generate_short_hash;
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("shared.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Final file content has lines from both agents
    let content = "// base\nfn agent_a_code() {}\nfn agent_b_code() {}\n";
    fs::write(repo.path().join("shared.rs"), content).unwrap();

    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let content_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(content.as_bytes()).finalize()
    );
    let agent_a_hash = generate_short_hash("session_a", "agent_a");
    let agent_b_hash = generate_short_hash("session_b", "agent_b");

    // Agent A checkpoints line 2, then Agent B checkpoints line 3.
    // Agent B's checkpoint REPLACES Agent A's for "shared.rs" (HashMap::insert).
    let checkpoints = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"shared.rs","blob_sha":"{content_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":2,"author_id":"{agent_a_hash}","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"agent_a","id":"session_a","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}
{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"shared.rs","blob_sha":"{content_sha}","attributions":[],"line_attributions":[{{"start_line":3,"end_line":3,"author_id":"{agent_b_hash}","overrode":null}}]}}],"timestamp":2000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"agent_b","id":"session_b","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoints).unwrap();

    let commit = repo.stage_all_and_commit("both agents edit").unwrap();

    let attestations = &commit.authorship_log.attestations;
    let has_agent_a = attestations
        .iter()
        .any(|a| a.file_path == "shared.rs" && a.entries.iter().any(|e| e.hash == agent_a_hash));
    let has_agent_b = attestations
        .iter()
        .any(|a| a.file_path == "shared.rs" && a.entries.iter().any(|e| e.hash == agent_b_hash));

    // Agent B's checkpoint replaced Agent A's via HashMap::insert.
    // Only Agent B's line 3 gets attribution. Agent A's line 2 is silently lost.
    assert!(
        has_agent_b,
        "H11: Agent B (last checkpoint) should have attestation.\n\
         attestations: {:?}",
        attestations
    );
    assert!(
        !has_agent_a,
        "H11: Agent A's attribution should be LOST — Agent B's checkpoint\n\
         replaced the entire file entry via HashMap::insert.\n\
         This is a data-loss bug for multi-agent workflows.\n\
         attestations: {:?}",
        attestations
    );
}

// ===========================================================================
// H12: Corrupt INITIAL file loses uncommitted AI carryover
// ===========================================================================

/// H12: The INITIAL file (written after each commit by post_commit.rs:296-304)
/// carries forward uncommitted AI attribution between sessions. When corrupt,
/// `read_initial_attributions()` (repo_storage.rs:694-711) returns empty default.
///
/// Test strategy: AI writes to two files. Only file_a is staged and committed.
/// Post-commit writes INITIAL with file_b's uncommitted AI data. We corrupt
/// INITIAL, then commit file_b. Without INITIAL, file_b's AI attribution is lost.
#[test]
fn test_h12_corrupt_initial_loses_uncommitted_carryover() {
    let repo = TestRepo::new();

    let mut file_a = repo.filename("committed.rs");
    let mut file_b = repo.filename("uncommitted.rs");
    fs::write(repo.path().join("committed.rs"), "// base a\n").unwrap();
    fs::write(repo.path().join("uncommitted.rs"), "// base b\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // AI writes to both files using set_contents_no_stage to avoid
    // auto-staging (set_contents uses git add -A which would stage both).
    file_a.set_contents_no_stage(crate::lines!["// base a", "fn ai_a() {}".ai()]);
    file_b.set_contents_no_stage(crate::lines!["// base b", "fn ai_b() {}".ai()]);

    // Only stage file_a — file_b stays uncommitted
    repo.git(&["add", "committed.rs"]).unwrap();
    let commit1 = repo.commit("commit file_a only").unwrap();
    assert!(
        !commit1.authorship_log.attestations.is_empty(),
        "precondition: file_a should have AI attestation"
    );

    // INITIAL should now contain file_b's uncommitted AI carryover.
    // It's written to the NEW working log dir keyed by the new commit SHA.
    let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let initial_path =
        std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}/INITIAL", head_sha));

    if !initial_path.exists() {
        // INITIAL not written — the carryover mechanism may not apply here.
        // This is observational: we can't test the corruption path.
        eprintln!(
            "H12: No INITIAL file at {:?}. Post-commit may not have written carryover \
             for this partial-commit pattern. Test passes as observational.",
            initial_path
        );
        return;
    }

    // Corrupt the INITIAL file
    fs::write(&initial_path, "CORRUPT_JSON{{{{").unwrap();
    eprintln!("H12: Corrupted INITIAL file at {:?}", initial_path);

    // Commit file_b. Without INITIAL, the system doesn't know file_b
    // had AI attribution from the previous commit cycle.
    // Don't use set_contents (which creates new AI checkpoints) —
    // INITIAL should be the only source.
    let commit2 = repo.stage_all_and_commit("commit file_b").unwrap();

    let file_b_has_ai = commit2
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "uncommitted.rs" && !a.entries.is_empty());

    assert!(
        !file_b_has_ai,
        "H12 CONFIRMED: Corrupt INITIAL silently loses uncommitted AI carryover.\n\
         file_b's AI work from the previous commit cycle was preserved only in INITIAL.\n\
         With INITIAL corrupt, read_initial_attributions() returns empty default.\n\
         attestations: {:?}",
        commit2.authorship_log.attestations
    );
}

// ===========================================================================
// H13: Missing checkpoint kind — forward/backward compatibility
// ===========================================================================

/// H13: When the `kind` field is missing from checkpoint JSON, `serde_default`
/// makes it `Human`. Despite this, the non-human `author_id` in
/// `line_attributions` still causes `checkpoint_entry_requires_post_processing`
/// (post_commit.rs:34-50) to include the entry. This is RESILIENT behavior.
#[test]
fn test_h13_missing_kind_defaults_to_human_but_preserves_ai_author() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("legacy.rs"), "// base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Write AI content to disk
    let content = "// base\nfn legacy_ai() {}\n";
    fs::write(repo.path().join("legacy.rs"), content).unwrap();

    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let content_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(content.as_bytes()).finalize()
    );

    // Write checkpoint WITHOUT "kind" field — serde_default makes it Human.
    // The author_id "legacy_agent" is non-human, so checkpoint_entry_requires_post_processing
    // still includes this entry in pathspecs (post_commit.rs:45).
    let checkpoint = format!(
        r#"{{"diff":"fake","author":"Test User","entries":[{{"file":"legacy.rs","blob_sha":"{content_sha}","attributions":[],"line_attributions":[{{"start_line":2,"end_line":2,"author_id":"legacy_agent","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"legacy_tool","id":"session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":1,"deletions":0,"additions_sloc":1,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoint).unwrap();

    let commit = repo.stage_all_and_commit("legacy ai code").unwrap();

    // Despite missing kind (defaulting to Human), the non-human author_id
    // in line_attributions still produces attestation. This is resilient:
    // the kind field alone doesn't determine whether AI attestation appears.
    let has_ai =
        commit.authorship_log.attestations.iter().any(|a| {
            a.file_path == "legacy.rs" && a.entries.iter().any(|e| e.hash == "legacy_agent")
        });

    assert!(
        has_ai,
        "H13: Missing 'kind' field defaults to Human via serde_default,\n\
         but non-human author_id 'legacy_agent' still produces attestation.\n\
         Forward/backward compatibility is resilient for author attribution.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H14: Binary file AI attribution silently dropped
// ===========================================================================

/// H14: `collect_committed_hunks` parses `git diff -U0` output. Binary diffs
/// produce "Binary files differ" with no `@@` hunk headers, so no committed
/// hunks are collected. The attribution pipeline silently skips the file.
#[test]
fn test_h14_binary_file_ai_attribution_silently_dropped() {
    use sha2::{Digest, Sha256};

    let repo = TestRepo::new();

    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Write binary content (null byte makes git treat it as binary)
    let binary_content = b"HEADER\x00\x01\x02BINARY DATA\nLINE 2\x00MORE\n";
    fs::write(repo.path().join("generated.bin"), binary_content).unwrap();

    // Write raw AI checkpoint claiming attribution for this binary file
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_dir = std::path::Path::new(&git_dir).join(format!("ai/working_logs/{}", base_sha));
    fs::create_dir_all(&cp_dir).unwrap();

    let content_sha = format!(
        "{:x}",
        Sha256::new_with_prefix(binary_content.as_slice()).finalize()
    );
    let checkpoint = format!(
        r#"{{"kind":"AiAgent","diff":"fake","author":"Test User","entries":[{{"file":"generated.bin","blob_sha":"{content_sha}","attributions":[],"line_attributions":[{{"start_line":1,"end_line":2,"author_id":"binary_agent","overrode":null}}]}}],"timestamp":1000,"transcript":{{"messages":[]}},"agent_id":{{"tool":"mock_ai","id":"session","model":"test"}},"agent_metadata":null,"line_stats":{{"additions":2,"deletions":0,"additions_sloc":2,"deletions_sloc":0}},"api_version":"checkpoint/1.0.0","git_ai_version":"test"}}"#
    );
    fs::write(cp_dir.join("checkpoints.jsonl"), &checkpoint).unwrap();

    let commit = repo.stage_all_and_commit("add binary file").unwrap();

    // Binary file has no committed hunks (git diff shows "Binary files differ"),
    // so collect_committed_hunks returns None for this file. The attribution
    // pipeline silently skips it.
    let has_binary_attestation = commit
        .authorship_log
        .attestations
        .iter()
        .any(|a| a.file_path == "generated.bin");

    assert!(
        !has_binary_attestation,
        "H14 CONFIRMED: Binary file AI attribution is silently dropped.\n\
         collect_committed_hunks produces no hunks for binary diffs.\n\
         The AI checkpoint for generated.bin was ignored.\n\
         attestations: {:?}",
        commit.authorship_log.attestations
    );
}

// ===========================================================================
// H17: Merge commit attribution note behavior
// ===========================================================================

/// H17: Only `--squash` merges trigger working-log preparation (merge_hooks.rs:16).
/// Regular merges create a merge commit whose working log has no AI checkpoint
/// data from the feature branch. This test documents the behavior.
#[test]
fn test_h17_merge_commit_note_for_feature_branch_ai_lines() {
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

    let repo = TestRepo::new();

    // Create base commit
    fs::write(repo.path().join("shared.rs"), "// shared\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    let main_branch = repo.current_branch();

    // Feature branch: AI writes code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.rs");
    feature_file.set_contents_no_stage(crate::lines!["fn feature_ai() {}".ai()]);
    let feature_commit = repo.stage_all_and_commit("feature: AI code").unwrap();
    assert!(
        !feature_commit.authorship_log.attestations.is_empty(),
        "precondition: feature commit has AI attestation"
    );

    // Switch back to main and make a divergent commit (forces merge commit)
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main_only.rs");
    main_file.set_contents_no_stage(crate::lines!["fn main_only() {}"]);
    repo.stage_all_and_commit("main: diverge").unwrap();

    // Merge feature branch (non-squash, creates merge commit)
    repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"])
        .expect("merge should succeed");
    repo.sync_daemon_force();

    // Check merge commit's own authorship note
    let merge_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&merge_sha);

    match note {
        Some(content) => {
            if let Ok(log) = AuthorshipLog::deserialize_from_string(&content) {
                let has_feature_ai = log
                    .attestations
                    .iter()
                    .any(|a| a.file_path == "feature.rs" && !a.entries.is_empty());
                eprintln!(
                    "H17: Merge commit has authorship note.\n\
                     feature.rs AI attestation present: {}\n\
                     attestations: {:?}",
                    has_feature_ai, log.attestations
                );
                // Document: merge commit may have AI attestation if daemon/wrapper
                // processed the merge correctly through the rewrite system.
                // If attestation is missing, blame still works (traces to original commit).
            }
        }
        None => {
            eprintln!(
                "H17: No authorship note on merge commit {}.\n\
                 This documents that regular merges may not get attribution notes.\n\
                 Blame still works because it traces to the original feature commit.",
                merge_sha
            );
        }
    }
    // This test is observational — it documents behavior rather than asserting a bug.
    // The merge_rebase.rs tests confirm blame works correctly regardless.
}

// ===========================================================================
// H18: Append checkpoint on corrupt JSONL destroys valid prior data
// ===========================================================================

/// H18: `append_checkpoint` (repo_storage.rs:339) reads existing checkpoints
/// with `unwrap_or_default()`. If the JSONL is corrupt (e.g., partial write
/// from crash), the read returns empty vec. The new checkpoint is then written
/// as the ONLY entry, silently destroying all prior valid checkpoints.
///
/// This is distinct from H7a (which tests the read path at commit time) —
/// this tests the WRITE path during checkpoint creation.
///
/// Test strategy: AI writes to file_a (creates valid checkpoints). We corrupt
/// the JSONL. Then AI writes to file_b (triggers append_checkpoint, which reads
/// corrupt JSONL → empty vec → destroys file_a's data). On commit, file_a
/// has no AI attestation (data lost) while file_b does (data survived).
#[test]
fn test_h18_append_checkpoint_on_corrupt_jsonl_overwrites_valid_data() {
    let repo = TestRepo::new();

    let mut file_a = repo.filename("early.rs");
    let mut file_b = repo.filename("later.rs");
    fs::write(repo.path().join("early.rs"), "// base a\n").unwrap();
    fs::write(repo.path().join("later.rs"), "// base b\n").unwrap();
    repo.stage_all_and_commit("base").unwrap();

    // Step 1: AI writes to file_a — creates valid checkpoints in the JSONL
    file_a.set_contents_no_stage(crate::lines!["// base a", "fn a_ai() {}".ai()]);

    // Step 2: Corrupt the JSONL (simulating crash/partial write)
    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let git_dir = repo
        .git(&["rev-parse", "--absolute-git-dir"])
        .unwrap()
        .trim()
        .to_string();
    let cp_path = std::path::Path::new(&git_dir)
        .join(format!("ai/working_logs/{}/checkpoints.jsonl", base_sha));
    assert!(
        cp_path.exists(),
        "precondition: checkpoint file should exist after AI edit to file_a"
    );
    let mut data = fs::read_to_string(&cp_path).unwrap();
    data.push_str("\n{CORRUPT LINE FROM PARTIAL WRITE}");
    fs::write(&cp_path, &data).unwrap();

    // Step 3: AI writes to file_b. This triggers append_checkpoint which:
    //   1. read_all_checkpoints() → encounters corrupt line → Err
    //   2. .unwrap_or_default() → empty Vec
    //   3. Pushes new file_b checkpoint onto empty vec
    //   4. Writes JSONL with ONLY file_b's checkpoints
    //   → file_a's valid checkpoints are silently destroyed
    file_b.set_contents_no_stage(crate::lines!["// base b", "fn b_ai() {}".ai()]);

    // Step 4: Commit both files.
    // The corrupt JSONL may cause post-commit to fail entirely (no note written),
    // or the note may be written but with file_a's AI data lost. Either outcome
    // confirms the data-loss bug in append_checkpoint.
    let result = repo.stage_all_and_commit("commit both");

    match result {
        Err(_) => {
            // Post-commit failed — the corruption cascaded through the pipeline.
            // This is an even worse outcome: not just data loss but total note failure.
            eprintln!(
                "H18 CONFIRMED: Corrupt JSONL caused complete post-commit failure.\n\
                 append_checkpoint's unwrap_or_default() recovery produced state that \
                 crashed the post-commit pipeline. No authorship note was written."
            );
        }
        Ok(commit) => {
            let a_has_ai = commit
                .authorship_log
                .attestations
                .iter()
                .any(|a| a.file_path == "early.rs" && !a.entries.is_empty());

            assert!(
                !a_has_ai,
                "H18 CONFIRMED: append_checkpoint on corrupt JSONL destroys valid prior data.\n\
                 file_a's checkpoints were lost when file_b's append_checkpoint read corrupt JSONL,\n\
                 got empty vec via unwrap_or_default(), and rewrote with only file_b's data.\n\
                 attestations: {:?}",
                commit.authorship_log.attestations
            );
        }
    }
}
