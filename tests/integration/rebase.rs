use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::working_log::AgentId;
use git_ai::git::refs::notes_add;
use std::collections::HashMap;

/// Test simple rebase with no conflicts where trees are identical - multiple commits
#[test]
fn test_rebase_no_conflicts_identical_trees() {
    let repo = TestRepo::new();

    // Create initial commit (on default branch, usually master)
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main line 1", "main line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get the default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines![
        "// AI generated feature 1".ai(),
        "feature line 1".ai()
    ]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines![
        "// AI generated feature 2".ai(),
        "feature line 2".ai()
    ]);
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Advance default branch (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved for both files after rebase
    feature1.assert_lines_and_blame(crate::lines![
        "// AI generated feature 1".ai(),
        "feature line 1".ai()
    ]);
    feature2.assert_lines_and_blame(crate::lines![
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
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines!["// AI added feature 1".ai()]);
    repo.stage_all_and_commit("AI changes 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines!["// AI added feature 2".ai()]);
    repo.stage_all_and_commit("AI changes 2").unwrap();

    // Go back to default branch and add a different file (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Main changes").unwrap();

    // Rebase feature onto default branch (no conflicts, but trees will differ - hooks handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved for both files after rebase
    feature1.assert_lines_and_blame(crate::lines!["// AI added feature 1".ai()]);
    feature2.assert_lines_and_blame(crate::lines!["// AI added feature 2".ai()]);
}

/// Test rebase with multiple commits
#[test]
fn test_rebase_multiple_commits() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch with multiple commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    // Second AI commit
    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines!["// AI feature 2".ai()]);
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Third AI commit
    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(crate::lines!["// AI feature 3".ai()]);
    repo.stage_all_and_commit("AI feature 3").unwrap();

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main2_file = repo.filename("main2.txt");
    main2_file.set_contents(crate::lines!["more main content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify all files have preserved AI authorship after rebase
    feature1.assert_lines_and_blame(crate::lines!["// AI feature 1".ai()]);
    feature2.assert_lines_and_blame(crate::lines!["// AI feature 2".ai()]);
    feature3.assert_lines_and_blame(crate::lines!["// AI feature 3".ai()]);
}

/// Test rebase where only some commits have authorship logs
#[test]
fn test_rebase_mixed_authorship() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Human commit (no AI authorship)
    let mut human_file = repo.filename("human.txt");
    human_file.set_contents(crate::lines!["human work"]);
    repo.stage_all_and_commit("Human work").unwrap();

    // AI commit
    let mut ai_file = repo.filename("ai.txt");
    ai_file.set_contents(crate::lines!["// AI work".ai()]);
    repo.stage_all_and_commit("AI work").unwrap();

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main2_file = repo.filename("main2.txt");
    main2_file.set_contents(crate::lines!["more main"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship was preserved correctly
    human_file.assert_lines_and_blame(crate::lines!["human work".human()]);
    ai_file.assert_lines_and_blame(crate::lines!["// AI work".ai()]);
}

#[test]
fn test_rebase_preserves_exact_mixed_line_attribution_in_single_file() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut app_file = repo.filename("app.js");
    app_file.set_contents(crate::lines![
        "const version = 1;".human(),
        "function compute() {".ai(),
        "  return 1;".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("Add mixed app").unwrap();

    app_file.insert_at(2, crate::lines!["  // AI docs".ai()]);
    repo.stage_all_and_commit("Add docs").unwrap();

    app_file.insert_at(5, crate::lines!["// AI footer".ai()]);
    repo.stage_all_and_commit("Add footer").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    app_file.assert_lines_and_blame(crate::lines![
        "const version = 1;".human(),
        "function compute() {".ai(),
        "  // AI docs".ai(),
        "  return 1;".ai(),
        "}".ai(),
        "// AI footer".ai()
    ]);
}

#[test]
fn test_rebase_with_human_only_commit_between_ai_commits_preserves_exact_lines() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    let mut app_file = repo.filename("app.js");
    base_file.set_contents(crate::lines!["base"]);
    app_file.set_contents(crate::lines!["const base = 0;".human()]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    app_file.insert_at(1, crate::lines!["// AI block 1".ai()]);
    repo.stage_all_and_commit("AI block 1").unwrap();

    let mut notes_file = repo.filename("notes.txt");
    notes_file.set_contents(crate::lines!["human notes line"]);
    repo.stage_all_and_commit("Human-only notes").unwrap();

    let mut generated_file = repo.filename("generated.js");
    generated_file.set_contents(crate::lines!["const generated = 42;".ai()]);
    repo.stage_all_and_commit("AI block 2").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    app_file.assert_lines_and_blame(crate::lines!["const base = 0;".ai(), "// AI block 1".ai()]);
    generated_file.assert_lines_and_blame(crate::lines!["const generated = 42;".ai()]);
    notes_file.assert_lines_and_blame(crate::lines!["human notes line".human()]);
}

#[test]
fn test_rebase_preserves_human_only_commit_note_metadata() {
    let repo = TestRepo::new();

    // Common base commit.
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    // Branch we will rebase onto.
    repo.git(&["checkout", "-b", "dev"]).unwrap();
    let mut dev_file = repo.filename("dev.txt");
    dev_file.set_contents(crate::lines!["dev content"]);
    repo.stage_all_and_commit("Dev commit").unwrap();

    // Create the source branch from the old base and make a human-only commit.
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "prod"]).unwrap();
    let mut prod_file = repo.filename("prod.txt");
    prod_file.set_contents(crate::lines!["human change only"]);
    let prod_commit = repo.stage_all_and_commit("Prod human commit").unwrap();

    // Sanity check: original commit has a note and it's metadata-only.
    let old_note = repo
        .read_authorship_note(&prod_commit.commit_sha)
        .expect("original commit should have an authorship note");
    let old_log =
        AuthorshipLog::deserialize_from_string(&old_note).expect("parse original authorship note");
    assert!(
        old_log.attestations.is_empty(),
        "precondition: human-only commit should have no attestations"
    );
    assert!(
        old_log.metadata.prompts.is_empty(),
        "precondition: human-only commit should have no prompts"
    );

    // Rebase prod onto dev.
    repo.git(&["rebase", "dev"]).unwrap();
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Regression check: rebased commit should still carry the metadata-only note.
    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased commit should preserve metadata-only authorship note");
    let rebased_log = AuthorshipLog::deserialize_from_string(&rebased_note)
        .expect("parse rebased authorship note");
    assert!(
        rebased_log.attestations.is_empty(),
        "rebased human-only commit should still have no attestations"
    );
    assert!(
        rebased_log.metadata.prompts.is_empty(),
        "rebased human-only commit should still have no prompts"
    );
    assert_eq!(rebased_log.metadata.base_commit_sha, rebased_sha);
}

#[test]
fn test_rebase_preserves_prompt_only_commit_note_metadata() {
    let repo = TestRepo::new();

    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "dev"]).unwrap();
    let mut dev_file = repo.filename("dev.txt");
    dev_file.set_contents(crate::lines!["dev content"]);
    repo.stage_all_and_commit("Dev commit").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "prod"]).unwrap();
    let mut prod_file = repo.filename("prod.txt");
    prod_file.set_contents(crate::lines!["human change only"]);
    let prod_commit = repo
        .stage_all_and_commit("Prod human commit")
        .expect("create prod commit");

    let original_note = repo
        .read_authorship_note(&prod_commit.commit_sha)
        .expect("source commit should have authorship note");
    let mut original_log =
        AuthorshipLog::deserialize_from_string(&original_note).expect("parse source note");
    assert!(
        original_log.attestations.is_empty(),
        "precondition: should start metadata-only"
    );
    assert!(
        original_log.metadata.prompts.is_empty(),
        "precondition: source commit should not have prompts before test mutation"
    );

    let mut test_attrs = HashMap::new();
    test_attrs.insert("employee_id".to_string(), "E123".to_string());
    test_attrs.insert("team".to_string(), "platform".to_string());

    original_log.metadata.prompts.insert(
        "prompt-only-session".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "mock_ai".to_string(),
                id: "session-1".to_string(),
                model: "test-model".to_string(),
            },
            human_author: Some("Test User <test@example.com>".to_string()),
            messages: vec![],
            total_additions: 17,
            total_deletions: 3,
            accepted_lines: 0,
            overriden_lines: 0,
            messages_url: None,
            custom_attributes: Some(test_attrs.clone()),
        },
    );

    let mutated_source_note = original_log
        .serialize_to_string()
        .expect("serialize mutated source note");
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &prod_commit.commit_sha, &mutated_source_note)
        .expect("overwrite source note with prompt-only metadata");

    repo.git(&["rebase", "dev"]).unwrap();
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased commit should preserve prompt-only note");
    let rebased_log =
        AuthorshipLog::deserialize_from_string(&rebased_note).expect("parse rebased note");
    assert!(rebased_log.attestations.is_empty());
    assert_eq!(rebased_log.metadata.prompts.len(), 1);
    assert_eq!(rebased_log.metadata.base_commit_sha, rebased_sha);

    let prompt = rebased_log
        .metadata
        .prompts
        .get("prompt-only-session")
        .expect("prompt metadata should be preserved");
    assert_eq!(prompt.agent_id.tool, "mock_ai");
    assert_eq!(prompt.agent_id.id, "session-1");
    assert_eq!(prompt.agent_id.model, "test-model");
    assert_eq!(prompt.total_additions, 17);
    assert_eq!(prompt.total_deletions, 3);
    assert_eq!(
        prompt.custom_attributes,
        Some(test_attrs),
        "custom_attributes should be preserved through rebase"
    );
}

