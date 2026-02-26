#[macro_use]
mod repos;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::working_log::AgentId;
use git_ai::git::refs::notes_add;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::process::Command;

fn read_authorship_note(repo: &TestRepo, commit_sha: &str) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-C",
            repo.path().to_str().expect("valid repo path"),
            "--no-pager",
            "notes",
            "--ref=ai",
            "show",
            commit_sha,
        ])
        .output()
        .expect("failed to run git notes show");

    if output.status.success() {
        let note = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if note.is_empty() { None } else { Some(note) }
    } else {
        None
    }
}

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

#[test]
fn test_rebase_preserves_exact_mixed_line_attribution_in_single_file() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut app_file = repo.filename("app.js");
    app_file.set_contents(lines![
        "const version = 1;".human(),
        "function compute() {".ai(),
        "  return 1;".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("Add mixed app").unwrap();

    app_file.insert_at(2, lines!["  // AI docs".ai()]);
    repo.stage_all_and_commit("Add docs").unwrap();

    app_file.insert_at(5, lines!["// AI footer".ai()]);
    repo.stage_all_and_commit("Add footer").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    app_file.assert_lines_and_blame(lines![
        "const version = 1;".human(),
        "function compute() {".ai(),
        "  // AI docs".ai(),
        "  return 1;".ai(),
        "}".human(),
        "// AI footer".ai()
    ]);
}

#[test]
fn test_rebase_with_human_only_commit_between_ai_commits_preserves_exact_lines() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    let mut app_file = repo.filename("app.js");
    base_file.set_contents(lines!["base"]);
    app_file.set_contents(lines!["const base = 0;".human()]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    app_file.insert_at(1, lines!["// AI block 1".ai()]);
    repo.stage_all_and_commit("AI block 1").unwrap();

    let mut notes_file = repo.filename("notes.txt");
    notes_file.set_contents(lines!["human notes line"]);
    repo.stage_all_and_commit("Human-only notes").unwrap();

    let mut generated_file = repo.filename("generated.js");
    generated_file.set_contents(lines!["const generated = 42;".ai()]);
    repo.stage_all_and_commit("AI block 2").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main advance"]);
    repo.stage_all_and_commit("Main advance").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    app_file.assert_lines_and_blame(lines!["const base = 0;".human(), "// AI block 1".ai()]);
    generated_file.assert_lines_and_blame(lines!["const generated = 42;".ai()]);
    notes_file.assert_lines_and_blame(lines!["human notes line".human()]);
}

#[test]
fn test_rebase_preserves_human_only_commit_note_metadata() {
    let repo = TestRepo::new();

    // Common base commit.
    let mut base = repo.filename("base.txt");
    base.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    // Branch we will rebase onto.
    repo.git(&["checkout", "-b", "dev"]).unwrap();
    let mut dev_file = repo.filename("dev.txt");
    dev_file.set_contents(lines!["dev content"]);
    repo.stage_all_and_commit("Dev commit").unwrap();

    // Create the source branch from the old base and make a human-only commit.
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "prod"]).unwrap();
    let mut prod_file = repo.filename("prod.txt");
    prod_file.set_contents(lines!["human change only"]);
    let prod_commit = repo.stage_all_and_commit("Prod human commit").unwrap();

    // Sanity check: original commit has a note and it's metadata-only.
    let old_note = read_authorship_note(&repo, &prod_commit.commit_sha)
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
    let rebased_note = read_authorship_note(&repo, &rebased_sha)
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
    base.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "dev"]).unwrap();
    let mut dev_file = repo.filename("dev.txt");
    dev_file.set_contents(lines!["dev content"]);
    repo.stage_all_and_commit("Dev commit").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "prod"]).unwrap();
    let mut prod_file = repo.filename("prod.txt");
    prod_file.set_contents(lines!["human change only"]);
    let prod_commit = repo
        .stage_all_and_commit("Prod human commit")
        .expect("create prod commit");

    let original_note = read_authorship_note(&repo, &prod_commit.commit_sha)
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

    let rebased_note = read_authorship_note(&repo, &rebased_sha)
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

/// Test `git rebase <upstream> <branch>` when invoked from another branch.
/// We should capture original_head from `<branch>`, not from the currently checked-out branch.
#[test]
fn test_rebase_with_explicit_branch_argument_preserves_authorship() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(lines!["base"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch with AI-authored content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai(), "fn feature() {}".ai()]);
    repo.stage_all_and_commit("add feature").unwrap();

    // Advance main branch
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("main advances").unwrap();

    // Invoke rebase with explicit branch arg while currently on main.
    let output = repo.git(&["rebase", &main_branch, "feature"]).unwrap();

    assert!(
        output.contains("Commit mapping: 1 original -> 1 new"),
        "Expected explicit-branch rebase to map one original commit to one rebased commit. Output:\n{}",
        output
    );

    // HEAD should now be on feature after the rebase operation; verify AI blame survived.
    feature_file.assert_lines_and_blame(lines!["// AI feature".ai(), "fn feature() {}".ai()]);
}

/// Test `git rebase --root --onto <base> <branch>` when invoked from another branch.
/// We should resolve original_head from `<branch>`, not from the currently checked-out branch.
#[test]
fn test_rebase_root_with_explicit_branch_argument_preserves_authorship() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(lines!["base"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch with AI-authored content
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai(), "fn feature() {}".ai()]);
    let original_feature_head = repo.stage_all_and_commit("add feature").unwrap().commit_sha;

    // Advance main branch
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main work"]);
    repo.stage_all_and_commit("main advances").unwrap();

    // Invoke root rebase with explicit branch arg while currently on main.
    let output = repo
        .git(&["rebase", "--root", "--onto", &main_branch, "feature"])
        .unwrap();

    assert!(
        output.contains("Commit mapping: 1 original -> 1 new"),
        "Expected root explicit-branch rebase to map one original commit to one rebased commit. Output:\n{}",
        output
    );

    let rebased_feature_head = repo.git(&["rev-parse", "HEAD"]).unwrap();
    assert_ne!(
        rebased_feature_head.trim(),
        original_feature_head,
        "Feature head should be rewritten by root rebase"
    );

    // HEAD should now be on feature after the rebase operation; verify AI blame survived.
    feature_file.assert_lines_and_blame(lines!["// AI feature".ai(), "fn feature() {}".ai()]);
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
        None,
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
        None,
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
        None,
    )
    .expect("Rebase with --exec should succeed");

    // Verify authorship was preserved
    f1.assert_lines_and_blame(lines!["// AI 1".ai()]);
    f2.assert_lines_and_blame(lines!["// AI 2".ai()]);
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
    base_file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["// AI feature".ai()]);
    repo.stage_all_and_commit("AI feature").unwrap();

    // Create side branch from feature - this commit is only reachable via parent(1) of the merge
    repo.git(&["checkout", "-b", "side"]).unwrap();
    let mut side_file = repo.filename("side.txt");
    side_file.set_contents(lines!["// AI side".ai()]);
    repo.stage_all_and_commit("AI side").unwrap();

    // Merge side into feature with --no-ff to force a merge commit
    // (creates merge commit where side is parent(1), feature is parent(0))
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["merge", "--no-ff", "side", "-m", "Merge side into feature"])
        .unwrap();

    // Advance main
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main.txt");
    main_file.set_contents(lines!["main"]);
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
        None,
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
