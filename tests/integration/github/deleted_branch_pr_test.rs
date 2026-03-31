use super::github_test_harness::{GitHubTestRepo, MergeStrategy};
use crate::repos::test_file::ExpectedLineExt;

// GitHub deletes the branch after the PR is merged, so we need to test that we can still access the PR commits using PR refs.

#[test]
#[ignore] // Ignored by default - run with `cargo test --test github_integration -- --ignored`
fn test_squash_merge_with_deleted_branch() {
    let test_repo = match GitHubTestRepo::new("test_squash_merge_with_deleted_branch") {
        Some(repo) => repo,
        None => {
            println!("⏭️  Test skipped - GitHub CLI not available");
            return;
        }
    };

    println!("🚀 Starting squash merge test with deleted branch");

    if let Err(e) = test_repo.create_on_github() {
        panic!("Failed to create GitHub repository: {}", e);
    }

    // Install GitHub CI workflow to preserve AI authorship
    test_repo
        .install_github_ci_workflow()
        .expect("Failed to install GitHub CI workflow");

    test_repo
        .create_branch("feature/squash-test")
        .expect("Failed to create feature branch");

    let mut test_file = test_repo.repo.filename("test.txt");
    test_file.set_contents(crate::lines!["LINE 1", "LINE 2 (ai)".ai(), "LINE 3",]);

    test_repo
        .repo
        .stage_all_and_commit("Add lines 1-3")
        .expect("Failed to create commit");

    test_file.insert_at(
        3,
        crate::lines![
            "LINE 4",
            "LINE 5 (ai)".ai(),
            "LINE 6 (ai)".ai(),
            "LINE 7 (ai)".ai(),
        ],
    );

    test_repo
        .repo
        .stage_all_and_commit("Add lines 4-7")
        .expect("Failed to create second commit");

    let head_sha = test_repo
        .repo
        .git(&["rev-parse", "HEAD"])
        .expect("Failed to get HEAD SHA")
        .trim()
        .to_string();

    test_repo
        .push_branch("feature/squash-test")
        .expect("Failed to push branch");

    let pr_url = test_repo
        .create_pr(
            "Test squash merge with deletion",
            "Testing squash merge with deleted branch",
        )
        .expect("Failed to create PR");

    let pr_number = test_repo
        .extract_pr_number(&pr_url)
        .expect("Failed to extract PR number");

    // Use squash merge strategy
    test_repo
        .merge_pr(&pr_number, MergeStrategy::Squash)
        .expect("Failed to squash merge PR");

    println!("✅ Squash merged and deleted branch");

    // Wait for GitHub CI workflow to complete
    test_repo
        .wait_for_workflows(300)
        .expect("GitHub CI workflow failed or timed out");

    // Verify we can still fetch the original commits via PR refs
    let fetch_result = test_repo.repo.git(&[
        "fetch",
        "origin",
        &format!("pull/{}/head:refs/github/pr/{}", pr_number, pr_number),
    ]);

    assert!(
        fetch_result.is_ok(),
        "Should be able to fetch original PR commits"
    );

    // Verify the original commits are accessible
    let commit_exists = test_repo.repo.git(&["cat-file", "-t", &head_sha]);

    assert!(
        commit_exists.is_ok(),
        "Original PR commits should be accessible even after squash merge"
    );

    test_repo
        .checkout_and_pull_default_branch()
        .expect("Failed to checkout and pull main branch");

    println!("✅ Test completed successfully - PR refs work with squash merge");

    test_file.assert_lines_and_blame(crate::lines![
        "LINE 1",
        "LINE 2 (ai)".ai(),
        "LINE 3",
        "LINE 4",
        "LINE 5 (ai)".ai(),
        "LINE 6 (ai)".ai(),
        "LINE 7 (ai)".ai(),
    ]);
}