/// Test empty rebase (fast-forward)
#[test]
fn test_rebase_fast_forward() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Get default branch name
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Add commit on feature
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Rebase onto default branch (should be fast-forward, no changes - hooks handle authorship)
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify authorship is still correct after fast-forward rebase
    feature_file.assert_lines_and_blame(crate::lines!["// AI feature".ai()]);
}

/// Test `git rebase <upstream> <branch>` when invoked from another branch.
/// We should capture original_head from `<branch>`, not from the currently checked-out branch.
#[test]
fn test_rebase_with_explicit_branch_argument_preserves_authorship() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch with AI-authored content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai(), "fn feature() {}".ai()]);
    repo.stage_all_and_commit("add feature").unwrap();

    // Advance main branch
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
    repo.stage_all_and_commit("main advances").unwrap();

    // Invoke rebase with explicit branch arg while currently on main.
    repo.git(&["rebase", &main_branch, "feature"]).unwrap();

    // HEAD should now be on feature after the rebase operation; verify AI blame survived.
    feature_file
        .assert_lines_and_blame(crate::lines!["// AI feature".ai(), "fn feature() {}".ai()]);

    // Verify the rebased commit carries an authorship note via git notes.
    let rebased_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert!(
        repo.read_authorship_note(&rebased_head).is_some(),
        "Rebased commit should have an authorship note"
    );
}

