#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Test cherry-picking a single AI-authored commit
#[test]
fn test_single_commit_cherry_pick() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Initial content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get current branch name
    let main_branch = repo.current_branch();

    // Create feature branch with AI-authored changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["AI feature line".ai()]);
    repo.stage_all_and_commit("Add AI feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick the feature commit
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_commit]).unwrap();

    // Verify final file state - hooks should have preserved AI authorship
    file.assert_lines_and_blame(lines!["Initial content".human(), "AI feature line".ai(),]);

    // Verify stats
    let stats = repo.stats().unwrap();
    assert_eq!(
        stats.git_diff_added_lines, 2,
        "Should add 1 AI line (+ newline)"
    );
    assert_eq!(stats.ai_additions, 1, "1 AI line added");
    assert_eq!(stats.ai_accepted, 1, "1 AI line accepted");
    assert_eq!(stats.human_additions, 1, "1 human line (newline)");

    // Verify prompt records have correct stats
    let head_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = git_ai::git::refs::get_reference_as_authorship_log_v3(
        &git_ai::git::find_repository_in_path(repo.path().to_str().unwrap()).unwrap(),
        &head_commit,
    )
    .unwrap();

    let prompts = &log.metadata.prompts;
    assert!(
        !prompts.is_empty(),
        "Should have at least one prompt record"
    );

    for (prompt_id, prompt_record) in prompts {
        assert!(
            prompt_record.accepted_lines > 0,
            "Prompt {} should have accepted_lines > 0",
            prompt_id
        );
        assert_eq!(
            prompt_record.overriden_lines, 0,
            "Prompt {} should have overridden_lines = 0",
            prompt_id
        );
    }

    let total_accepted: u32 = prompts.values().map(|p| p.accepted_lines).sum();
    assert_eq!(
        total_accepted, stats.ai_accepted,
        "Sum of accepted_lines should match ai_accepted stat"
    );
}

/// Test cherry-picking multiple commits in sequence
#[test]
fn test_multiple_commits_cherry_pick() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with multiple AI-authored commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    file.insert_at(1, lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();
    let commit1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Second AI commit
    file.insert_at(2, lines!["AI line 3".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let commit2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Third AI commit
    file.insert_at(3, lines!["AI line 4".ai()]);
    repo.stage_all_and_commit("AI commit 3").unwrap();
    let commit3 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick all three commits
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &commit1, &commit2, &commit3])
        .unwrap();

    // Verify final file state - hooks should have preserved AI authorship
    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
    ]);

    // Verify stats for the last cherry-picked commit
    let stats = repo.stats().unwrap();
    eprintln!("Stats: {:?}", stats);
    // Last commit inserts "AI line 4" - git_diff_added_lines only counts this commit's changes
    // ai_additions is capped by git_diff_added_lines, so it reflects this commit only
    assert_eq!(stats.git_diff_added_lines, 1, "Should have added 1 lines");
    assert_eq!(stats.ai_additions, 1, "At least 1 AI line in this commit");
    assert_eq!(stats.ai_accepted, 1, "1 AI lines accepted in commit");

    // Verify prompt records have correct stats
    let head_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = git_ai::git::refs::get_reference_as_authorship_log_v3(
        &git_ai::git::find_repository_in_path(repo.path().to_str().unwrap()).unwrap(),
        &head_commit,
    )
    .unwrap();

    let prompts = &log.metadata.prompts;
    for (prompt_id, prompt_record) in prompts {
        assert!(
            prompt_record.accepted_lines > 0,
            "Prompt {} should have accepted_lines > 0",
            prompt_id
        );
        assert_eq!(
            prompt_record.overriden_lines, 0,
            "Prompt {} should have overridden_lines = 0",
            prompt_id
        );
    }
}

/// Test cherry-pick with conflicts and --continue
#[test]
fn test_cherry_pick_with_conflict_and_continue() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1", "Line 2", "Line 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(1, "AI_FEATURE_VERSION".ai());
    repo.stage_all_and_commit("AI feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and make conflicting change
    repo.git(&["checkout", &main_branch]).unwrap();
    file.replace_at(1, "MAIN_BRANCH_VERSION".human());
    repo.stage_all_and_commit("Human change").unwrap();

    // Try to cherry-pick (should conflict)
    let cherry_pick_result = repo.git(&["cherry-pick", &feature_commit]);
    assert!(cherry_pick_result.is_err(), "Should have conflict");

    // Resolve conflict by choosing the AI version
    use std::fs;
    fs::write(
        repo.path().join("file.txt"),
        "Line 1\nAI_FEATURE_VERSION\nLine 3",
    )
    .unwrap();
    repo.git(&["add", "file.txt"]).unwrap();

    // Continue cherry-pick
    repo.git(&["cherry-pick", "--continue"]).unwrap();

    // Verify final file state - hooks should have preserved AI authorship
    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "AI_FEATURE_VERSION".ai(),
        "Line 3".human(),
    ]);
}

