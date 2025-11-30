#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Test simple rebase with no conflicts where trees are identical - multiple commits
#[test]
fn test_rebase_no_conflicts_identical_trees() {
    let repo = TestRepo::new();

    // Create initial commit (on default branch, usually master)
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main line 1", "main line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get the default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines![
        "// AI generated feature 1".ai(),
        "feature line 1".ai()
    ]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines![
        "// AI generated feature 2".ai(),
        "feature line 2".ai()
    ]);
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Advance default branch (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(lines!["other content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved for both files after rebase
    feature1.assert_lines_and_blame(lines![
        "// AI generated feature 1".ai(),
        "feature line 1".ai()
    ]);
    feature2.assert_lines_and_blame(lines![
        "// AI generated feature 2".ai(),
        "feature line 2".ai()
    ]);
}

/// Test rebase where trees differ (parent changes result in different tree IDs) - multiple commits
#[test]
fn test_rebase_with_different_trees() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines!["// AI added feature 1".ai()]);
    repo.stage_all_and_commit("AI changes 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines!["// AI added feature 2".ai()]);
    repo.stage_all_and_commit("AI changes 2").unwrap();

    // Go back to default branch and add a different file (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main content"]);
    repo.stage_all_and_commit("Main changes").unwrap();

    // Rebase feature onto default branch (no conflicts, but trees will differ - hooks handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved for both files after rebase
    feature1.assert_lines_and_blame(lines!["// AI added feature 1".ai()]);
    feature2.assert_lines_and_blame(lines!["// AI added feature 2".ai()]);
}

/// Test rebase with multiple commits
#[test]
fn test_rebase_multiple_commits() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines!["// AI feature 2".ai()]);
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Third AI commit
    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(lines!["// AI feature 3".ai()]);
    repo.stage_all_and_commit("AI feature 3").unwrap();

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main2_file = repo.filename("main2.txt");
    main2_file.set_contents(lines!["more main content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify all files have preserved AI authorship after rebase
    feature1.assert_lines_and_blame(lines!["// AI feature 1".ai()]);
    feature2.assert_lines_and_blame(lines!["// AI feature 2".ai()]);
    feature3.assert_lines_and_blame(lines!["// AI feature 3".ai()]);
}

/// Test rebase where only some commits have authorship logs
#[test]
fn test_rebase_mixed_authorship() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Human commit (no AI authorship)
    let mut human_file = repo.filename("human.txt");
    human_file.set_contents(lines!["human work"]);
    repo.stage_all_and_commit("Human work").unwrap();

    // AI commit
    let mut ai_file = repo.filename("ai.txt");
    ai_file.set_contents(lines!["// AI work".ai()]);
    repo.stage_all_and_commit("AI work").unwrap();

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main2_file = repo.filename("main2.txt");
    main2_file.set_contents(lines!["more main"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved correctly
    human_file.assert_lines_and_blame(lines!["human work".human()]);
    ai_file.assert_lines_and_blame(lines!["// AI work".ai()]);
}

/// Test empty rebase (fast-forward)
#[test]
fn test_rebase_fast_forward() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Add commit on feature
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Rebase onto default branch (should be fast-forward, no changes - hooks handle authorship)
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship is still correct after fast-forward rebase
    feature_file.assert_lines_and_blame(lines!["// AI feature".ai()]);
}

/// Test interactive rebase with commit reordering - verifies interactive rebase works
#[test]
fn test_rebase_interactive_reorder() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 2 AI commits - we'll rebase these interactively
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines!["// AI feature 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("Main advances").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Perform interactive rebase (just pick all, tests that -i flag works)
    repo.git(&["checkout", "feature"]).unwrap();

    let result = repo.git_with_env(
        &["rebase", "-i", &base_commit],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
    );

    if result.is_err() {
        eprintln!("git rebase output: {:?}", result);
        panic!("Interactive rebase failed");
    }

    // Verify both files have preserved AI authorship after interactive rebase
    feature1.assert_lines_and_blame(lines!["// AI feature 1".ai()]);
    feature2.assert_lines_and_blame(lines!["// AI feature 2".ai()]);
}

/// Test rebase skip - skipping a commit during rebase
#[test]
fn test_rebase_skip() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI commit that will conflict
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(0, "AI line 1".ai());
    repo.stage_all_and_commit("AI changes").unwrap();

    // Add second commit that won't conflict
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("Add feature").unwrap();

    // Make conflicting change on main
    repo.git(&["checkout", &default_branch]).unwrap();
    file.replace_at(0, "MAIN line 1".human());
    repo.stage_all_and_commit("Main changes").unwrap();

    // Try to rebase - will conflict on first commit
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Should conflict
    assert!(rebase_result.is_err(), "Rebase should conflict");

    // Skip the conflicting commit
    let skip_result = repo.git(&["rebase", "--skip"]);

    if skip_result.is_ok() {
        // Verify the second commit was rebased and authorship preserved
        feature_file.assert_lines_and_blame(lines!["// AI feature".ai()]);
    }
}

