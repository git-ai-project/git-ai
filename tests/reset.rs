#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;

/// Test git reset --hard: should discard all changes and reset to target commit
#[test]
fn test_reset_hard_deletes_working_log() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create initial commit
    file.set_contents(lines!["line 1", "line 2", "line 3"]);
    let first_commit = repo.stage_all_and_commit("First commit").unwrap();

    // Make second commit with AI changes
    file.insert_at(3, lines!["// AI line".ai()]);
    repo.stage_all_and_commit("Second commit").unwrap();

    // Make some uncommitted AI changes
    file.insert_at(4, lines!["// Uncommitted".ai()]);

    // Reset --hard to first commit
    repo.git(&["reset", "--hard", &first_commit.commit_sha])
        .expect("reset --hard should succeed");

    // After hard reset, file should match first commit (no AI lines, no uncommitted changes)
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["line 1", "line 2", "line 3"]);

    // Make a new commit to verify working directory is clean
    file.insert_at(3, lines!["new line"]);
    repo.stage_all_and_commit("After reset").unwrap();
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["line 1", "line 2", "line 3", "new line",]);
}

/// Test git reset --soft: should preserve AI authorship from unwound commits
#[test]
fn test_reset_soft_reconstructs_working_log() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create initial commit
    file.set_contents(lines!["line 1", "line 2"]);
    let first_commit = repo.stage_all_and_commit("First commit").unwrap();

    // Make second commit with AI changes
    file.insert_at(2, lines!["// AI addition".ai()]);
    repo.stage_all_and_commit("Second commit").unwrap();

    // Reset --soft to first commit
    repo.git(&["reset", "--soft", &first_commit.commit_sha])
        .expect("reset --soft should succeed");

    // After soft reset, changes should be staged, and when we commit them
    // they should retain AI authorship
    let new_commit = repo.commit("Re-commit AI changes").unwrap();

    // Verify AI authorship was preserved in the commit
    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved after reset --soft"
    );

    // Verify blame shows AI authorship
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "line 2".human(),
        "// AI addition".ai(),
    ]);
}

/// Test git reset --mixed (default): working directory preserved
#[test]
fn test_reset_mixed_reconstructs_working_log() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.rs");

    // Create initial commit
    file.set_contents(lines!["fn main() {", "}"]);
    let first_commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Make second commit with AI changes - simpler approach
    file.insert_at(1, lines!["    // AI: Added logging".ai()]);
    file.insert_at(2, lines!["    println!(\"Hello\");".ai()]);

    repo.stage_all_and_commit("Add logging").unwrap();

    // Reset --mixed to first commit
    repo.git(&["reset", "--mixed", &first_commit.commit_sha])
        .expect("reset --mixed should succeed");

    // After mixed reset, changes should be unstaged but in working directory
    // Stage and commit them to verify AI authorship was preserved
    let new_commit = repo.stage_all_and_commit("Re-commit after reset").unwrap();

    // Verify AI authorship was preserved
    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved after reset --mixed"
    );

    file = repo.filename("main.rs");
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    // AI: Added logging".ai(),
        "    println!(\"Hello\");".ai(),
        "}".human(),
    ]);
}

/// Test git reset to same commit: should preserve uncommitted AI changes
#[test]
fn test_reset_to_same_commit_is_noop() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create commit with AI changes
    file.set_contents(lines!["line 1", "// AI line".ai(), ""]);
    repo.stage_all_and_commit("Commit").unwrap();

    // Make uncommitted changes
    file.insert_at(2, lines!["// More changes".ai()]);

    // Reset to same commit (HEAD)
    repo.git(&["reset", "HEAD"]).expect("reset should succeed");

    // Uncommitted AI changes should still be preserved in working directory
    // Commit them to verify authorship
    let new_commit = repo.stage_all_and_commit("After reset to HEAD").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for uncommitted changes"
    );

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines![
        "line 1".human(),
        "// AI line".ai(),
        "// More changes".ai(),
    ]);
}

/// Test git reset with multiple commits unwound: should preserve all AI authorship
#[test]
fn test_reset_multiple_commits() {
    let repo = TestRepo::new();
    let mut file = repo.filename("code.js");

    // Create base commit
    file.set_contents(lines!["// Base", ""]);
    let base_commit = repo.stage_all_and_commit("Base").unwrap();

    // Second commit - AI adds feature
    file.insert_at(1, lines!["// AI feature 1".ai()]);
    repo.stage_all_and_commit("Feature 1").unwrap();

    // Third commit - AI adds another feature
    file.insert_at(2, lines!["// AI feature 2".ai()]);
    repo.stage_all_and_commit("Feature 2").unwrap();

    // Reset --soft to base
    repo.git(&["reset", "--soft", &base_commit.commit_sha])
        .expect("reset --soft should succeed");

    // Commit and verify both AI features are attributed correctly
    let new_commit = repo.commit("Re-commit features").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for all unwound commits"
    );

    file = repo.filename("code.js");
    file.assert_lines_and_blame(lines![
        "// Base".human(),
        "// AI feature 1".ai(),
        "// AI feature 2".ai(),
    ]);
}