/// Test cherry-pick --abort
#[test]
fn test_cherry_pick_abort() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1", "Line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let initial_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let main_branch = repo.current_branch();

    // Create feature branch with AI changes (modify line 2)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(1, "AI modification of line 2".ai());
    repo.stage_all_and_commit("AI feature").unwrap();

    // Assert intermediary blame
    file.assert_lines_and_blame(lines!["Line 1".human(), "AI modification of line 2".ai(),]);

    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and make conflicting change (also modify line 2)
    repo.git(&["checkout", &main_branch]).unwrap();
    file.replace_at(1, "Human modification of line 2".human());
    repo.stage_all_and_commit("Human change").unwrap();

    // Assert intermediary blame
    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "Human modification of line 2".human(),
    ]);

    // Try to cherry-pick (should conflict)
    let cherry_pick_result = repo.git(&["cherry-pick", &feature_commit]);
    assert!(cherry_pick_result.is_err(), "Should have conflict");

    // Abort the cherry-pick
    repo.git(&["cherry-pick", "--abort"]).unwrap();

    // Verify HEAD is back to before the cherry-pick
    let current_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(current_head, initial_head); // Different because we made the "Human change" commit

    // Verify final file state (should have human's version)
    file.assert_lines_and_blame(lines![
        "Line 1".human(),
        "Human modification of line 2".human(),
    ]);
}

/// Test cherry-picking from branch without AI authorship
#[test]
fn test_cherry_pick_no_ai_authorship() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();
    // Create feature branch with human-only changes (no AI)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["Human line 2".human()]);
    repo.stage_all_and_commit("Human feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_commit]).unwrap();

    // Verify final file state - should have no AI authorship
    file.assert_lines_and_blame(lines!["Line 1".human(), "Human line 2".human(),]);
}

/// Test cherry-pick preserving multiple AI sessions from different commits
#[test]
fn test_cherry_pick_multiple_ai_sessions() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("main.rs");
    file.set_contents(lines!["fn main() {}"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI session adds logging
    file.replace_at(0, "fn main() {".human());
    file.insert_at(1, lines!["    println!(\"Starting\");".ai()]);
    file.insert_at(2, lines!["}".human()]);
    repo.stage_all_and_commit("Add logging").unwrap();
    let commit1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Second AI session adds error handling
    file.insert_at(2, lines!["    // TODO: Add error handling".ai()]);
    repo.stage_all_and_commit("Add error handling").unwrap();
    let commit2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Cherry-pick both to main
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &commit1, &commit2]).unwrap();

    // Verify final file state - hooks should have preserved AI authorship
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    println!(\"Starting\");".ai(),
        "    // TODO: Add error handling".ai(),
        "}".human(),
    ]);

    // Verify stats for the last cherry-picked commit
    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1, "Last commit adds 1 line");
    assert_eq!(stats.ai_additions, 1, "1 AI line in last commit");
    assert_eq!(stats.ai_accepted, 1, "1 AI lines accepted");

    // Verify prompt records have correct stats
    let head_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let log = git_ai::git::refs::get_reference_as_authorship_log_v3(
        &git_ai::git::find_repository_in_path(repo.path().to_str().unwrap()).unwrap(),
        &head_commit,
    )
    .unwrap();

    let prompts = &log.metadata.prompts;
    assert!(
        !prompts.is_empty(),
        "Should have at least one prompt record"
    );

    for (prompt_id, prompt_record) in prompts {
        assert!(
            prompt_record.accepted_lines > 0,
            "Prompt {} should have accepted_lines > 0",
            prompt_id
        );
        assert_eq!(
            prompt_record.overriden_lines, 0,
            "Prompt {} should have overridden_lines = 0",
            prompt_id
        );
    }
}

/// Test that trees-identical fast path works
#[test]
fn test_cherry_pick_identical_trees() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["AI line".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Add another commit on feature (just to have a parent)
    file.insert_at(2, lines!["More AI".ai()]);
    repo.stage_all_and_commit("More AI").unwrap();

    // Cherry-pick the first feature commit to main
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_commit]).unwrap();

    // Verify final file state - hooks should have preserved AI authorship
    file.assert_lines_and_blame(lines!["Line 1".human(), "AI line".ai(),]);
}

/// Test cherry-pick where some commits become empty (already applied)
#[test]
fn test_cherry_pick_empty_commits() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["Feature line".ai()]);
    repo.stage_all_and_commit("Add feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Manually apply the same change to main
    repo.git(&["checkout", &main_branch]).unwrap();

    // Get a fresh TestFile after branch switch - it will auto-populate from the existing file
    let mut file_on_main = repo.filename("file.txt");
    file_on_main.insert_at(1, lines!["Feature line".human()]);
    repo.stage_all_and_commit("Apply feature manually").unwrap();

    // Try to cherry-pick the feature commit (should become empty or conflict)
    let result = repo.git(&["cherry-pick", &feature_commit]);

    // Git might succeed and skip the empty commit, or it might create a conflict
    // The key is that it shouldn't crash
    match result {
        Ok(_) => {
            // Empty commit was skipped successfully
        }
        Err(_) => {
            // Git reported an error (conflict or empty commit)
            // Abort the cherry-pick to clean up
            let _ = repo.git(&["cherry-pick", "--abort"]);
        }
    }

    // Verify final file state - content should be preserved
    let actual_content = repo.read_file("file.txt").unwrap();
    assert_eq!(
        actual_content.trim(),
        "Line 1\nFeature line",
        "File content should be preserved after cherry-pick/abort"
    );
}