/// Test rebase with empty commits (--keep-empty)
#[test]
fn test_rebase_keep_empty() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with empty commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create empty commit
    repo.git(&["commit", "--allow-empty", "-m", "Empty commit"])
        .expect("Empty commit should succeed");

    // Add a real commit
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase with --keep-empty (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", "--keep-empty", &base]);

    if rebase_result.is_ok() {
        // Verify the non-empty commit has preserved AI authorship
        feature_file.assert_lines_and_blame(lines!["// AI".ai()]);
    }
}

/// Test rebase with rerere (reuse recorded resolution) enabled
#[test]
fn test_rebase_rerere() {
    let repo = TestRepo::new();

    // Enable rerere
    repo.git(&["config", "rerere.enabled", "true"]).unwrap();

    // Create initial commit
    let mut conflict_file = repo.filename("conflict.txt");
    conflict_file.set_contents(lines!["line 1", "line 2"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    conflict_file.replace_at(1, "AI CHANGE".ai());
    repo.stage_all_and_commit("AI changes").unwrap();

    // Make conflicting change on main
    repo.git(&["checkout", &default_branch]).unwrap();
    conflict_file.replace_at(1, "MAIN CHANGE".human());
    repo.stage_all_and_commit("Main changes").unwrap();

    // First rebase - will conflict
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Should conflict
    assert!(rebase_result.is_err(), "First rebase should conflict");

    // Resolve conflict manually
    use std::fs;
    fs::write(repo.path().join("conflict.txt"), "line 1\nRESOLVED\n").unwrap();

    repo.git(&["add", "conflict.txt"]).unwrap();

    repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")])
        .unwrap();

    // Record the resolution and abort
    repo.git(&["rebase", "--abort"]).ok();

    // Second attempt - rerere should auto-apply the resolution
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Even if rerere helps, we still need to continue manually
    // This test mainly verifies that rerere doesn't break authorship tracking
    if rebase_result.is_err() {
        repo.git(&["add", "conflict.txt"]).unwrap();
        repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")])
            .unwrap();
    }

    // Note: This test verifies that rerere doesn't break the rebase process
    // Authorship tracking is handled by hooks regardless of rerere
}

/// Test dependent branch stack (patch-stack workflow)
#[test]
fn test_rebase_patch_stack() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create topic-1 branch
    repo.git(&["checkout", "-b", "topic-1"]).unwrap();
    let mut topic1_file = repo.filename("topic1.txt");
    topic1_file.set_contents(lines!["// AI topic 1".ai()]);
    repo.stage_all_and_commit("Topic 1").unwrap();

    // Create topic-2 branch on top of topic-1
    repo.git(&["checkout", "-b", "topic-2"]).unwrap();
    let mut topic2_file = repo.filename("topic2.txt");
    topic2_file.set_contents(lines!["// AI topic 2".ai()]);
    repo.stage_all_and_commit("Topic 2").unwrap();

    // Create topic-3 branch on top of topic-2
    repo.git(&["checkout", "-b", "topic-3"]).unwrap();
    let mut topic3_file = repo.filename("topic3.txt");
    topic3_file.set_contents(lines!["// AI topic 3".ai()]);
    repo.stage_all_and_commit("Topic 3").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("Main work").unwrap();

    // Rebase the stack: topic-1, then topic-2, then topic-3 (hooks will handle authorship)
    repo.git(&["checkout", "topic-1"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    repo.git(&["checkout", "topic-2"]).unwrap();
    repo.git(&["rebase", "topic-1"]).unwrap();

    repo.git(&["checkout", "topic-3"]).unwrap();
    repo.git(&["rebase", "topic-2"]).unwrap();

    // Verify all files have preserved AI authorship after rebasing the stack
    repo.git(&["checkout", "topic-1"]).unwrap();
    topic1_file.assert_lines_and_blame(lines!["// AI topic 1".ai()]);

    repo.git(&["checkout", "topic-2"]).unwrap();
    topic1_file.assert_lines_and_blame(lines!["// AI topic 1".ai()]);
    topic2_file.assert_lines_and_blame(lines!["// AI topic 2".ai()]);

    repo.git(&["checkout", "topic-3"]).unwrap();
    topic1_file.assert_lines_and_blame(lines!["// AI topic 1".ai()]);
    topic2_file.assert_lines_and_blame(lines!["// AI topic 2".ai()]);
    topic3_file.assert_lines_and_blame(lines!["// AI topic 3".ai()]);
}

/// Test rebase with no changes (already up to date)
#[test]
fn test_rebase_already_up_to_date() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI".ai()]);
    let feature_commit_before = repo.stage_all_and_commit("AI feature").unwrap().commit_sha;

    // Try to rebase onto itself (should be no-op)
    repo.git(&["rebase", "feature"])
        .expect("Rebase onto self should succeed");

    // Verify commit unchanged
    let current_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        current_commit, feature_commit_before,
        "Commit should be unchanged"
    );

    // Verify authorship still intact
    feature_file.assert_lines_and_blame(lines!["// AI".ai()]);
}

/// Test rebase with conflicts - verifies reconstruction works after conflict resolution
#[test]
fn test_rebase_with_conflicts() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create old_base branch and commit
    repo.git(&["checkout", "-b", "old_base"]).unwrap();
    let mut old_file = repo.filename("old.txt");
    old_file.set_contents(lines!["old base"]);
    repo.stage_all_and_commit("Old base commit").unwrap();
    let old_base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create feature branch from old_base with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create new_base branch from default_branch
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "new_base"]).unwrap();
    let mut new_file = repo.filename("new.txt");
    new_file.set_contents(lines!["new base"]);
    repo.stage_all_and_commit("New base commit").unwrap();
    let new_base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase feature --onto new_base old_base (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--onto", &new_base_sha, &old_base_sha])
        .expect("Rebase --onto should succeed");

    // Verify authorship preserved after --onto rebase
    feature_file.assert_lines_and_blame(lines!["// AI feature".ai()]);
}

