use crate::repos::test_repo::TestRepo;
use git_ai::ci::ci_context::{CiContext, CiEvent, CiRunOptions, CiRunResult};
use git_ai::git::repository as GitAiRepository;
use std::fs;

fn direct_test_repo() -> TestRepo {
    TestRepo::new()
}

/// Test that single-parent squash merges work even when the parent is not reachable
/// from the base ref (partial clone scenario). This is the core fix from PR #918.
#[test]
fn test_squash_merge_single_parent_not_on_base_ref() {
    let repo = direct_test_repo();
    let file_path = repo.path().join("file.txt");

    // Create initial commit on main
    fs::write(&file_path, "init").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "init"]).unwrap();
    let init_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create a feature commit (dangling, not on any branch)
    fs::write(&file_path, "feature work").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    let tree_id = {
        let git_repo = git2::Repository::open(repo.path()).unwrap();
        let mut index = git_repo.index().unwrap();
        index.write_tree().unwrap()
    };

    let feature_oid = {
        let git_repo = git2::Repository::open(repo.path()).unwrap();
        let sig = git_repo.signature().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let init_commit = git_repo
            .find_commit(git2::Oid::from_str(&init_sha).unwrap())
            .unwrap();
        git_repo
            .commit(None, &sig, &sig, "feature commit", &tree, &[&init_commit])
            .unwrap()
    };
    let feature_sha = feature_oid.to_string();

    // Advance main branch
    fs::write(&file_path, "main advance").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "advance main"]).unwrap();
    let _adv_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create squash merge commit (single parent = adv_sha, tree = feature work)
    fs::write(&file_path, "feature work").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "squash feature"]).unwrap();
    let squash_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create a ref pointing to the feature commit (simulating worker base ref)
    repo.git_og(&["update-ref", "refs/worker/pr/test/base", &feature_sha])
        .unwrap();

    // Set up CI event
    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let event = CiEvent::Merge {
        merge_commit_sha: squash_sha.clone(),
        head_ref: "feature".to_string(),
        head_sha: feature_sha.clone(),
        base_ref: "refs/worker/pr/test/base".to_string(),
        base_sha: feature_sha.clone(),
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    let result = ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
        skip_push: false,
    });

    // Should not fail with "No parent of commit" error
    assert!(
        !matches!(&result, Err(e) if e.to_string().contains("No parent of commit")),
        "Should not fail with parent_on_refname error, got: {:?}",
        result
    );
}

/// Test single-commit rebase where the parent IS reachable from base ref.
/// This is the happy path - both parent(0) shortcut and parent_on_refname should work.
#[test]
fn test_single_commit_rebase_parent_on_base_ref() {
    let repo = direct_test_repo();

    // Initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["init"]);
    let init_sha = repo.stage_all_and_commit("init").unwrap().commit_sha;
    let default_branch = repo.current_branch();

    // Create feature branch with 1 commit
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(crate::lines!["feature work"]);
    let feature_sha = repo
        .stage_all_and_commit("feature commit")
        .unwrap()
        .commit_sha;

    // Advance default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main2.txt");
    main_file.set_contents(crate::lines!["main work"]);
    repo.stage_all_and_commit("advance main").unwrap();

    // Rebase feature onto default branch
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Merge the rebased commit into default branch
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "feature", "-m", "merge feature"])
        .unwrap();
    let merge_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Set up base ref pointing at init (on default branch's ancestry)
    repo.git_og(&["update-ref", "refs/worker/pr/test/base", &init_sha])
        .unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let event = CiEvent::Merge {
        merge_commit_sha: merge_sha.clone(),
        head_ref: "feature".to_string(),
        head_sha: feature_sha.clone(),
        base_ref: "refs/worker/pr/test/base".to_string(),
        base_sha: init_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    let result = ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
        skip_push: false,
    });

    assert!(
        !matches!(&result, Err(e) if e.to_string().contains("No parent of commit")),
        "Single-commit rebase with reachable parent should not fail, got: {:?}",
        result
    );
}