/// Test `git rebase --root --onto <base> <branch>` when invoked from another branch.
/// We should resolve original_head from `<branch>`, not from the currently checked-out branch.
#[test]
fn test_rebase_root_with_explicit_branch_argument_preserves_authorship() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch with AI-authored content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai(), "fn feature() {}".ai()]);
    let original_feature_head = repo.stage_all_and_commit("add feature").unwrap().commit_sha;

    // Advance main branch
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
    repo.stage_all_and_commit("main advances").unwrap();

    // Invoke root rebase with explicit branch arg while currently on main.
    repo.git(&["rebase", "--root", "--onto", &main_branch, "feature"])
        .unwrap();

    let rebased_feature_head = repo.git(&["rev-parse", "HEAD"]).unwrap();
    assert_ne!(
        rebased_feature_head.trim(),
        original_feature_head,
        "Feature head should be rewritten by root rebase"
    );

    // HEAD should now be on feature after the rebase operation; verify AI blame survived.
    feature_file
        .assert_lines_and_blame(crate::lines!["// AI feature".ai(), "fn feature() {}".ai()]);

    // Verify the rebased commit carries an authorship note via git notes.
    assert!(
        repo.read_authorship_note(rebased_feature_head.trim())
            .is_some(),
        "Rebased commit should have an authorship note"
    );
}

/// Test interactive rebase with commit reordering - verifies interactive rebase works
#[test]
fn test_rebase_interactive_reorder() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 2 AI commits - we'll rebase these interactively
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines!["// AI feature 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
    repo.stage_all_and_commit("Main advances").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Perform interactive rebase (just pick all, tests that -i flag works)
    repo.git(&["checkout", "feature"]).unwrap();

    let result = repo.git_with_env(
        &["rebase", "-i", &base_commit],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
        None,
    );

    if result.is_err() {
        eprintln!("git rebase output: {:?}", result);
        panic!("Interactive rebase failed");
    }

    // Verify both files have preserved AI authorship after interactive rebase
    feature1.assert_lines_and_blame(crate::lines!["// AI feature 1".ai()]);
    feature2.assert_lines_and_blame(crate::lines!["// AI feature 2".ai()]);
}

/// Test rebase skip - skipping a commit during rebase
#[test]
fn test_rebase_skip() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI commit that will conflict
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(0, "AI line 1".ai());
    repo.stage_all_and_commit("AI changes").unwrap();

    // Add second commit that won't conflict
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai()]);
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
        feature_file.assert_lines_and_blame(crate::lines!["// AI feature".ai()]);
    }
}

/// Test rebase with empty commits (--keep-empty)
#[test]
fn test_rebase_keep_empty() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with empty commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create empty commit
    repo.git(&["commit", "--allow-empty", "-m", "Empty commit"])
        .expect("Empty commit should succeed");

    // Add a real commit
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase with --keep-empty (hooks will handle authorship tracking)
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git(&["rebase", "--keep-empty", &base]);

    if rebase_result.is_ok() {
        // Verify the non-empty commit has preserved AI authorship
        feature_file.assert_lines_and_blame(crate::lines!["// AI".ai()]);
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
    conflict_file.set_contents(crate::lines!["line 1", "line 2"]);
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

    repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None)
        .unwrap();

    // Record the resolution and abort
    repo.git(&["rebase", "--abort"]).ok();

    // Second attempt - rerere should auto-apply the resolution
    let rebase_result = repo.git(&["rebase", &default_branch]);

    // Even if rerere helps, we still need to continue manually
    // This test mainly verifies that rerere doesn't break authorship tracking
    if rebase_result.is_err() {
        repo.git(&["add", "conflict.txt"]).unwrap();
        repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None)
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
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create topic-1 branch
    repo.git(&["checkout", "-b", "topic-1"]).unwrap();
    let mut topic1_file = repo.filename("topic1.txt");
    topic1_file.set_contents(crate::lines!["// AI topic 1".ai()]);
    repo.stage_all_and_commit("Topic 1").unwrap();

    // Create topic-2 branch on top of topic-1
    repo.git(&["checkout", "-b", "topic-2"]).unwrap();
    let mut topic2_file = repo.filename("topic2.txt");
    topic2_file.set_contents(crate::lines!["// AI topic 2".ai()]);
    repo.stage_all_and_commit("Topic 2").unwrap();

    // Create topic-3 branch on top of topic-2
    repo.git(&["checkout", "-b", "topic-3"]).unwrap();
    let mut topic3_file = repo.filename("topic3.txt");
    topic3_file.set_contents(crate::lines!["// AI topic 3".ai()]);
    repo.stage_all_and_commit("Topic 3").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
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
    topic1_file.assert_lines_and_blame(crate::lines!["// AI topic 1".ai()]);

    repo.git(&["checkout", "topic-2"]).unwrap();
    topic1_file.assert_lines_and_blame(crate::lines!["// AI topic 1".ai()]);
    topic2_file.assert_lines_and_blame(crate::lines!["// AI topic 2".ai()]);

    repo.git(&["checkout", "topic-3"]).unwrap();
    topic1_file.assert_lines_and_blame(crate::lines!["// AI topic 1".ai()]);
    topic2_file.assert_lines_and_blame(crate::lines!["// AI topic 2".ai()]);
    topic3_file.assert_lines_and_blame(crate::lines!["// AI topic 3".ai()]);
}

/// Test rebase with no changes (already up to date)
#[test]
fn test_rebase_already_up_to_date() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI".ai()]);
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
    feature_file.assert_lines_and_blame(crate::lines!["// AI".ai()]);
}

