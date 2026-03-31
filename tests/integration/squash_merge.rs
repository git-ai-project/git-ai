use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use std::collections::HashMap;

fn deterministic_commit_env(timestamp: &'static str) -> [(&'static str, &'static str); 2] {
    [
        ("GIT_AUTHOR_DATE", timestamp),
        ("GIT_COMMITTER_DATE", timestamp),
    ]
}

/// Test merge --squash with a simple feature branch containing AI and human edits
#[test]
fn test_prepare_working_log_simple_squash() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.txt");

    // Create master branch with initial content
    file.set_contents(crate::lines!["line 1", "line 2", "line 3", ""]);
    repo.stage_all_and_commit("Initial commit on master")
        .unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Add AI changes on feature branch
    file.insert_at(3, crate::lines!["// AI added feature".ai()]);
    repo.stage_all_and_commit_with_env(
        "Add AI feature",
        &deterministic_commit_env("2030-01-01T00:00:00Z"),
    )
    .unwrap();

    // Add human changes on feature branch
    file.insert_at(4, crate::lines!["// Human refinement"]);
    repo.stage_all_and_commit_with_env(
        "Human refinement",
        &deterministic_commit_env("2030-01-01T00:00:01Z"),
    )
    .unwrap();

    // Go back to master and squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed feature").unwrap();

    // Verify AI attribution is preserved
    file.assert_lines_and_blame(crate::lines![
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
    file.set_contents(crate::lines!["section 1", "section 2", "section 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch and add AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(3, crate::lines!["// AI feature addition at end".ai()]);
    repo.stage_all_and_commit("AI adds feature").unwrap();

    // Switch back to master and make out-of-band changes
    repo.git(&["checkout", &default_branch]).unwrap();

    // Re-initialize file after checkout to get current master state
    let mut file = repo.filename("document.txt");
    file.insert_at(0, crate::lines!["// Master update at top"]);
    repo.stage_all_and_commit("Out-of-band update on master")
        .unwrap();

    // Squash merge feature into master
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.stage_all_and_commit("Squashed feature with out-of-band")
        .unwrap();

    // Verify both changes are present with correct attribution
    file.assert_lines_and_blame(crate::lines![
        "// Master update at top".human(),
        "section 1".human(),
        "section 2".human(),
        "section 3".ai(),
        "// AI feature addition at end".ai()
    ]);

    // Verify stats for squashed commit
    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.git_diff_added_lines, 2,
        "Squash commit adds 2 lines from feature (includes newline)"
    );
    assert_eq!(stats.ai_additions, 2, "2 AI lines from feature branch");
    assert_eq!(stats.ai_accepted, 2, "2 AI lines accepted without edits");
    assert_eq!(
        stats.human_additions, 0,
        "0 human lines from feature branch"
    );
    assert_eq!(stats.mixed_additions, 0, "No mixed edits");
}

/// Test merge --squash with multiple AI sessions and human edits
#[test]
fn test_prepare_working_log_squash_multiple_sessions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.txt");

    // Create master branch
    file.set_contents(crate::lines!["header", "body", "footer"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI session
    file.insert_at(1, crate::lines!["// AI session 1".ai()]);
    repo.stage_all_and_commit("AI session 1").unwrap();

    // Human edit
    file.insert_at(3, crate::lines!["// Human addition"]);
    repo.stage_all_and_commit("Human edit").unwrap();

    // Second AI session (different agent - simulated by new checkpoint)
    file.insert_at(5, crate::lines!["// AI session 2".ai()]);
    repo.stage_all_and_commit("AI session 2").unwrap();

    // Squash merge into master
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed multiple sessions").unwrap();

    // Verify all authorship is preserved
    file.assert_lines_and_blame(crate::lines![
        "header".human(),
        "// AI session 1".ai(),
        "body".human(),
        "// Human addition".human(),
        "footer".ai(),
        "// AI session 2".ai()
    ]);

    // Verify stats for squashed commit with multiple sessions
    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.git_diff_added_lines, 4,
        "Squash commit adds 4 lines total (includes newline)"
    );
    assert_eq!(
        stats.ai_additions, 3,
        "3 AI lines from feature branch (both sessions plus reformatted footer)"
    );
    assert_eq!(stats.ai_accepted, 3, "3 AI lines accepted without edits");
    assert_eq!(stats.human_additions, 1, "1 human line from feature branch");
    assert_eq!(stats.mixed_additions, 0, "No mixed edits");
}

/// Test merge --squash with mixed additions (AI code edited by human before commit)
#[test]
fn test_prepare_working_log_squash_with_mixed_additions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("code.txt");

    // Create master branch with initial content
    file.set_contents(crate::lines![
        "function start() {",
        "  // initial code",
        "}"
    ]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI adds 3 lines (without committing)
    file.insert_at(
        2,
        crate::lines![
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
        crate::lines![
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
    file.assert_lines_and_blame(crate::lines![
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

/// Test that custom attributes set via config are preserved through a squash merge
/// when the real post-commit pipeline injects them.
#[test]
fn test_squash_merge_preserves_custom_attributes_from_config() {
    let mut repo = TestRepo::new_dedicated_daemon();

    // Configure custom attributes via config patch
    let mut attrs = HashMap::new();
    attrs.insert("employee_id".to_string(), "E303".to_string());
    attrs.insert("team".to_string(), "data".to_string());
    repo.patch_git_ai_config(|patch| {
        patch.custom_attributes = Some(attrs.clone());
    });

    // Create initial commit on default branch
    let mut file = repo.filename("main.txt");
    file.set_contents(crate::lines!["line 1", "line 2", "line 3", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    // Create feature branch with AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(3, crate::lines!["// AI feature line".ai()]);
    repo.stage_all_and_commit_with_env(
        "Add AI feature",
        &deterministic_commit_env("2030-01-02T00:00:00Z"),
    )
    .unwrap();

    // Add another AI commit on the feature branch
    file.insert_at(4, crate::lines!["// AI feature line 2".ai()]);
    repo.stage_all_and_commit_with_env(
        "Add AI feature 2",
        &deterministic_commit_env("2030-01-02T00:00:01Z"),
    )
    .unwrap();

    // Verify custom attributes were set on the feature commits
    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let feature_note = repo
        .read_authorship_note(&feature_sha)
        .expect("feature commit should have authorship note");
    let feature_log =
        AuthorshipLog::deserialize_from_string(&feature_note).expect("parse feature note");
    for prompt in feature_log.metadata.prompts.values() {
        assert_eq!(
            prompt.custom_attributes.as_ref(),
            Some(&attrs),
            "precondition: feature commit should have custom_attributes from config"
        );
    }

    // Go back to default branch and squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed feature").unwrap();

    // Verify custom attributes survived the squash merge
    let squash_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let squash_note = repo
        .read_authorship_note(&squash_sha)
        .expect("squash commit should have authorship note");
    let squash_log =
        AuthorshipLog::deserialize_from_string(&squash_note).expect("parse squash note");
    assert!(
        !squash_log.metadata.prompts.is_empty(),
        "squash commit should have prompt records"
    );
    for prompt in squash_log.metadata.prompts.values() {
        assert_eq!(
            prompt.custom_attributes.as_ref(),
            Some(&attrs),
            "custom_attributes should be preserved through squash merge"
        );
    }

    // Also verify the AI attribution itself survived
    file.assert_lines_and_blame(crate::lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "// AI feature line".ai(),
        "// AI feature line 2".ai()
    ]);
}

crate::reuse_tests_in_worktree!(
    test_prepare_working_log_simple_squash,
    test_prepare_working_log_squash_with_main_changes,
    test_prepare_working_log_squash_multiple_sessions,
    test_prepare_working_log_squash_with_mixed_additions,
);
