use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// Chinese filename — checkpoint and blame
// =============================================================================

#[test]
fn test_chinese_filename_checkpoint_and_blame() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file with a Chinese filename
    let file_path = repo.path().join("数据.rs");
    fs::write(&file_path, "fn data() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "数据.rs"]).unwrap();
    repo.stage_all_and_commit("add chinese file").unwrap();

    // Blame should work and show AI attribution
    let blame = repo.git_ai(&["blame", "数据.rs"]).unwrap();
    assert!(
        blame.contains("mock_ai"),
        "Blame on Chinese-named file should show AI attribution, got:\n{}",
        blame
    );
}

// =============================================================================
// Emoji filename
// =============================================================================

#[test]
fn test_emoji_filename() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file with emoji in the name
    let mut emoji_file = repo.filename("🚀rocket.txt");
    emoji_file.set_contents(crate::lines!["Launch sequence".ai(), "Liftoff!".ai(),]);

    repo.stage_all_and_commit("Add emoji file").unwrap();

    // Blame should work with emoji filename
    emoji_file.assert_lines_and_blame(crate::lines!["Launch sequence".ai(), "Liftoff!".ai(),]);
}

// =============================================================================
// Mixed ASCII and UTF-8 filenames in one commit
// =============================================================================

#[test]
fn test_mixed_ascii_utf8_filenames() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create files with different naming styles
    let mut ascii_file = repo.filename("normal.txt");
    ascii_file.set_contents(crate::lines!["Normal line".ai()]);

    let mut chinese_file = repo.filename("配置.json");
    chinese_file.set_contents(crate::lines!["{\"key\": \"value\"}".ai()]);

    let mut emoji_file = repo.filename("🎉party.txt");
    emoji_file.set_contents(crate::lines!["Celebration".ai()]);

    // Commit all together
    repo.stage_all_and_commit("Add mixed named files").unwrap();

    // All files should have correct attribution
    ascii_file.assert_lines_and_blame(crate::lines!["Normal line".ai()]);
    chinese_file.assert_lines_and_blame(crate::lines!["{\"key\": \"value\"}".ai()]);
    emoji_file.assert_lines_and_blame(crate::lines!["Celebration".ai()]);
}

// =============================================================================
// Nested UTF-8 directory names
// =============================================================================

#[test]
fn test_nested_utf8_directory_names() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file in nested directories with non-ASCII names
    let mut nested_file = repo.filename("src/模块/组件.ts");
    nested_file.set_contents(crate::lines![
        "export const Component = () => {};".ai(),
        "export default Component;".ai(),
    ]);

    repo.stage_all_and_commit("Add nested UTF-8 file").unwrap();

    // Blame should work with nested UTF-8 paths
    nested_file.assert_lines_and_blame(crate::lines![
        "export const Component = () => {};".ai(),
        "export default Component;".ai(),
    ]);
}

// =============================================================================
// Japanese filename
// =============================================================================

#[test]
fn test_japanese_filename() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file with Japanese characters
    let mut japanese_file = repo.filename("テスト.rs");
    japanese_file.set_contents(crate::lines![
        "fn main() {".ai(),
        "    println!(\"こんにちは\");".ai(),
        "}".ai(),
    ]);

    repo.stage_all_and_commit("Add Japanese file").unwrap();

    // Attribution should be preserved
    japanese_file.assert_lines_and_blame(crate::lines![
        "fn main() {".ai(),
        "    println!(\"こんにちは\");".ai(),
        "}".ai(),
    ]);
}

// =============================================================================
// Korean filename
// =============================================================================

#[test]
fn test_korean_filename() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file with Korean characters
    let mut korean_file = repo.filename("한글파일.txt");
    korean_file.set_contents(crate::lines!["안녕하세요".ai(), "감사합니다".ai(),]);

    repo.stage_all_and_commit("Add Korean file").unwrap();

    // Attribution should be preserved
    korean_file.assert_lines_and_blame(crate::lines!["안녕하세요".ai(), "감사합니다".ai(),]);
}

crate::reuse_tests_in_worktree!(
    test_chinese_filename_checkpoint_and_blame,
    test_emoji_filename,
    test_mixed_ascii_utf8_filenames,
    test_nested_utf8_directory_names,
    test_japanese_filename,
    test_korean_filename,
);