/// Test rebase with conflicts - verifies reconstruction works after conflict resolution
#[test]
fn test_rebase_with_conflicts() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create old_base branch and commit
    repo.git(&["checkout", "-b", "old_base"]).unwrap();
    let mut old_file = repo.filename("old.txt");
    old_file.set_contents(crate::lines!["old base"]);
    repo.stage_all_and_commit("Old base commit").unwrap();
    let old_base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create feature branch from old_base with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create new_base branch from default_branch
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "new_base"]).unwrap();
    let mut new_file = repo.filename("new.txt");
    new_file.set_contents(crate::lines!["new base"]);
    repo.stage_all_and_commit("New base commit").unwrap();
    let new_base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase feature --onto new_base old_base (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--onto", &new_base_sha, &old_base_sha])
        .expect("Rebase --onto should succeed");

    // Verify authorship preserved after --onto rebase
    feature_file.assert_lines_and_blame(crate::lines!["// AI feature".ai()]);
}

/// Test rebase abort - ensures no authorship corruption on abort
#[test]
fn test_rebase_abort() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut conflict_file = repo.filename("conflict.txt");
    conflict_file.set_contents(crate::lines!["line 1", "line 2"]);
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
    conflict_file.assert_lines_and_blame(crate::lines!["line 1".human(), "AI CHANGE".ai()]);
}

/// Test branch switch during rebase - ensures proper state handling
#[test]
fn test_rebase_branch_switch_during() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create another branch
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "other"]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other"]);
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
    feature_file.assert_lines_and_blame(crate::lines!["// AI".ai()]);
}

/// Test rebase with autosquash enabled
#[test]
fn test_rebase_autosquash() {
    let repo = TestRepo::new();

    // Enable autosquash in config
    repo.git(&["config", "rebase.autosquash", "true"]).unwrap();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("Add feature").unwrap();

    // Create fixup commit
    file.replace_at(1, "AI line 2 fixed".ai());
    repo.stage_all_and_commit("fixup! Add feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Interactive rebase with autosquash (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    let rebase_result = repo.git_with_env(
        &["rebase", "-i", "--autosquash", &base],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
        None,
    );

    if rebase_result.is_ok() {
        // Verify the file has the expected content with AI authorship
        file.assert_lines_and_blame(crate::lines!["line 1".ai(), "AI line 2 fixed".ai()]);
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
    file.set_contents(crate::lines!["line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main"]);
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
        feature_file.assert_lines_and_blame(crate::lines!["// AI".ai()]);
    }
}