/// Test multi-commit PR squashed to 1 merge commit (single parent).
/// Verifies the squash path handles multiple original commits correctly.
#[test]
fn test_multi_commit_squash_merge_single_parent() {
    let repo = direct_test_repo();

    // Initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["init"]);
    let init_sha = repo.stage_all_and_commit("init").unwrap().commit_sha;
    let default_branch = repo.current_branch();

    // Create feature branch with 3 commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut a_file = repo.filename("a.txt");
    a_file.set_contents(crate::lines!["aaa"]);
    repo.stage_all_and_commit("feature commit 1").unwrap();

    let mut b_file = repo.filename("b.txt");
    b_file.set_contents(crate::lines!["bbb"]);
    repo.stage_all_and_commit("feature commit 2").unwrap();

    let mut c_file = repo.filename("c.txt");
    c_file.set_contents(crate::lines!["ccc"]);
    let feature_head_sha = repo
        .stage_all_and_commit("feature commit 3")
        .unwrap()
        .commit_sha;

    // Advance default branch independently
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_file = repo.filename("main2.txt");
    main_file.set_contents(crate::lines!["main work"]);
    repo.stage_all_and_commit("advance main").unwrap();

    // Squash merge feature (produces single-parent commit)
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let merge_sha = repo
        .stage_all_and_commit("squash feature")
        .unwrap()
        .commit_sha;

    // Verify it's actually a single-parent commit
    let git_repo = git2::Repository::open(repo.path()).unwrap();
    let merge_commit = git_repo
        .find_commit(git2::Oid::from_str(&merge_sha).unwrap())
        .unwrap();
    assert_eq!(
        merge_commit.parent_count(),
        1,
        "Squash merge should have exactly 1 parent"
    );

    // Base ref points to init commit
    repo.git_og(&["update-ref", "refs/worker/pr/test/base", &init_sha])
        .unwrap();

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let event = CiEvent::Merge {
        merge_commit_sha: merge_sha.clone(),
        head_ref: "feature".to_string(),
        head_sha: feature_head_sha.clone(),
        base_ref: "refs/worker/pr/test/base".to_string(),
        base_sha: init_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    let result = ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
        skip_push: false,
    });

    assert!(
        !matches!(&result, Err(e) if e.to_string().contains("No parent of commit")),
        "Multi-commit squash merge should not fail with parent_on_refname error, got: {:?}",
        result
    );
}

/// Test that true merge commits (2 parents) are detected as simple merges
/// and skipped entirely - verifying the multi-parent path is unchanged.
#[test]
fn test_regular_two_parent_merge_skipped() {
    let repo = direct_test_repo();
    let file_path = repo.path().join("file.txt");

    // Create initial commit
    fs::write(&file_path, "init").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "init"]).unwrap();
    let init_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create feature commit (diverges from init)
    let feature_oid = {
        let git_repo = git2::Repository::open(repo.path()).unwrap();
        let sig = git_repo.signature().unwrap();

        fs::write(&file_path, "feature work").unwrap();
        let mut index = git_repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let init_commit = git_repo
            .find_commit(git2::Oid::from_str(&init_sha).unwrap())
            .unwrap();

        git_repo
            .commit(None, &sig, &sig, "feature commit", &tree, &[&init_commit])
            .unwrap()
    };
    let feature_sha = feature_oid.to_string();

    // Advance default branch
    fs::write(&file_path, "main advance").unwrap();
    repo.git_og(&["add", "file.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "advance main"]).unwrap();
    let adv_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Create true merge commit (2 parents) using low-level API
    let merge_sha = {
        let git_repo = git2::Repository::open(repo.path()).unwrap();
        let sig = git_repo.signature().unwrap();

        fs::write(&file_path, "merged").unwrap();
        let mut index = git_repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();

        let adv_commit = git_repo
            .find_commit(git2::Oid::from_str(&adv_sha).unwrap())
            .unwrap();
        let feature_commit = git_repo.find_commit(feature_oid).unwrap();

        let merge_oid = git_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Merge feature",
                &tree,
                &[&adv_commit, &feature_commit],
            )
            .unwrap();
        merge_oid.to_string()
    };

    // Verify it's a 2-parent commit
    let git_repo = git2::Repository::open(repo.path()).unwrap();
    let merge_commit = git_repo
        .find_commit(git2::Oid::from_str(&merge_sha).unwrap())
        .unwrap();
    assert_eq!(
        merge_commit.parent_count(),
        2,
        "Regular merge should have 2 parents"
    );

    let git_ai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("Failed to find repository");

    let event = CiEvent::Merge {
        merge_commit_sha: merge_sha.clone(),
        head_ref: "feature".to_string(),
        head_sha: feature_sha.clone(),
        base_ref: "refs/heads/master".to_string(),
        base_sha: adv_sha,
    };

    let ctx = CiContext::with_repository(git_ai_repo, event);
    let result = ctx.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
        skip_push: false,
    });

    assert!(
        matches!(&result, Ok(CiRunResult::SkippedSimpleMerge)),
        "2-parent merge should be skipped as simple merge, got: {:?}",
        result
    );
}