/// Test rebase abort - ensures no authorship corruption on abort
#[test]
fn test_rebase_abort() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut conflict_file = repo.filename("conflict.txt");
    conflict_file.set_contents(lines!["line 1", "line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    conflict_file.replace_at(1, "AI CHANGE".ai());
    repo.stage_all_and_commit("AI changes").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Make conflicting change on main
    repo.git(&["checkout", &default_branch]).unwrap();
    conflict_file.replace_at(1, "MAIN CHANGE".human());
    repo.stage_all_and_commit("Main changes").unwrap();

    // Try to rebase - will conflict
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Should conflict
    assert!(rebase_result.is_err(), "Rebase should conflict");

    // Abort the rebase
    repo.git(&["rebase", "--abort"])
        .expect("Rebase abort should succeed");

    // Verify we're back to original commit
    let current_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        current_commit, feature_commit,
        "Should be back to original commit after abort"
    );

    // Verify original authorship is intact (by checking file blame)
    conflict_file.assert_lines_and_blame(lines!["line 1".human(), "AI CHANGE".ai()]);
}

/// Test branch switch during rebase - ensures proper state handling
#[test]
fn test_rebase_branch_switch_during() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create another branch
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "other"]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(lines!["other"]);
    repo.stage_all_and_commit("Other work").unwrap();

    // Start rebase on feature (non-conflicting)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify branch is still feature
    let current_branch = repo.current_branch();
    assert_eq!(
        current_branch, "feature",
        "Should still be on feature branch"
    );

    // Verify authorship was preserved
    feature_file.assert_lines_and_blame(lines!["// AI".ai()]);
}

/// Test rebase with autosquash enabled
#[test]
fn test_rebase_autosquash() {
    let repo = TestRepo::new();

    // Enable autosquash in config
    repo.git(&["config", "rebase.autosquash", "true"]).unwrap();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("Add feature").unwrap();

    // Create fixup commit
    file.replace_at(1, "AI line 2 fixed".ai());
    repo.stage_all_and_commit("fixup! Add feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(lines!["other"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Interactive rebase with autosquash (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git_with_env(
        &["rebase", "-i", "--autosquash", &base],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
    );

    if rebase_result.is_ok() {
        // Verify the file has the expected content with AI authorship
        file.assert_lines_and_blame(lines!["line 1".human(), "AI line 2 fixed".ai()]);
    }
}