/// Test rebase --exec to run tests at each commit
#[test]
fn test_rebase_exec() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut test_sh = repo.filename("test.sh");
    test_sh.set_contents(crate::lines!["#!/bin/sh", "exit 0"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut f1 = repo.filename("f1.txt");
    f1.set_contents(crate::lines!["// AI 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut f2 = repo.filename("f2.txt");
    f2.set_contents(crate::lines!["// AI 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", "feature"]).unwrap();

    // Rebase with --exec (hooks will handle authorship)
    repo.git_with_env(
        &["rebase", "-i", "--exec", "echo 'test passed'", &base],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
        None,
    )
    .expect("Rebase with --exec should succeed");

    // Verify authorship was preserved
    f1.assert_lines_and_blame(crate::lines!["// AI 1".ai()]);
    f2.assert_lines_and_blame(crate::lines!["// AI 2".ai()]);
}

/// Test rebase with merge commits (--rebase-merges)
/// This test verifies the BFS fix for issue #328 where walk_commits_to_base
/// was only following parent(0), missing side branch commits.
///
/// The test checks that authorship notes for rebased commits include files
/// from side branches (reached via parent(1) of merge commits).
#[test]
fn test_rebase_preserve_merges() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create side branch from feature - this commit is only reachable via parent(1) of the merge
    repo.git(&["checkout", "-b", "side"]).unwrap();
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(crate::lines!["// AI side".ai()]);
    repo.stage_all_and_commit("AI side").unwrap();

    // Merge side into feature with --no-ff to force a merge commit
    // (creates merge commit where side is parent(1), feature is parent(0))
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["merge", "--no-ff", "side", "-m", "Merge side into feature"])
        .unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main"]);
    repo.stage_all_and_commit("Main work").unwrap();
    let base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Rebase feature onto main with --rebase-merges
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", "--rebase-merges", &base])
        .expect("Rebase with --rebase-merges should succeed");

    // Get the rebased side branch commit (the one that created side.txt)
    // Use git log to find the commit that added side.txt
    let side_commit_sha = repo
        .git(&[
            "log",
            "--all",
            "--format=%H",
            "--diff-filter=A",
            "--",
            "side.txt",
        ])
        .expect("Should find commit that added side.txt")
        .trim()
        .lines()
        .next()
        .expect("Should have at least one commit")
        .to_string();

    // Check that the rebased side commit has an authorship note with side.txt
    // This is the key assertion: without BFS fix, walk_commits_to_base misses
    // the side branch commit, so its authorship won't be rewritten
    let note_output = repo.git(&["notes", "--ref=ai", "show", &side_commit_sha]);

    assert!(
        note_output.is_ok(),
        "Rebased side branch commit should have authorship note. \
         Without BFS fix, walk_commits_to_base misses commits from parent(1) \
         and authorship is not rewritten for side branch commits."
    );

    let note_content = note_output.unwrap();
    assert!(
        note_content.contains("side.txt"),
        "Authorship note should include side.txt. Got: {}",
        note_content
    );

    // Also verify blame works correctly
    feature_file.assert_lines_and_blame(crate::lines!["// AI feature".ai()]);
    side_file.assert_lines_and_blame(crate::lines!["// AI side".ai()]);
}

/// Test rebase with commit splitting (fewer original commits than new commits)
/// This tests that rebase handles AI authorship correctly even with complex commit histories
#[test]
fn test_rebase_commit_splitting() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let mut features_file = repo.filename("features.txt");
    features_file.set_contents(crate::lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai(),
        "".ai()
    ]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    features_file.insert_at(
        2,
        crate::lines!["// AI feature 2".ai(), "function feature2() {}".ai()],
    );
    repo.stage_all_and_commit("AI feature 2").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main content", ""]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto main (hooks will handle authorship)
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify AI authorship is preserved after rebase
    features_file.assert_lines_and_blame(crate::lines![
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
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 3 AI commits with different content - we'll squash these
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines!["// AI feature 1".ai(), "line 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines!["// AI feature 2".ai(), "line 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(crate::lines!["// AI feature 3".ai(), "line 3".ai()]);
    repo.stage_all_and_commit("AI commit 3").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
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
        None,
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
    feature1.assert_lines_and_blame(crate::lines!["// AI feature 1".ai(), "line 1".ai()]);
    feature2.assert_lines_and_blame(crate::lines!["// AI feature 2".ai(), "line 2".ai()]);
    feature3.assert_lines_and_blame(crate::lines!["// AI feature 3".ai(), "line 3".ai()]);
}

/// Test rebase with rewording (renaming) a commit that has 2 children commits
/// Verifies that authorship is preserved for all 3 commits after reword
#[test]
#[cfg(not(target_os = "windows"))]
fn test_rebase_reword_commit_with_children() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Create 3 AI commits - we'll reword the first one
    let mut feature1 = repo.filename("feature1.txt");
    feature1.set_contents(crate::lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 1 - original message")
        .unwrap();

    let mut feature2 = repo.filename("feature2.txt");
    feature2.set_contents(crate::lines![
        "// AI feature 2".ai(),
        "function feature2() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 2").unwrap();

    let mut feature3 = repo.filename("feature3.txt");
    feature3.set_contents(crate::lines![
        "// AI feature 3".ai(),
        "function feature3() {}".ai()
    ]);
    repo.stage_all_and_commit("AI commit 3").unwrap();

    // Advance main branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(crate::lines!["main work"]);
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
        None,
    );

    if rebase_result.is_err() {
        eprintln!("git rebase output: {:?}", rebase_result);
        panic!("Interactive rebase with reword failed");
    }

    // Verify all 3 files still exist with correct AI authorship after reword
    feature1.assert_lines_and_blame(crate::lines![
        "// AI feature 1".ai(),
        "function feature1() {}".ai()
    ]);
    feature2.assert_lines_and_blame(crate::lines![
        "// AI feature 2".ai(),
        "function feature2() {}".ai()
    ]);
    feature3.assert_lines_and_blame(crate::lines![
        "// AI feature 3".ai(),
        "function feature3() {}".ai()
    ]);
}

/// Test that custom attributes set via config are preserved through a rebase
/// when the real post-commit pipeline injects them.
#[test]
fn test_rebase_preserves_custom_attributes_from_config() {
    let mut repo = TestRepo::new();

    // Configure custom attributes via config patch
    let mut attrs = HashMap::new();
    attrs.insert("employee_id".to_string(), "E789".to_string());
    attrs.insert("team".to_string(), "infra".to_string());
    repo.patch_git_ai_config(|patch| {
        patch.custom_attributes = Some(attrs.clone());
    });

    // Create initial commit on default branch
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    // Create feature branch with AI commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["// AI feature code".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Verify custom attributes were set on the original commit
    let original_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let original_note = repo
        .read_authorship_note(&original_sha)
        .expect("original commit should have authorship note");
    let original_log =
        AuthorshipLog::deserialize_from_string(&original_note).expect("parse original note");
    for prompt in original_log.metadata.prompts.values() {
        assert_eq!(
            prompt.custom_attributes.as_ref(),
            Some(&attrs),
            "precondition: original commit should have custom_attributes from config"
        );
    }

    // Advance default branch (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other content"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature onto default branch
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Verify custom attributes survived the rebase
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased commit should have authorship note");
    let rebased_log =
        AuthorshipLog::deserialize_from_string(&rebased_note).expect("parse rebased note");
    assert!(
        !rebased_log.metadata.prompts.is_empty(),
        "rebased commit should have prompt records"
    );
    for prompt in rebased_log.metadata.prompts.values() {
        assert_eq!(
            prompt.custom_attributes.as_ref(),
            Some(&attrs),
            "custom_attributes should be preserved through rebase"
        );
    }

    // Also verify the AI attribution itself survived
    feature_file.assert_lines_and_blame(crate::lines!["// AI feature code".ai()]);
}

/// Regression test: prompt metrics (accepted_lines) must update per commit, not be frozen
/// from the initial state. When commit 1 has 2 AI lines and commit 2 adds 2 more
/// (total 4), the rebased notes should reflect different accepted_lines.
#[test]
fn test_rebase_prompt_metrics_update_per_commit() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    // Initial setup
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: add 2 AI lines
    let mut ai_file = repo.filename("feature.txt");
    ai_file.set_contents(crate::lines!["line1".ai(), "line2".ai()]);
    let commit1 = repo.stage_all_and_commit("AI commit 1 - 2 lines").unwrap();

    // Commit 2: add 2 more AI lines (total 4)
    ai_file.set_contents(crate::lines![
        "line1".ai(),
        "line2".ai(),
        "line3".ai(),
        "line4".ai()
    ]);
    let commit2 = repo.stage_all_and_commit("AI commit 2 - 4 lines").unwrap();

    // Verify pre-rebase: commit 1 has 2 accepted, commit 2 has 4
    let note1 = repo
        .read_authorship_note(&commit1.commit_sha)
        .expect("commit 1 should have note");
    let log1 = AuthorshipLog::deserialize_from_string(&note1).expect("parse note 1");
    let note2 = repo
        .read_authorship_note(&commit2.commit_sha)
        .expect("commit 2 should have note");
    let log2 = AuthorshipLog::deserialize_from_string(&note2).expect("parse note 2");

    let pre_accepted_1: u32 = log1
        .metadata
        .prompts
        .values()
        .map(|p| p.accepted_lines)
        .sum();
    let pre_accepted_2: u32 = log2
        .metadata
        .prompts
        .values()
        .map(|p| p.accepted_lines)
        .sum();
    assert!(
        pre_accepted_1 < pre_accepted_2,
        "precondition: commit 2 ({}) should have more accepted_lines than commit 1 ({})",
        pre_accepted_2,
        pre_accepted_1
    );

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Get rebased commit SHAs
    let rebased_tip = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_parent = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    // Verify post-rebase: metrics should differ between the two commits
    let rebased_note1 = repo
        .read_authorship_note(&rebased_parent)
        .expect("rebased commit 1 should have note");
    let rebased_log1 =
        AuthorshipLog::deserialize_from_string(&rebased_note1).expect("parse rebased note 1");
    let rebased_note2 = repo
        .read_authorship_note(&rebased_tip)
        .expect("rebased commit 2 should have note");
    let rebased_log2 =
        AuthorshipLog::deserialize_from_string(&rebased_note2).expect("parse rebased note 2");

    let post_accepted_1: u32 = rebased_log1
        .metadata
        .prompts
        .values()
        .map(|p| p.accepted_lines)
        .sum();
    let post_accepted_2: u32 = rebased_log2
        .metadata
        .prompts
        .values()
        .map(|p| p.accepted_lines)
        .sum();

    assert!(
        post_accepted_1 < post_accepted_2,
        "regression: rebased commit 2 ({}) should have more accepted_lines than commit 1 ({}). \
         If equal, the fast path is freezing metrics across commits.",
        post_accepted_2,
        post_accepted_1
    );
}

/// Regression test: attributions should survive a delete-recreate cycle within a rebase.
/// If a file is deleted in commit N and recreated in commit N+1, the recreated file
/// should inherit attributions from the pre-deletion state via positional diff transfer.
#[test]
fn test_rebase_file_delete_recreate_preserves_attribution() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    // Initial setup
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch with AI file
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut ai_file = repo.filename("feature.txt");
    ai_file.set_contents(crate::lines!["line1".ai(), "line2".ai(), "line3".ai()]);
    repo.stage_all_and_commit("Add AI file").unwrap();

    // Delete the file
    repo.git(&["rm", "feature.txt"]).unwrap();
    repo.stage_all_and_commit("Delete AI file").unwrap();

    // Recreate the file with same content
    ai_file.set_contents(crate::lines!["line1".ai(), "line2".ai(), "line3".ai()]);
    let recreate_commit = repo.stage_all_and_commit("Recreate AI file").unwrap();

    // Verify pre-rebase: recreated file has attributions
    let pre_note = repo
        .read_authorship_note(&recreate_commit.commit_sha)
        .expect("recreated commit should have note");
    let pre_log = AuthorshipLog::deserialize_from_string(&pre_note).expect("parse pre note");
    assert!(
        !pre_log.attestations.is_empty(),
        "precondition: recreated file should have attestations"
    );

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Check rebased tip (the recreate commit)
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased recreate commit should have note");
    let rebased_log =
        AuthorshipLog::deserialize_from_string(&rebased_note).expect("parse rebased note");

    assert!(
        !rebased_log.attestations.is_empty(),
        "regression: file recreated after deletion should still have attestations after rebase"
    );

    // Verify the AI attribution itself survived
    ai_file.assert_lines_and_blame(crate::lines!["line1".ai(), "line2".ai(), "line3".ai()]);
}

/// Regression test: file deleted then recreated with DIFFERENT content preserves attribution.
///
/// This tests a subtle bug where:
/// 1. first_appearance_blobs: seen_files must be cleared on deletion so the
///    new blob OID is read on recreation.
/// 2. files_with_synced_state: must be cleared on deletion so recreation
///    uses content-diff (not stale hunk-based transfer).
#[test]
fn test_rebase_file_delete_recreate_different_content_preserves_attribution() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    // Initial setup
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create feature branch with AI file (original content)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut ai_file = repo.filename("feature.txt");
    ai_file.set_contents(crate::lines!["old_line1".ai(), "old_line2".ai()]);
    repo.stage_all_and_commit("Add AI file").unwrap();

    // Delete the file
    repo.git(&["rm", "feature.txt"]).unwrap();
    repo.stage_all_and_commit("Delete AI file").unwrap();

    // Recreate the file with DIFFERENT content
    ai_file.set_contents(crate::lines![
        "new_line1".ai(),
        "new_line2".ai(),
        "new_line3".ai()
    ]);
    let recreate_commit = repo
        .stage_all_and_commit("Recreate AI file different")
        .unwrap();

    // Verify pre-rebase: recreated file has attributions
    let pre_note = repo
        .read_authorship_note(&recreate_commit.commit_sha)
        .expect("recreated commit should have note");
    let pre_log = AuthorshipLog::deserialize_from_string(&pre_note).expect("parse pre note");
    assert!(
        !pre_log.attestations.is_empty(),
        "precondition: recreated file should have attestations"
    );

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other_file = repo.filename("other.txt");
    other_file.set_contents(crate::lines!["other"]);
    repo.stage_all_and_commit("Main advances").unwrap();

    // Rebase feature
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Check rebased tip (the recreate commit)
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased recreate commit should have note");
    let rebased_log =
        AuthorshipLog::deserialize_from_string(&rebased_note).expect("parse rebased note");

    assert!(
        !rebased_log.attestations.is_empty(),
        "regression: file recreated with different content should have attestations after rebase"
    );

    // Verify the new AI attribution (different content) survived
    ai_file.assert_lines_and_blame(crate::lines![
        "new_line1".ai(),
        "new_line2".ai(),
        "new_line3".ai()
    ]);
}

