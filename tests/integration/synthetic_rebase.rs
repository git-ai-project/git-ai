use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Test synthetic rebase detection for Graphite-style plumbing command workflow.
///
/// Graphite (and similar tools) bypass `git rebase` porcelain and instead use:
/// 1. `git merge-tree --write-tree` to compute merge result
/// 2. `git commit-tree` to create new commit object
/// 3. `git update-ref refs/heads/branch <new> <old>` to move branch pointer
///
/// This test verifies that git-ai's daemon detects this pattern as a SyntheticRebase
/// and triggers v3 attribution rewriting.
#[test]
fn test_graphite_style_synthetic_rebase_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["Base content"]);
    repo.stage_all_and_commit("Base commit").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Feature branch: AI adds code
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut code = repo.filename("code.js");
    code.set_contents(crate::lines![
        "// AI-generated code".ai(),
        "function sort(arr) {".ai(),
        "    return arr.sort();".ai(),
        "}".ai()
    ]);
    repo.stage_all_and_commit("AI adds sort function").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify AI attribution on feature branch
    code.assert_committed_lines(crate::lines![
        "// AI-generated code".ai(),
        "function sort(arr) {".ai(),
        "    return arr.sort();".ai(),
        "}".ai()
    ]);

    // Main branch: adds unrelated file
    repo.git(&["checkout", &base_commit]).unwrap();
    let mut other = repo.filename("other.txt");
    other.set_contents(crate::lines!["Other content"]);
    repo.stage_all_and_commit("Add other file").unwrap();
    let new_base = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Now perform synthetic rebase using plumbing commands (Graphite-style)
    // Step 1: Compute merge tree
    let merge_tree_output = repo
        .git(&["merge-tree", "--write-tree", &new_base, &feature_commit])
        .unwrap();
    let tree_oid = merge_tree_output.trim();

    // Step 2: Create new commit object
    let commit_tree_output = repo
        .git(&[
            "commit-tree",
            tree_oid,
            "-p",
            &new_base,
            "-m",
            "Synthetic rebase of AI commit",
        ])
        .unwrap();
    let new_commit = commit_tree_output.trim();

    // Step 3: Move branch pointer using update-ref (THIS SHOULD TRIGGER SyntheticRebase)
    repo.git(&[
        "update-ref",
        "refs/heads/feature",
        new_commit,
        &feature_commit,
    ])
    .unwrap();

    // Checkout the new commit
    repo.git(&["checkout", "feature"]).unwrap();

    // Verify attribution was preserved through synthetic rebase
    code.assert_committed_lines(crate::lines![
        "// AI-generated code".ai(),
        "function sort(arr) {".ai(),
        "    return arr.sort();".ai(),
        "}".ai()
    ]);
}

/// Test that synthetic rebase detection handles squash-style operations.
///
/// Tools like Graphite can squash multiple commits into one using plumbing commands.
/// This tests that v3 attribution merges notes from multiple original commits.
#[test]
fn test_synthetic_rebase_squash_merges_notes() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["Base"]);
    repo.stage_all_and_commit("Base").unwrap();
    let base_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Feature branch: two AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut code = repo.filename("code.js");
    code.set_contents(crate::lines!["// First AI commit".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();

    code.set_contents(crate::lines![
        "// First AI commit".ai(),
        "// Second AI commit".ai()
    ]);
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let commit2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify both commits have AI attribution
    code.assert_committed_lines(crate::lines![
        "// First AI commit".ai(),
        "// Second AI commit".ai()
    ]);

    // Synthetic squash: create single commit with both changes
    let tree_oid = repo
        .git(&["rev-parse", format!("{}^{{tree}}", commit2).as_str()])
        .unwrap()
        .trim()
        .to_string();

    let squashed_commit = repo
        .git(&[
            "commit-tree",
            &tree_oid,
            "-p",
            &base_commit,
            "-m",
            "Squashed commits",
        ])
        .unwrap()
        .trim()
        .to_string();

    // Move branch to squashed commit (triggers SyntheticRebase)
    repo.git(&[
        "update-ref",
        "refs/heads/feature",
        &squashed_commit,
        &commit2,
    ])
    .unwrap();

    repo.git(&["checkout", "feature"]).unwrap();

    // Both lines should still be AI-attributed (merged from multiple commits)
    code.assert_committed_lines(crate::lines![
        "// First AI commit".ai(),
        "// Second AI commit".ai()
    ]);
}

/// Test that false positives (branch creation) don't break attribution.
///
/// The synthetic rebase heuristic is conservative and may trigger on branch creation
/// or other update-ref operations. This test verifies that v3 gracefully handles
/// these cases by finding no commits to map.
#[test]
fn test_synthetic_rebase_false_positive_branch_creation() {
    let repo = TestRepo::new();

    // Create commit with AI attribution
    let mut code = repo.filename("code.js");
    code.set_contents(crate::lines!["// AI code".ai()]);
    repo.stage_all_and_commit("AI commit").unwrap();
    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify AI attribution
    code.assert_committed_lines(crate::lines!["// AI code".ai()]);

    // Use update-ref to create a new branch (potential false positive)
    repo.git(&[
        "update-ref",
        "refs/heads/new-branch",
        &commit,
        "0000000000000000000000000000000000000000",
    ])
    .unwrap();

    // Checkout new branch and verify attribution is unchanged
    repo.git(&["checkout", "new-branch"]).unwrap();
    code.assert_committed_lines(crate::lines!["// AI code".ai()]);
}
