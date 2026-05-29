//! Tests that an empty `.git` directory (e.g. from a docker-compose volume mount)
//! does not fool git-ai into treating it as the repository root.
//!
//! Issue #1415: a user's docker-compose setup accidentally created an empty `.git`
//! directory in a subfolder. git-ai found it first and wrote authorship notes there
//! instead of the real repo's `.git`.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// When a subdirectory contains an empty `.git` dir (no HEAD), checkpoints and
/// commits should still target the real parent repository.
#[test]
fn empty_git_subdir_does_not_hijack_attribution() {
    let repo = TestRepo::new();

    // Bootstrap the repo with an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Root repo"]);
    repo.stage_all_and_commit("initial commit").unwrap();

    // Create a subdirectory with an empty .git directory (simulates docker-compose
    // accidentally creating one via a volume mount)
    let subdir = repo.path().join("services").join("app");
    fs::create_dir_all(subdir.join(".git")).unwrap();

    // Write a file inside that subdirectory
    let file_path = subdir.join("main.txt");
    fs::write(&file_path, "Human line\n").unwrap();

    // Fire a known-human checkpoint scoped to the file
    repo.git_ai(&["checkpoint", "mock_known_human", "services/app/main.txt"])
        .unwrap();

    // Now simulate an AI edit
    fs::write(&file_path, "Human line\nAI line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "services/app/main.txt"])
        .unwrap();

    // Commit in the real repo
    repo.stage_all_and_commit("add services/app/main.txt")
        .unwrap();

    // Verify attribution lands in the real repo, not in the empty .git
    let mut file = repo.filename("services/app/main.txt");
    file.assert_committed_lines(crate::lines!["Human line".human(), "AI line".ai(),]);
}

/// Verify that when the empty `.git` dir is between the file and the real repo,
/// worktree discovery still reaches the real repo root.
#[test]
fn empty_git_subdir_skipped_during_worktree_discovery() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Root repo"]);
    repo.stage_all_and_commit("initial commit").unwrap();

    // Empty .git in an intermediate directory
    let intermediate = repo.path().join("packages").join("core");
    fs::create_dir_all(intermediate.join(".git")).unwrap();

    // A deeper nested file
    let deep = intermediate.join("src");
    fs::create_dir_all(&deep).unwrap();
    let file_path = deep.join("lib.txt");

    fs::write(&file_path, "line one\n").unwrap();
    repo.git_ai(&[
        "checkpoint",
        "mock_known_human",
        "packages/core/src/lib.txt",
    ])
    .unwrap();

    fs::write(&file_path, "line one\nai added\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "packages/core/src/lib.txt"])
        .unwrap();

    repo.stage_all_and_commit("deep nested file").unwrap();

    let mut file = repo.filename("packages/core/src/lib.txt");
    file.assert_committed_lines(crate::lines!["line one".human(), "ai added".ai(),]);
}