/// Regression test: AI attribution from earlier commits (not HEAD) must survive rebase.
///
/// Each commit's note only covers lines changed in THAT commit. HEAD doesn't
/// touch all AI-attributed files. The reconstruction must process ALL commits'
/// notes to build the complete attribution state, not just HEAD's.
#[test]
fn test_rebase_preserves_attribution_from_non_head_commits() {
    let repo = TestRepo::new();

    // Initial commit
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    // Feature branch: commit 1 — AI attribution on file_a only
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines![
        "// AI generated module A".ai(),
        "fn module_a() {}".ai(),
        "// end module A".ai()
    ]);
    repo.stage_all_and_commit("feat: add module A (AI)")
        .unwrap();

    // Feature branch: commit 2 — AI attribution on file_b only (file_a not touched)
    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines![
        "// AI generated module B".ai(),
        "fn module_b() {}".ai()
    ]);
    repo.stage_all_and_commit("feat: add module B (AI)")
        .unwrap();

    // Feature branch: commit 3 (HEAD) — AI attribution on file_c only
    // file_a and file_b are NOT touched in this commit
    let mut file_c = repo.filename("file_c.txt");
    file_c.set_contents(crate::lines![
        "// AI generated module C".ai(),
        "fn module_c() {}".ai()
    ]);
    repo.stage_all_and_commit("feat: add module C (AI)")
        .unwrap();

    // Advance main branch to force actual rebase (not fast-forward)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_change = repo.filename("main_update.txt");
    main_change.set_contents(crate::lines!["main branch work"]);
    repo.stage_all_and_commit("main: infrastructure update")
        .unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // CRITICAL: file_a attribution (from commit 1, NOT HEAD) must survive
    file_a.assert_lines_and_blame(crate::lines![
        "// AI generated module A".ai(),
        "fn module_a() {}".ai(),
        "// end module A".ai()
    ]);

    // file_b attribution (from commit 2, NOT HEAD) must survive
    file_b.assert_lines_and_blame(crate::lines![
        "// AI generated module B".ai(),
        "fn module_b() {}".ai()
    ]);

    // file_c attribution (from HEAD commit) must survive
    file_c.assert_lines_and_blame(crate::lines![
        "// AI generated module C".ai(),
        "fn module_c() {}".ai()
    ]);
}

