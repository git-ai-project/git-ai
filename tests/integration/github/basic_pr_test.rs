use super::github_test_harness::{GitHubTestRepo, MergeStrategy};
use crate::repos::test_file::ExpectedLineExt;

#[test]
#[ignore] // Ignored by default - run with `cargo test --test github_integration -- --ignored`
fn test_basic_pr_with_mixed_authorship() {
    let test_repo = match GitHubTestRepo::new("test_basic_pr_with_mixed_authorship") {
        Some(repo) => repo,
        None => {
            println!("⏭️  Test skipped - GitHub CLI not available");
            return;
        }
    };

    println!("🚀 Starting basic PR test with mixed human+AI authorship");

    if let Err(e) = test_repo.create_on_github() {
        panic!("Failed to create GitHub repository: {}", e);
    }

    test_repo
        .create_branch("feature/basic-test")
        .expect("Failed to create feature branch");

    std::fs::create_dir(test_repo.repo.path().join("src")).expect("Failed to create src directory");

    let mut test_file = test_repo.repo.filename("src/main.rs");
    test_file.set_contents(crate::lines![
        "fn main() {",
        "    println!(\"Hello, world!\");".ai(),
        "}",
    ]);

    test_repo
        .repo
        .stage_all_and_commit("Add basic main function")
        .expect("Failed to create commit");

    test_file.insert_at(
        2,
        crate::lines![
            "    // AI-generated greeting".ai(),
            "    println!(\"Welcome to git-ai!\");".ai(),
        ],
    );

    test_repo
        .repo
        .stage_all_and_commit("AI adds greeting")
        .expect("Failed to create AI commit");

    test_repo
        .push_branch("feature/basic-test")
        .expect("Failed to push branch");

    let pr_url = test_repo
        .create_pr(
            "Basic mixed authorship test",
            "Testing basic human + AI authorship tracking",
        )
        .expect("Failed to create PR");

    println!("✅ Pull request created: {}", pr_url);

    let pr_number = test_repo
        .extract_pr_number(&pr_url)
        .expect("Failed to extract PR number");

    test_repo
        .merge_pr(&pr_number, MergeStrategy::Merge)
        .expect("Failed to merge PR");

    test_repo
        .checkout_and_pull_default_branch()
        .expect("Failed to checkout and pull main branch");

    println!("✅ Test completed successfully");

    test_file.assert_lines_and_blame(crate::lines![
        "fn main() {".human(),
        "    println!(\"Hello, world!\");".ai(),
        "    // AI-generated greeting".ai(),
        "    println!(\"Welcome to git-ai!\");".ai(),
        "}".human(),
    ]);
}