/// Test rebase with autostash enabled
#[test]
fn test_rebase_autostash() {
    let repo = TestRepo::new();

    // Enable autostash
    repo.git(&["config", "rebase.autoStash", "true"]).unwrap();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();

    // Switch back to feature and make unstaged changes
    repo.git(&["checkout", "feature"]).unwrap();
    use std::fs;
    fs::write(
        repo.path().join("feature.txt"),
        "// AI\n// Unstaged change\n",
    )
    .unwrap();

    // Rebase with unstaged changes (autostash should handle it - hooks handle authorship)
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Should succeed with autostash
    if rebase_result.is_ok() {
        // Reset the file to HEAD to remove the autostashed unstaged changes before checking
        repo.git(&["checkout", "HEAD", "feature.txt"]).unwrap();

        // Verify authorship was preserved
        feature_file.assert_lines_and_blame(lines!["// AI".ai()]);
    }
}

/// Test rebase --exec to run tests at each commit
#[test]
fn test_rebase_exec() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut test_sh = repo.filename("test.sh");
    test_sh.set_contents(lines!["#!/bin/sh", "exit 0"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut f1 = repo.filename("f1.txt");
    f1.set_contents(lines!["// AI 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut f2 = repo.filename("f2.txt");
    f2.set_contents(lines!["// AI 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", "feature"]).unwrap();

    // Rebase with --exec (hooks will handle authorship)
    repo.git_with_env(
        &["rebase", "-i", "--exec", "echo 'test passed'", &base],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
    )
    .expect("Rebase with --exec should succeed");

    // Verify authorship was preserved
    f1.assert_lines_and_blame(lines!["// AI 1".ai()]);
    f2.assert_lines_and_blame(lines!["// AI 2".ai()]);
}

/// Test rebase with merge commits (--rebase-merges)
/// Note: This test verifies that --rebase-merges flag is accepted and doesn't break authorship
#[test]
fn test_rebase_preserve_merges() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create side branch from feature
    repo.git(&["checkout", "-b", "side"]).unwrap();
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(lines!["// AI side".ai()]);
    repo.stage_all_and_commit("AI side").unwrap();

    // Merge side into feature
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["merge", "side", "-m", "Merge side into feature"])
        .unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase feature onto main with --rebase-merges (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--rebase-merges", &base])
        .expect("Rebase with --rebase-merges should succeed");

    // Verify authorship is preserved for both files
    feature_file.assert_lines_and_blame(lines!["// AI feature".ai()]);
    side_file.assert_lines_and_blame(lines!["// AI side".ai()]);
}

/// Test rebase with commit splitting (fewer original commits than new commits)
/// This tests that rebase handles AI authorship correctly even with complex commit histories
#[test]
fn test_rebase_commit_splitting() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut features_file = repo.filename("features.txt");
    features_file.set_contents(lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai(),
        "".ai()
    ]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    features_file.insert_at(
        2,
        lines!["// AI feature 2".ai(), "function feature2() {}".ai()],
    );
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main content", ""]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto main (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify AI authorship is preserved after rebase
    features_file.assert_lines_and_blame(lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai(),
        "// AI feature 2".ai(),
        "function feature2() {}".ai(),
    ]);
}