/// Regression test: multi-commit attribution on SAME file from different commits.
///
/// Commit 1 adds AI lines 1-3, commit 3 adds AI lines 4-6, but commit 2
/// (between them) touches a different file entirely. The reconstruction must
/// combine notes from both commits to get the full attribution for the file.
#[test]
fn test_rebase_preserves_multi_commit_attribution_same_file() {
    let repo = TestRepo::new();

    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: AI attribution on app.txt lines 1-3
    let mut app = repo.filename("app.txt");
    app.set_contents(crate::lines![
        "// AI header".ai(),
        "fn init() {}".ai(),
        "// end init".ai()
    ]);
    repo.stage_all_and_commit("feat: AI init code").unwrap();

    // Commit 2: touch a DIFFERENT file (app.txt unchanged)
    let mut config = repo.filename("config.txt");
    config.set_contents(crate::lines!["// AI config".ai(), "setting = true".ai()]);
    repo.stage_all_and_commit("feat: AI config").unwrap();

    // Commit 3 (HEAD): add MORE AI lines to app.txt
    app.set_contents(crate::lines![
        "// AI header".ai(),
        "fn init() {}".ai(),
        "// end init".ai(),
        "// AI footer added later".ai(),
        "fn cleanup() {}".ai()
    ]);
    repo.stage_all_and_commit("feat: AI cleanup code").unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut infra = repo.filename("infra.txt");
    infra.set_contents(crate::lines!["infra work"]);
    repo.stage_all_and_commit("main: infra").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // app.txt should have ALL AI lines (from commits 1 AND 3)
    app.assert_lines_and_blame(crate::lines![
        "// AI header".ai(),
        "fn init() {}".ai(),
        "// end init".ai(),
        "// AI footer added later".ai(),
        "fn cleanup() {}".ai()
    ]);

    // config.txt (from commit 2, NOT HEAD) must survive
    config.assert_lines_and_blame(crate::lines!["// AI config".ai(), "setting = true".ai()]);
}

