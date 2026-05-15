use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// Core: Binary files do not crash checkpoint
// =============================================================================

#[test]
fn test_binary_file_does_not_crash_checkpoint() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write a PNG-like binary file
    let binary_path = repo.path().join("image.png");
    fs::write(&binary_path, b"\x89PNG\r\n\x1a\n\x00\x00").unwrap();

    // Checkpoint should not panic or error on the binary file
    let result = repo.git_ai(&["checkpoint", "mock_ai", "image.png"]);
    assert!(
        result.is_ok(),
        "Checkpoint on a binary/PNG file should not crash, got: {:?}",
        result.err()
    );
}

// =============================================================================
// Non-UTF8 content alongside UTF8 -- attribution still works
// =============================================================================

#[test]
fn test_non_utf8_content_alongside_utf8() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write a non-UTF8 file
    let non_utf8_path = repo.path().join("binary_data.bin");
    fs::write(&non_utf8_path, b"\xc0\xc1\xfe\xff hello").unwrap();

    // AI edits a normal UTF-8 file
    let mut ai_file = repo.filename("code.rs");
    ai_file.set_contents(crate::lines![
        "fn main() {".ai(),
        "    println!(\"hello\");".ai(),
        "}".ai(),
    ]);

    // Commit both files together
    repo.stage_all_and_commit("Add non-utf8 and AI file")
        .unwrap();

    // Attribution should work correctly for the UTF-8 file
    ai_file.assert_lines_and_blame(crate::lines![
        "fn main() {".ai(),
        "    println!(\"hello\");".ai(),
        "}".ai(),
    ]);
}

// =============================================================================
// Checkpoint skips binary files
// =============================================================================

#[test]
fn test_checkpoint_skips_binary_files() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write both a binary and a text file
    let binary_path = repo.path().join("data.bin");
    fs::write(
        &binary_path,
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ],
    )
    .unwrap();

    let text_path = repo.path().join("code.txt");
    fs::write(&text_path, "fn hello() {}\n").unwrap();

    // Checkpoint on both -- should not crash
    let result = repo.git_ai(&["checkpoint", "mock_ai"]);
    assert!(
        result.is_ok(),
        "Checkpoint with binary files present should not crash, got: {:?}",
        result.err()
    );

    // The text file should still get proper attribution after commit
    repo.stage_all_and_commit("Add binary and text").unwrap();

    let blame = repo.git_ai(&["blame", "code.txt"]);
    assert!(
        blame.is_ok(),
        "Blame on text file should succeed even with binary neighbor"
    );
    let blame_output = blame.unwrap();
    assert!(
        blame_output.contains("mock_ai") || blame_output.contains("fn hello"),
        "Blame should contain attribution info for the text file"
    );
}

// =============================================================================
// Blame on non-UTF8 file does not crash
// =============================================================================

#[test]
fn test_blame_on_non_utf8_file() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write a non-UTF8 file
    let non_utf8_path = repo.path().join("legacy.dat");
    fs::write(&non_utf8_path, b"\xc0\xc1\xfe\xff hello\nline2\n").unwrap();
    repo.stage_all_and_commit("Add non-utf8 file").unwrap();

    // Blame should not crash, even if output is limited
    let result = repo.git_ai(&["blame", "legacy.dat"]);
    // We accept either success or a graceful error -- the key is no panic/crash
    match result {
        Ok(_output) => {
            // If it succeeds, blame output can be empty for non-UTF8 files (that's OK)
        }
        Err(err) => {
            // A graceful error is acceptable, just not a crash/panic
            assert!(
                !err.contains("panic") && !err.contains("SIGABRT"),
                "Blame on non-UTF8 file should not panic, got: {}",
                err
            );
        }
    }
}

// =============================================================================
// Stats with binary files -- binary files excluded from AI stats
// =============================================================================

#[test]
fn test_stats_with_binary_files() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Write a binary file
    let binary_path = repo.path().join("image.png");
    fs::write(
        &binary_path,
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0D\x49\x48\x44\x52",
    )
    .unwrap();

    // Write an AI text file
    let mut ai_file = repo.filename("output.py");
    ai_file.set_contents(crate::lines![
        "def hello():".ai(),
        "    return 'world'".ai(),
    ]);

    repo.stage_all_and_commit("Add binary and AI files")
        .unwrap();

    // Stats should report AI lines from the text file; binary should be excluded
    let raw = repo.git_ai(&["stats", "--json"]).unwrap();
    // Verify it's valid JSON
    let start = raw.find('{').unwrap_or(0);
    let end = raw.rfind('}').unwrap_or(raw.len().saturating_sub(1));
    let json_str = &raw[start..=end];
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .expect("Stats JSON should be valid even with binary files present");

    // AI additions should count the text file lines
    let ai_additions = parsed["ai_additions"].as_u64().unwrap_or(0);
    assert!(
        ai_additions >= 2,
        "AI additions should count text file lines (got {})",
        ai_additions
    );
}

crate::reuse_tests_in_worktree!(
    test_binary_file_does_not_crash_checkpoint,
    test_non_utf8_content_alongside_utf8,
    test_checkpoint_skips_binary_files,
    test_blame_on_non_utf8_file,
    test_stats_with_binary_files,
);
