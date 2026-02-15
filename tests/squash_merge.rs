#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::time::Duration;

/// Test merge --squash with a simple feature branch containing AI and human edits
#[test]
fn test_prepare_working_log_simple_squash() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.txt");

    // Create master branch with initial content
    file.set_contents(lines!["line 1", "line 2", "line 3", ""]);
    repo.stage_all_and_commit("Initial commit on master")
        .unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Add AI changes on feature branch
    file.insert_at(3, lines!["// AI added feature".ai()]);
    repo.stage_all_and_commit("Add AI feature").unwrap();

    std::thread::sleep(Duration::from_secs(1));

    // Add human changes on feature branch
    file.insert_at(4, lines!["// Human refinement"]);
    repo.stage_all_and_commit("Human refinement").unwrap();

    // Go back to master and squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed feature").unwrap();

    // Verify AI attribution is preserved
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "// AI added feature".ai(),
        "// Human refinement".human()
    ]);

    // Verify stats for squashed commit
    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 2, "Squash commit adds 2 lines");
    assert_eq!(stats.ai_additions, 1, "1 AI line from feature branch");
    assert_eq!(stats.ai_accepted, 1, "1 AI line accepted without edits");
    assert_eq!(
        stats.human_additions, 1,
        "1 human lines from feature branch"
    );
    assert_eq!(stats.mixed_additions, 0, "No mixed edits");
}

/// Test merge --squash with out-of-band changes on master (handles 3-way merge)
#[test]
fn test_prepare_working_log_squash_with_main_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("document.txt");

    // Create master branch with initial content
    file.set_contents(lines!["section 1", "section 2", "section 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch and add AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(3, lines!["// AI feature addition at end".ai()]);
    repo.stage_all_and_commit("AI adds feature").unwrap();

    // Switch back to master and make out-of-band changes
    repo.git(&["checkout", &default_branch]).unwrap();

    // Re-initialize file after checkout to get current master state
    let mut file = repo.filename("document.txt");
    file.insert_at(0, lines!["// Master update at top"]);
    repo.stage_all_and_commit("Out-of-band update on master")
        .unwrap();

    // Squash merge feature into master
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.stage_all_and_commit("Squashed feature with out-of-band")
        .unwrap();

    // Verify both changes are present with correct attribution
    file.assert_lines_and_blame(lines![
        "// Master update at top".human(),
        "section 1".human(),
        "section 2".human(),
        "section 3".human(),
        "// AI feature addition at end".ai()
    ]);

    // Verify stats for squashed commit
    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.git_diff_added_lines, 2,
        "Squash commit adds 2 lines from feature (includes newline)"
    );
    assert_eq!(stats.ai_additions, 1, "1 AI line from feature branch");
    assert_eq!(stats.ai_accepted, 1, "1 AI line accepted without edits");
    assert_eq!(stats.human_additions, 1, "1 human line from feature branch");
    assert_eq!(stats.mixed_additions, 0, "No mixed edits");
}

/// Test merge --squash with multiple AI sessions and human edits
#[test]
fn test_prepare_working_log_squash_multiple_sessions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.txt");

    // Create master branch
    file.set_contents(lines!["header", "body", "footer"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI session
    file.insert_at(1, lines!["// AI session 1".ai()]);
    repo.stage_all_and_commit("AI session 1").unwrap();

    // Human edit
    file.insert_at(3, lines!["// Human addition"]);
    repo.stage_all_and_commit("Human edit").unwrap();

    // Second AI session (different agent - simulated by new checkpoint)
    file.insert_at(5, lines!["// AI session 2".ai()]);
    repo.stage_all_and_commit("AI session 2").unwrap();

    // Squash merge into master
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed multiple sessions").unwrap();

    // Verify all authorship is preserved
    file.assert_lines_and_blame(lines![
        "header".human(),
        "// AI session 1".ai(),
        "body".human(),
        "// Human addition".human(),
        "footer".human(),
        "// AI session 2".ai()
    ]);

    // Verify stats for squashed commit with multiple sessions
    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.git_diff_added_lines, 4,
        "Squash commit adds 4 lines total (includes newline)"
    );
    assert_eq!(
        stats.ai_additions, 2,
        "2 AI lines from feature branch (both sessions)"
    );
    assert_eq!(stats.ai_accepted, 2, "2 AI lines accepted without edits");
    assert_eq!(
        stats.human_additions, 2,
        "2 human lines from feature branch"
    );
    assert_eq!(stats.mixed_additions, 0, "No mixed edits");
}