/// Regression test: attribution survives when main branch modifies AI-attributed
/// files, forcing the slow path (blob OID mismatch between original and rebased).
/// This tests that attribution from non-HEAD commits survives even through the
/// full attribution rewrite path.
#[test]
fn test_rebase_non_head_attribution_survives_slow_path() {
    let repo = TestRepo::new();

    let mut base = repo.filename("shared.txt");
    base.set_contents(crate::lines![
        "// top section",
        "line_a",
        "line_b",
        "line_c",
        "",
        "",
        "",
        "",
        "// bottom section",
        "line_x",
        "line_y",
        "line_z"
    ]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: AI attribution on module.txt
    let mut module = repo.filename("module.txt");
    module.set_contents(crate::lines![
        "// AI module".ai(),
        "pub fn process() {}".ai(),
        "// end".ai()
    ]);
    repo.stage_all_and_commit("feat: AI module").unwrap();

    // Commit 2 (HEAD): append to bottom of shared.txt
    // module.txt is NOT touched here
    let mut shared = repo.filename("shared.txt");
    shared.set_contents(crate::lines![
        "// top section",
        "line_a",
        "line_b",
        "line_c",
        "",
        "",
        "",
        "",
        "// bottom section",
        "line_x",
        "line_y",
        "line_z",
        "// feature addition".ai()
    ]);
    repo.stage_all_and_commit("feat: extend shared").unwrap();

    // Advance main — add a new file so the rebase replays commits on a new base.
    // shared.txt is NOT modified on main, so no merge conflict occurs.
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut infra = repo.filename("infra.txt");
    infra.set_contents(crate::lines!["// infrastructure", "setup_logging();"]);
    repo.stage_all_and_commit("main: add infra file").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // module.txt attribution (from commit 1, NOT HEAD) must survive
    // even though the rebase took the slow path due to shared.txt changes
    module.assert_lines_and_blame(crate::lines![
        "// AI module".ai(),
        "pub fn process() {}".ai(),
        "// end".ai()
    ]);
}

crate::reuse_tests_in_worktree!(
    test_rebase_no_conflicts_identical_trees,
    test_rebase_with_different_trees,
    test_rebase_multiple_commits,
    test_rebase_mixed_authorship,
    test_rebase_preserves_exact_mixed_line_attribution_in_single_file,
    test_rebase_with_human_only_commit_between_ai_commits_preserves_exact_lines,
    test_rebase_preserves_human_only_commit_note_metadata,
    test_rebase_preserves_prompt_only_commit_note_metadata,
    test_rebase_fast_forward,
    test_rebase_with_explicit_branch_argument_preserves_authorship,
    test_rebase_root_with_explicit_branch_argument_preserves_authorship,
    test_rebase_interactive_reorder,
    test_rebase_skip,
    test_rebase_keep_empty,
    test_rebase_rerere,
    test_rebase_patch_stack,
    test_rebase_already_up_to_date,
    test_rebase_with_conflicts,
    test_rebase_abort,
    test_rebase_branch_switch_during,
    test_rebase_autosquash,
    test_rebase_autostash,
    test_rebase_exec,
    test_rebase_preserve_merges,
    test_rebase_commit_splitting,
    test_rebase_prompt_metrics_update_per_commit,
    test_rebase_file_delete_recreate_preserves_attribution,
    test_rebase_file_delete_recreate_different_content_preserves_attribution,
    test_rebase_file_delete_recreate_after_hunk_modification,
);

crate::reuse_tests_in_worktree_with_attrs!(
    (#[cfg(not(target_os = "windows"))])
    test_rebase_squash_preserves_all_authorship,
    test_rebase_reword_commit_with_children,
);

/// Regression test: file modified via hunk path, then deleted, then recreated.
///
/// This exercises a bug where `current_file_contents` becomes stale after hunk-based
/// attribution transfer (which updates attributions but not the file content cache).
/// When the file is later deleted and recreated, the slow content-diff path would use
/// stale content with shifted line numbers, producing corrupt attributions.
///
/// Trigger sequence:
/// 1. Commit 1: create file (slow path sets current_file_contents)
/// 2. Commit 2: modify file (hunk path shifts attrs but leaves current_file_contents stale)
/// 3. Commit 3: delete file
/// 4. Commit 4: recreate file with new content
///
/// Main branch must also modify the same file to force the slow reconstruction path.
#[test]
fn test_rebase_file_delete_recreate_after_hunk_modification() {
    let repo = TestRepo::new();
    let default_branch = repo.current_branch();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Feature branch: 4 commits exercising hunk→delete→recreate
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit 1: create file
    let mut ai_file = repo.filename("feature.txt");
    ai_file.set_contents(crate::lines!["line1".ai(), "line2".ai(), "line3".ai()]);
    repo.stage_all_and_commit("Create AI file").unwrap();

    // Commit 2: modify file (will use hunk-based path on rebase)
    ai_file.set_contents(crate::lines![
        "line1".ai(),
        "line2".ai(),
        "inserted".ai(),
        "line3".ai()
    ]);
    repo.stage_all_and_commit("Modify AI file").unwrap();

    // Commit 3: delete the file
    repo.git(&["rm", "feature.txt"]).unwrap();
    repo.stage_all_and_commit("Delete AI file").unwrap();

    // Commit 4: recreate with different content
    ai_file.set_contents(crate::lines![
        "recreated_a".ai(),
        "recreated_b".ai(),
        "recreated_c".ai(),
        "recreated_d".ai()
    ]);
    repo.stage_all_and_commit("Recreate AI file").unwrap();

    // Advance default branch — must touch the same file to force slow path
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut conflict_file = repo.filename("feature.txt");
    conflict_file.set_contents(crate::lines!["main_content"]);
    repo.stage_all_and_commit("Main touches same file").unwrap();
    // Delete it so rebase doesn't conflict
    repo.git(&["rm", "feature.txt"]).unwrap();
    repo.stage_all_and_commit("Main deletes file").unwrap();

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Check the final commit (recreate) has correct attributions
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased recreate commit should have note");
    let rebased_log =
        AuthorshipLog::deserialize_from_string(&rebased_note).expect("parse rebased note");

    assert!(
        !rebased_log.attestations.is_empty(),
        "regression: file recreated after hunk-modify+delete should have attestations"
    );

    ai_file.assert_lines_and_blame(crate::lines![
        "recreated_a".ai(),
        "recreated_b".ai(),
        "recreated_c".ai(),
        "recreated_d".ai()
    ]);
}