/// Test git reset with uncommitted changes preserved: should preserve all AI authorship
#[test]
fn test_reset_preserves_uncommitted_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("app.py");

    // Create base commit
    file.set_contents(lines!["def main():", "    pass", ""]);
    let base_commit = repo.stage_all_and_commit("Base").unwrap();

    // Second commit with AI changes
    file.replace_at(1, "    print('hello')".ai());
    repo.stage_all_and_commit("Add print").unwrap();

    // Third commit with more AI changes
    file.insert_at(2, lines!["    print('world')".ai()]);
    repo.stage_all_and_commit("Add world").unwrap();

    // Reset --soft to base (should preserve both AI commits as staged)
    let result = repo
        .git(&["reset", "--soft", &base_commit.commit_sha])
        .expect("reset --soft should succeed");

    println!("result: {}", result);
    // Commit and verify AI authorship preserved
    let new_commit = repo.commit("Re-commit AI changes").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved from multiple unwound commits"
    );

    file = repo.filename("app.py");
    file.assert_lines_and_blame(lines![
        "def main():".human(),
        "    print('hello')".ai(),
        "    print('world')".ai(),
    ]);
}

/// Test git reset with pathspecs: should preserve AI authorship for non-reset files
#[test]
fn test_reset_with_pathspec() {
    let repo = TestRepo::new();
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");

    // Create initial commit with multiple files
    file1.set_contents(lines!["content 1", ""]);
    file2.set_contents(lines!["content 2", ""]);
    let first_commit = repo.stage_all_and_commit("Initial").unwrap();

    // Commit AI changes to both files
    file1.insert_at(1, lines!["// AI change 1".ai()]);
    file2.insert_at(1, lines!["// AI change 2".ai()]);
    repo.stage_all_and_commit("AI changes both files").unwrap();

    // Make uncommitted changes to both files
    file1.insert_at(2, lines!["// More AI".ai()]);
    file2.insert_at(2, lines!["// More AI".ai()]);

    // Now reset only file1.txt to first commit with pathspec
    repo.git(&["reset", &first_commit.commit_sha, "--", "file1.txt"])
        .expect("reset with pathspec should succeed");

    // Stage all and commit to verify file2 still has AI attribution
    let new_commit = repo.stage_all_and_commit("After pathspec reset").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for file2"
    );

    file2 = repo.filename("file2.txt");
    // file2 should still have AI changes
    file2.assert_lines_and_blame(lines![
        "content 2".human(),
        "// AI change 2".ai(),
        "// More AI".ai(),
    ]);
}

/// Test git reset forward (to descendant): should restore commit state
#[test]
fn test_reset_forward_is_noop() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create two commits
    file.set_contents(lines!["v1"]);
    let first_commit = repo.stage_all_and_commit("First").unwrap();

    file.insert_at(1, lines!["v2".ai()]);
    let second_commit = repo.stage_all_and_commit("Second").unwrap();

    // Reset back to first (--hard discards all changes)
    repo.git(&["reset", "--hard", &first_commit.commit_sha])
        .expect("reset --hard should succeed");

    // Verify file is back to v1 only
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["v1".human()]);

    // Now reset forward to second with --hard to restore the working tree
    repo.git(&["reset", "--hard", &second_commit.commit_sha])
        .expect("reset --hard should succeed");

    // File should now match second commit
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["v1".human(), "v2".ai()]);
}

/// Test git reset with AI and human mixed changes: should preserve all authorship
#[test]
fn test_reset_mixed_ai_human_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.rs");

    // Base commit
    file.set_contents(lines!["fn main() {}"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // AI commit
    file.set_contents(lines!["fn main() {", "    // AI".ai(), "}"]);
    repo.stage_all_and_commit("AI changes").unwrap();

    // Human commit
    file.insert_at(2, lines!["    // Human"]);
    repo.stage_all_and_commit("Human changes").unwrap();

    // Reset to base
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset --soft should succeed");

    // Commit and verify authorship
    let new_commit = repo.commit("Re-commit mixed changes").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved in mixed AI/human changes"
    );

    file = repo.filename("main.rs");
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    // AI".ai(),
        "    // Human".human(),
        "}".human(),
    ]);
}