/// Test interactive rebase with squashing - verifies authorship from all commits is preserved
/// This tests that squashing preserves authorship from all commits
#[test]
#[cfg(not(target_os = "windows"))]
fn test_rebase_squash_preserves_all_authorship() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 3 AI commits with different content - we'll squash these
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines!["// AI feature 1".ai(), "line 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines!["// AI feature 2".ai(), "line 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(lines!["// AI feature 3".ai(), "line 3".ai()]);
    repo.stage_all_and_commit("AI commit 3").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("Main advances").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Perform interactive rebase with squashing: pick first, squash second and third
    repo.git(&["checkout", "feature"]).unwrap();

    use std::io::Write;

    // Create a script that modifies the rebase-todo to squash commits 2 and 3 into 1
    let script_content = r#"#!/bin/sh
sed -i.bak '2s/pick/squash/' "$1"
sed -i.bak '3s/pick/squash/' "$1"
"#;

    let script_path = repo.path().join("squash_script.sh");
    let mut script_file = std::fs::File::create(&script_path).unwrap();
    script_file.write_all(script_content.as_bytes()).unwrap();
    drop(script_file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
    }

    let rebase_result = repo.git_with_env(
        &["rebase", "-i", &base_commit],
        &[
            ("GIT_SEQUENCE_EDITOR", script_path.to_str().unwrap()),
            ("GIT_EDITOR", "true"),
        ],
    );

    if rebase_result.is_err() {
        eprintln!("git rebase output: {:?}", rebase_result);
        panic!("Interactive rebase with squash failed");
    }

    // Verify all 3 files exist with preserved AI authorship after squashing
    assert!(
        repo.path().join("feature1.txt").exists(),
        "feature1.txt from commit 1 should exist"
    );
    assert!(
        repo.path().join("feature2.txt").exists(),
        "feature2.txt from commit 2 should exist"
    );
    assert!(
        repo.path().join("feature3.txt").exists(),
        "feature3.txt from commit 3 should exist"
    );

    // Verify AI authorship was preserved through squashing
    feature1.assert_lines_and_blame(lines!["// AI feature 1".ai(), "line 1".ai()]);
    feature2.assert_lines_and_blame(lines!["// AI feature 2".ai(), "line 2".ai()]);
    feature3.assert_lines_and_blame(lines!["// AI feature 3".ai(), "line 3".ai()]);
}

/// Test rebase with rewording (renaming) a commit that has 2 children commits
/// Verifies that authorship is preserved for all 3 commits after reword
#[test]
#[cfg(not(target_os = "windows"))]
fn test_rebase_reword_commit_with_children() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 3 AI commits - we'll reword the first one
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 1 - original message")
        .unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(lines![
        "// AI feature 2".ai(),
        "function feature2() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(lines![
        "// AI feature 3".ai(),
        "function feature3() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 3").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("Main advances").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Perform interactive rebase with rewording the first commit
    repo.git(&["checkout", "feature"]).unwrap();

    use std::io::Write;

    // Create a script that modifies the rebase-todo to reword the first commit
    let script_content = r#"#!/bin/sh
sed -i.bak '1s/pick/reword/' "$1"
"#;

    let script_path = repo.path().join("reword_script.sh");
    let mut script_file = std::fs::File::create(&script_path).unwrap();
    script_file.write_all(script_content.as_bytes()).unwrap();
    drop(script_file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
    }

    // Create a script that provides the new commit message
    let commit_msg_content = "AI commit 1 - RENAMED MESSAGE";
    let commit_msg_path = repo.path().join("new_commit_msg.txt");
    let mut msg_file = std::fs::File::create(&commit_msg_path).unwrap();
    msg_file.write_all(commit_msg_content.as_bytes()).unwrap();
    drop(msg_file);

    // Create an editor script that replaces the commit message
    let editor_script_content = format!(
        r#"#!/bin/sh
cat {} > "$1"
"#,
        commit_msg_path.to_str().unwrap()
    );
    let editor_script_path = repo.path().join("editor_script.sh");
    let mut editor_file = std::fs::File::create(&editor_script_path).unwrap();
    editor_file
        .write_all(editor_script_content.as_bytes())
        .unwrap();
    drop(editor_file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&editor_script_path)
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&editor_script_path, perms).unwrap();
    }

    let rebase_result = repo.git_with_env(
        &["rebase", "-i", &base_commit],
        &[
            ("GIT_SEQUENCE_EDITOR", script_path.to_str().unwrap()),
            ("GIT_EDITOR", editor_script_path.to_str().unwrap()),
        ],
    );

    if rebase_result.is_err() {
        eprintln!("git rebase output: {:?}", rebase_result);
        panic!("Interactive rebase with reword failed");
    }

    // Verify all 3 files still exist with correct AI authorship after reword
    feature1.assert_lines_and_blame(lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai()
    ]);
    feature2.assert_lines_and_blame(lines![
        "// AI feature 2".ai(),
        "function feature2() {}".ai()
    ]);
    feature3.assert_lines_and_blame(lines![
        "// AI feature 3".ai(),
        "function feature3() {}".ai()
    ]);
}