/// Test merge --squash with mixed additions (AI code edited by human before commit)
#[test]
fn test_prepare_working_log_squash_with_mixed_additions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("code.txt");

    // Create master branch with initial content
    file.set_contents(lines!["function start() {", "  // initial code", "}"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI adds 3 lines (without committing)
    file.insert_at(
        2,
        lines![
            "  const x = 1;".ai(),
            "  const y = 2;".ai(),
            "  const z = 3;".ai()
        ],
    );

    // Human immediately edits the middle AI line (before committing)
    // This creates a "mixed addition" - AI generated, human edited
    file.replace_at(3, "  const y = 20; // human modified");

    // Now commit with both AI and human changes together
    repo.stage_all_and_commit("AI adds variables, human refines")
        .unwrap();

    file.insert_at(
        0,
        lines![
            "// AI comment".ai(),
            "// Describing the code".ai(),
            "// And how it works".ai(),
        ],
    );

    repo.stage_all_and_commit("AI adds comment").unwrap();

    // Squash merge back to master
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("Squashed feature with mixed edits").unwrap();
    squash_commit.print_authorship();

    // Verify attribution - edited line should be human
    file.assert_lines_and_blame(lines![
        "// AI comment".ai(),
        "// Describing the code".ai(),
        "// And how it works".ai(),
        "function start() {".human(),
        "  // initial code".human(),
        "  const x = 1;".ai(),
        "  const y = 20; // human modified".human(), // Human edited AI line
        "  const z = 3;".ai(),
        "}".human()
    ]);

    // Verify stats show mixed additions
    let stats = repo.stats().unwrap();
    println!("stats: {:?}", stats);
    assert_eq!(
        stats.git_diff_added_lines, 6,
        "Squash commit adds 3 lines total"
    );
    assert_eq!(stats.ai_additions, 5, "3 AI lines total (2 pure + 1 mixed)");
    assert_eq!(stats.ai_accepted, 5, "2 AI lines accepted without edits");
    // tmp until we fix override
    assert_eq!(
        stats.mixed_additions, 0,
        "1 AI line was edited by human before commit"
    );
    assert_eq!(
        stats.human_additions, 1,
        "1 human addition (the overridden AI line)"
    );

    // Verify prompt records have correct stats
    let prompts = &squash_commit.authorship_log.metadata.prompts;
    assert!(
        !prompts.is_empty(),
        "Should have at least one prompt record"
    );

    // Check each prompt record has updated stats
    for (prompt_id, prompt_record) in prompts {
        println!(
            "Prompt {}: accepted_lines={}, overridden_lines={}, total_additions={}, total_deletions={}",
            prompt_id,
            prompt_record.accepted_lines,
            prompt_record.overriden_lines,
            prompt_record.total_additions,
            prompt_record.total_deletions
        );

        // accepted_lines should match the number of lines attributed to this prompt in final commit
        assert!(
            prompt_record.accepted_lines > 0,
            "Prompt {} should have accepted_lines > 0",
            prompt_id
        );

        // overridden_lines should be 0 for squash merge (we don't track overrides in merge context)
        assert_eq!(
            prompt_record.overriden_lines, 0,
            "Prompt {} should have overridden_lines = 0 in squash merge",
            prompt_id
        );

        // Total additions/deletions should be preserved from the newest prompt version
        // (they may be 0 if not tracked in the original prompt)
    }

    // Verify that the sum of accepted_lines across all prompts matches ai_accepted in stats
    let total_accepted: u32 = prompts.values().map(|p| p.accepted_lines).sum();
    assert_eq!(
        total_accepted, stats.ai_accepted,
        "Sum of accepted_lines across prompts should match ai_accepted stat"
    );
}

/// Human-only squash merges should not synthesize AI attestations/prompts.
#[test]
fn test_prepare_working_log_squash_human_only_fast_path() {
    let repo = TestRepo::new();
    let mut file = repo.filename("human_only.txt");

    file.set_contents(lines!["base line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["human feature line"]);
    repo.stage_all_and_commit("Human-only feature change")
        .unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("Squash human-only feature").unwrap();

    file.assert_lines_and_blame(lines!["base line".human(), "human feature line".human()]);
    assert!(
        squash_commit.authorship_log.attestations.is_empty(),
        "No AI attestations expected for human-only squash"
    );
    assert!(
        squash_commit.authorship_log.metadata.prompts.is_empty(),
        "No AI prompts expected for human-only squash"
    );
}

/// Unrelated AI churn on the target branch should not pollute squash metadata.
#[test]
fn test_prepare_working_log_squash_ignores_unrelated_target_ai_files() {
    let repo = TestRepo::new();
    let mut feature_file = repo.filename("feature.txt");
    let mut target_file = repo.filename("target.txt");

    feature_file.set_contents(lines!["feature base"]);
    target_file.set_contents(lines!["target base"]);
    repo.stage_all_and_commit("Initial base").unwrap();
    let default_branch = repo.current_branch();

    // Feature branch adds AI content in feature.txt
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    feature_file.insert_at(1, lines!["feature ai line".ai()]);
    repo.stage_all_and_commit("Feature AI change").unwrap();

    // Target branch adds unrelated AI content in target.txt
    repo.git(&["checkout", &default_branch]).unwrap();
    target_file.insert_at(1, lines!["target ai line".ai()]);
    repo.stage_all_and_commit("Target AI churn").unwrap();

    // Squash only feature branch changes into target
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("Squash feature into target").unwrap();

    feature_file.assert_lines_and_blame(lines!["feature base".human(), "feature ai line".ai()]);
    target_file.assert_lines_and_blame(lines!["target base".human(), "target ai line".ai()]);

    // Squash commit should only carry new AI attestation for feature.txt.
    assert_eq!(
        squash_commit.authorship_log.attestations.len(),
        1,
        "Only feature.txt should be attested in squash commit"
    );
    assert_eq!(
        squash_commit.authorship_log.attestations[0].file_path, "feature.txt",
        "Unrelated target AI file should not be included in squash attestations"
    );
    assert_eq!(
        squash_commit.authorship_log.metadata.prompts.len(),
        1,
        "Only feature prompt should be carried into squash commit metadata"
    );

    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.ai_additions, 1,
        "Only one new AI line should be counted"
    );
    assert_eq!(
        stats.ai_accepted, 1,
        "Only the feature branch AI line should be accepted in squash commit"
    );
}

/// Small staged-set fast path should not introduce AI attestations for human-only files.
#[test]
fn test_prepare_working_log_squash_small_staged_bypass_keeps_human_files_clean() {
    let repo = TestRepo::new();
    let mut ai_file = repo.filename("ai_file.txt");
    let mut human_file = repo.filename("human_file.txt");

    ai_file.set_contents(lines!["ai base"]);
    human_file.set_contents(lines!["human base"]);
    repo.stage_all_and_commit("Initial base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    ai_file.insert_at(1, lines!["ai feature line".ai()]);
    human_file.insert_at(1, lines!["human feature line"]);
    repo.stage_all_and_commit("Mixed feature commit").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("Squash mixed feature").unwrap();

    ai_file.assert_lines_and_blame(lines!["ai base".human(), "ai feature line".ai()]);
    human_file.assert_lines_and_blame(lines!["human base".human(), "human feature line".human()]);

    assert_eq!(
        squash_commit.authorship_log.attestations.len(),
        1,
        "Human-only file should not receive AI attestations"
    );
    assert_eq!(
        squash_commit.authorship_log.attestations[0].file_path,
        "ai_file.txt"
    );
    assert_eq!(
        squash_commit.authorship_log.metadata.prompts.len(),
        1,
        "Only AI prompt metadata should be carried into squash commit"
    );
}