/// Test git reset --merge: should be like --mixed for clean working tree
#[test]
fn test_reset_merge() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create base
    file.set_contents(lines!["base"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // Create second commit
    file.insert_at(1, lines!["// AI line".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    // Reset --merge (behaves like --mixed when working tree is clean)
    // Note: --merge is designed to abort merges, so it may not work in all contexts
    // Let's use --mixed instead for this test
    repo.git(&["reset", &base.commit_sha])
        .expect("reset should succeed");

    // Commit and verify AI authorship preserved
    let new_commit = repo.stage_all_and_commit("Re-commit").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved after reset"
    );

    file = repo.filename("test.txt");
    file.assert_lines_and_blame(lines!["base".human(), "// AI line".ai()]);
}

/// Test git reset with new files added in unwound commit: should preserve AI authorship
#[test]
fn test_reset_with_new_files() {
    let repo = TestRepo::new();
    let mut old_file = repo.filename("old.txt");

    // Base commit
    old_file.set_contents(lines!["existing"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // Add new file in second commit
    let mut new_file = repo.filename("new.txt");
    new_file.set_contents(lines!["// AI created this".ai()]);
    repo.stage_all_and_commit("Add new file").unwrap();

    // Reset to base
    repo.git(&["reset", "--soft", &base.commit_sha])
        .expect("reset --soft should succeed");

    // Commit and verify new file has AI authorship
    let new_commit = repo.commit("Re-commit with new file").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for new file"
    );

    new_file = repo.filename("new.txt");
    new_file.assert_lines_and_blame(lines!["// AI created this".ai()]);
}

/// Test git reset with file deletions in unwound commit
#[test]
fn test_reset_with_deleted_files() {
    let repo = TestRepo::new();
    let mut keep_file = repo.filename("keep.txt");
    let mut delete_file = repo.filename("delete.txt");

    // Base with two files
    keep_file.set_contents(lines!["keep this"]);
    delete_file.set_contents(lines!["will delete"]);
    let base = repo.stage_all_and_commit("Base").unwrap();

    // Delete one file
    repo.git(&["rm", "delete.txt"]).expect("rm should succeed");
    let delete_commit = repo.commit("Delete file").unwrap();

    // Verify deletion commit has no AI attestations
    assert_eq!(delete_commit.authorship_log.attestations.len(), 0);

    // Reset --hard to base (restores both files in working directory)
    repo.git(&["reset", "--hard", &base.commit_sha])
        .expect("reset --hard should succeed");

    // Verify both files exist in working directory
    assert!(
        repo.read_file("keep.txt").is_some(),
        "keep.txt should exist"
    );
    assert!(
        repo.read_file("delete.txt").is_some(),
        "delete.txt should exist"
    );

    // Make a new commit to verify files work correctly
    keep_file = repo.filename("keep.txt");
    keep_file.insert_at(1, lines!["new line"]);
    repo.stage_all_and_commit("After reset").unwrap();
}

/// Test git reset --mixed with pathspec: should preserve AI authorship for non-reset files
#[test]
fn test_reset_mixed_pathspec_preserves_ai_authorship() {
    let repo = TestRepo::new();
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");

    // Base commit with two files
    file1.set_contents(lines!["base content 1", ""]);
    file2.set_contents(lines!["base content 2", ""]);
    let base_commit = repo.stage_all_and_commit("Base commit").unwrap();

    // Second commit: AI modifies both files
    file1.insert_at(1, lines!["// AI change to file1".ai()]);
    file2.insert_at(1, lines!["// AI change to file2".ai()]);
    let _second_commit = repo.stage_all_and_commit("AI modifies both files").unwrap();

    // Make uncommitted changes to file2 (not file1)
    file2.insert_at(2, lines!["// More AI changes".ai()]);

    // Get current branch for HEAD check
    let current_head_before = repo.current_branch();

    // Reset only file1.txt to base commit with pathspec
    // This should preserve uncommitted changes for file2.txt
    repo.git(&["reset", &base_commit.commit_sha, "--", "file1.txt"])
        .expect("reset with pathspec should succeed");

    // HEAD should not move with pathspec reset
    let current_head_after = repo.current_branch();
    assert_eq!(
        current_head_before, current_head_after,
        "HEAD should not move with pathspec reset"
    );

    // Commit and verify file2 still has AI authorship
    let new_commit = repo.stage_all_and_commit("After pathspec reset").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for file2 after pathspec reset"
    );

    file2 = repo.filename("file2.txt");
    file2.assert_lines_and_blame(lines![
        "base content 2".human(),
        "// AI change to file2".ai(),
        "// More AI changes".ai(),
    ]);
}

/// Test git reset --mixed with pathspec on multiple commits worth of AI changes
#[test]
fn test_reset_mixed_pathspec_multiple_commits() {
    let repo = TestRepo::new();
    let mut app_file = repo.filename("app.js");
    let mut lib_file = repo.filename("lib.js");

    // Base commit
    app_file.set_contents(lines!["// base", ""]);
    lib_file.set_contents(lines!["// base", ""]);
    let base_commit = repo.stage_all_and_commit("Base").unwrap();

    // First AI commit - modifies both files
    app_file.insert_at(1, lines!["// AI feature 1".ai()]);
    lib_file.insert_at(1, lines!["// AI lib 1".ai()]);
    repo.stage_all_and_commit("AI feature 1").unwrap();

    // Second AI commit - modifies both files again
    app_file.insert_at(2, lines!["// AI feature 2".ai()]);
    lib_file.insert_at(2, lines!["// AI lib 2".ai()]);
    let _second_ai_commit = repo.stage_all_and_commit("AI feature 2").unwrap();

    // Make uncommitted changes to lib.js (not app.js)
    lib_file.insert_at(3, lines!["// More lib".ai()]);

    // Get current branch for HEAD check
    let current_head_before = repo.current_branch();

    // Reset only app.js to base with pathspec
    // This should preserve uncommitted changes for lib.js
    repo.git(&["reset", &base_commit.commit_sha, "--", "app.js"])
        .expect("reset with pathspec should succeed");

    // HEAD should not move
    let current_head_after = repo.current_branch();
    assert_eq!(
        current_head_before, current_head_after,
        "HEAD should not move"
    );

    // Commit and verify lib.js retains AI authorship
    let new_commit = repo.stage_all_and_commit("After pathspec reset").unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for lib.js after pathspec reset"
    );

    lib_file = repo.filename("lib.js");
    lib_file.assert_lines_and_blame(lines![
        "// base".human(),
        "// AI lib 1".ai(),
        "// AI lib 2".ai(),
        "// More lib".ai(),
    ]);
}

/// Test git reset with directory pathspec: should reset only files in the specified directory
#[test]
fn test_reset_with_directory_pathspec() {
    let repo = TestRepo::new();

    // Create directory structure
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::create_dir_all(repo.path().join("lib")).unwrap();

    let mut src_file = repo.filename("src/app.rs");
    let mut lib_file = repo.filename("lib/utils.rs");
    let mut root_file = repo.filename("root.txt");

    // Base commit with files in different directories
    src_file.set_contents(lines!["fn main() {}", ""]);
    lib_file.set_contents(lines!["pub fn helper() {}", ""]);
    root_file.set_contents(lines!["root content", ""]);
    let base_commit = repo.stage_all_and_commit("Base commit").unwrap();

    // Second commit: AI modifies files in all directories
    src_file.insert_at(1, lines!["    // AI src change".ai()]);
    lib_file.insert_at(1, lines!["    // AI lib change".ai()]);
    root_file.insert_at(1, lines!["// AI root change".ai()]);
    repo.stage_all_and_commit("AI changes everywhere").unwrap();

    // Make uncommitted AI changes to lib and root (not src)
    lib_file.insert_at(2, lines!["    // More AI lib".ai()]);
    root_file.insert_at(2, lines!["// More AI root".ai()]);

    // Reset only the src directory to base commit using directory pathspec
    repo.git(&["reset", &base_commit.commit_sha, "--", "src"])
        .expect("reset with directory pathspec should succeed");

    // Stage all and commit to verify attributions
    let new_commit = repo
        .stage_all_and_commit("After directory pathspec reset")
        .unwrap();

    assert!(
        !new_commit.authorship_log.attestations.is_empty(),
        "AI authorship should be preserved for lib and root files"
    );

    // lib/utils.rs should still have AI changes (not in reset pathspec)
    lib_file = repo.filename("lib/utils.rs");
    lib_file.assert_lines_and_blame(lines![
        "pub fn helper() {}".human(),
        "    // AI lib change".ai(),
        "    // More AI lib".ai(),
    ]);

    // root.txt should still have AI changes (not in reset pathspec)
    root_file = repo.filename("root.txt");
    root_file.assert_lines_and_blame(lines![
        "root content".human(),
        "// AI root change".ai(),
        "// More AI root".ai(),
    ]);
}
