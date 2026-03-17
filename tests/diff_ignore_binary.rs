mod repos;

use repos::test_repo::TestRepo;
use serde_json::Value;
use std::fs;

/// Helper: write raw bytes to a file in the test repo
fn write_bytes(repo: &TestRepo, path: &str, bytes: &[u8]) {
    let abs_path = repo.path().join(path);
    if let Some(parent) = abs_path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be creatable");
    }
    fs::write(abs_path, bytes).expect("file write should succeed");
}

/// Helper: write text content to a file in the test repo
fn write_file(repo: &TestRepo, path: &str, contents: &str) {
    write_bytes(repo, path, contents.as_bytes());
}

/// Helper: run git-ai diff --json and parse the result
fn diff_json(repo: &TestRepo, args: &[&str]) -> Value {
    let output = repo.git_ai(args).expect("git-ai diff should succeed");
    serde_json::from_str(&output).expect("diff JSON should parse")
}

/// Helper: run git-ai diff and return raw output (for non-JSON mode)
fn diff_raw(repo: &TestRepo, args: &[&str]) -> String {
    repo.git_ai(args).expect("git-ai diff should succeed")
}

// ============================================================================
// Binary file tests
// ============================================================================

#[test]
fn test_diff_json_ignores_binary_files_in_output() {
    let repo = TestRepo::new();

    // Create initial commit with a text file
    write_file(&repo, "README.md", "# hello\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Add a binary file (PNG header bytes) and modify the text file
    let png_bytes: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG header
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1 pixel
        0x08, 0x02, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
    ];
    write_bytes(&repo, "image.png", &png_bytes);
    write_file(&repo, "README.md", "# hello\nupdated\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add binary and text").unwrap();

    // diff --json should succeed and not contain the binary file in hunks
    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);

    let hunks = json["hunks"].as_array().expect("hunks should be array");
    for hunk in hunks {
        let file_path = hunk["file_path"].as_str().unwrap_or("");
        assert_ne!(
            file_path, "image.png",
            "Binary file image.png should not appear in diff hunks"
        );
    }

    // README.md changes should still be present
    let readme_hunks: Vec<&Value> = hunks
        .iter()
        .filter(|h| h["file_path"].as_str() == Some("README.md"))
        .collect();
    assert!(
        !readme_hunks.is_empty(),
        "README.md text changes should appear in diff hunks"
    );
}

#[test]
fn test_diff_terminal_handles_binary_files_without_error() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# hello\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Add a binary file alongside text changes
    let binary_bytes: Vec<u8> = (0..256).map(|i| i as u8).collect();
    write_bytes(&repo, "data.bin", &binary_bytes);
    write_file(&repo, "README.md", "# hello\nworld\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add binary").unwrap();

    // Terminal diff should succeed (not crash on binary)
    let output = diff_raw(&repo, &["diff", &commit.commit_sha]);
    assert!(
        output.contains("README.md"),
        "Terminal diff output should contain the text file"
    );
}

#[test]
fn test_diff_json_with_only_binary_changes() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Only binary file change
    let binary_bytes: Vec<u8> = vec![0x00, 0xFF, 0xFE, 0xFD, 0x80, 0x81, 0x82];
    write_bytes(&repo, "data.bin", &binary_bytes);

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add binary only").unwrap();

    // Should succeed with empty hunks (binary files produce no text hunks)
    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    assert!(
        hunks.is_empty(),
        "Hunks should be empty when only binary files changed, got: {:?}",
        hunks
    );
}

// ============================================================================
// UTF-8 / non-UTF-8 charset tests
// ============================================================================

#[test]
fn test_diff_json_handles_non_utf8_file_content() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Create a file with invalid UTF-8 sequences (Latin-1 encoded text)
    // This simulates files with legacy encodings that git can still diff
    let mut content = b"// Latin-1 file\n".to_vec();
    content.extend_from_slice(&[0xC0, 0xE9, 0xF1, 0xFC]); // invalid UTF-8 bytes (Latin-1 chars)
    content.push(b'\n');
    write_bytes(&repo, "legacy.txt", &content);
    write_file(&repo, "README.md", "# repo\nupdated\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add non-utf8 file").unwrap();

    // This should NOT fail with "Failed to parse diff output: invalid utf-8 sequence"
    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);

    // The diff should still contain README.md changes
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let readme_hunks: Vec<&Value> = hunks
        .iter()
        .filter(|h| h["file_path"].as_str() == Some("README.md"))
        .collect();
    assert!(
        !readme_hunks.is_empty(),
        "README.md changes should be present in diff output"
    );
}

#[test]
fn test_diff_json_handles_mixed_utf8_and_binary_content() {
    let repo = TestRepo::new();

    write_file(&repo, "app.js", "console.log('hello');\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Valid UTF-8 with multi-byte characters (CJK, emoji, etc.)
    write_file(
        &repo,
        "app.js",
        "console.log('hello');\nconsole.log('\u{4e16}\u{754c}');\nconsole.log('\u{1F600}');\n",
    );

    // Binary file with mixed content
    let mut mixed = b"header\n".to_vec();
    mixed.extend_from_slice(&[0x00, 0x01, 0x02, 0xFF, 0xFE]);
    write_bytes(&repo, "mixed.dat", &mixed);

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add utf8 and binary").unwrap();

    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");

    // app.js changes should be present with the multi-byte content
    let js_hunks: Vec<&Value> = hunks
        .iter()
        .filter(|h| h["file_path"].as_str() == Some("app.js"))
        .collect();
    assert!(
        !js_hunks.is_empty(),
        "app.js with multi-byte UTF-8 content should appear in hunks"
    );
}

#[test]
fn test_diff_json_handles_complex_utf8_characters() {
    let repo = TestRepo::new();

    // Initial commit with a file containing ASCII only
    write_file(&repo, "i18n.txt", "hello world\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Update with complex UTF-8: CJK, Arabic, emoji, combining characters
    write_file(
        &repo,
        "i18n.txt",
        "hello world\n\u{4e16}\u{754c}\u{4f60}\u{597d}\n\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}\n\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}\ne\u{0301}\n",
    );

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add i18n content").unwrap();

    // Should succeed without any UTF-8 errors
    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let i18n_hunks: Vec<&Value> = hunks
        .iter()
        .filter(|h| h["file_path"].as_str() == Some("i18n.txt"))
        .collect();
    assert!(
        !i18n_hunks.is_empty(),
        "i18n.txt with complex UTF-8 should appear in diff hunks"
    );
}

#[test]
fn test_diff_terminal_handles_non_utf8_without_error() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Create file with invalid UTF-8
    let mut content = b"line one\n".to_vec();
    content.extend_from_slice(&[0x80, 0x81, 0x82, 0xFE, 0xFF]); // invalid UTF-8
    content.push(b'\n');
    write_bytes(&repo, "bad_encoding.txt", &content);
    write_file(&repo, "README.md", "# repo\nchanged\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo.stage_all_and_commit("add bad encoding").unwrap();

    // Terminal mode should not crash either
    let output = diff_raw(&repo, &["diff", &commit.commit_sha]);
    assert!(
        output.contains("README.md"),
        "Terminal diff should still show text file changes"
    );
}

// ============================================================================
// Ignore pattern tests for diff
// ============================================================================

#[test]
fn test_diff_json_respects_default_ignore_patterns() {
    let repo = TestRepo::new();

    write_file(&repo, "src/main.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Modify a source file and add changes to default-ignored files
    write_file(&repo, "src/main.rs", "fn main() {}\nfn added() {}\n");
    write_file(&repo, "Cargo.lock", "# lock content\nline 2\nline 3\n");
    write_file(&repo, "package-lock.json", "{\"lockfileVersion\": 1}\n");
    write_file(&repo, "dist/bundle.min.js", "var a=1;\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let commit = repo
        .stage_all_and_commit("modify source and ignored files")
        .unwrap();

    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let file_paths: Vec<&str> = hunks
        .iter()
        .filter_map(|h| h["file_path"].as_str())
        .collect();

    // Source file should be present
    assert!(
        file_paths.contains(&"src/main.rs"),
        "src/main.rs should appear in diff hunks"
    );

    // Default-ignored files should NOT be present
    assert!(
        !file_paths.contains(&"Cargo.lock"),
        "Cargo.lock should be filtered out by default ignores"
    );
    assert!(
        !file_paths.contains(&"package-lock.json"),
        "package-lock.json should be filtered out by default ignores"
    );
    assert!(
        !file_paths.contains(&"dist/bundle.min.js"),
        "*.min.js should be filtered out by default ignores"
    );
}

#[test]
fn test_diff_json_respects_gitattributes_linguist_generated() {
    let repo = TestRepo::new();

    write_file(&repo, "src/app.ts", "export const app = 1;\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Set up .gitattributes with linguist-generated
    write_file(
        &repo,
        ".gitattributes",
        "generated/** linguist-generated=true\n",
    );
    write_file(
        &repo,
        "src/app.ts",
        "export const app = 1;\nexport const next = 2;\n",
    );
    write_file(
        &repo,
        "generated/schema.ts",
        "export const schema = {};\nexport const types = {};\n",
    );

    repo.git_ai(&["checkpoint", "mock_ai", "src/app.ts", "generated/schema.ts"])
        .unwrap();
    let commit = repo
        .stage_all_and_commit("modify source and generated files")
        .unwrap();

    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let file_paths: Vec<&str> = hunks
        .iter()
        .filter_map(|h| h["file_path"].as_str())
        .collect();

    assert!(
        file_paths.contains(&"src/app.ts"),
        "src/app.ts should appear in diff hunks"
    );
    assert!(
        !file_paths.contains(&"generated/schema.ts"),
        "generated/schema.ts should be filtered by linguist-generated"
    );
}

#[test]
fn test_diff_json_respects_git_ai_ignore_file() {
    let repo = TestRepo::new();

    write_file(&repo, "src/main.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Set up .git-ai-ignore
    write_file(&repo, ".git-ai-ignore", "docs/**\n*.pdf\n");
    write_file(&repo, "src/main.rs", "fn main() {}\nfn added() {}\n");
    write_file(&repo, "docs/guide.md", "# Guide\nLine 1\nLine 2\n");

    repo.git_ai(&["checkpoint", "mock_ai", "src/main.rs", "docs/guide.md"])
        .unwrap();
    let commit = repo.stage_all_and_commit("modify source and docs").unwrap();

    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let file_paths: Vec<&str> = hunks
        .iter()
        .filter_map(|h| h["file_path"].as_str())
        .collect();

    assert!(
        file_paths.contains(&"src/main.rs"),
        "src/main.rs should appear in diff hunks"
    );
    assert!(
        !file_paths.contains(&"docs/guide.md"),
        "docs/guide.md should be filtered by .git-ai-ignore"
    );
}

#[test]
fn test_diff_json_respects_union_of_all_ignore_sources() {
    let repo = TestRepo::new();

    write_file(&repo, "src/app.ts", "export const app = 1;\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Set up both .gitattributes and .git-ai-ignore
    write_file(
        &repo,
        ".gitattributes",
        "generated/** linguist-generated=true\n",
    );
    write_file(&repo, ".git-ai-ignore", "docs/**\n");

    write_file(
        &repo,
        "src/app.ts",
        "export const app = 1;\nexport const next = 2;\n",
    );
    write_file(
        &repo,
        "generated/out.ts",
        "export const gen = 1;\nexport const gen2 = 2;\n",
    );
    write_file(&repo, "docs/api.md", "# API\nendpoint 1\nendpoint 2\n");
    write_file(&repo, "Cargo.lock", "# lock\nline2\n");

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/app.ts",
        "generated/out.ts",
        "docs/api.md",
    ])
    .unwrap();
    let commit = repo.stage_all_and_commit("all ignore sources").unwrap();

    let json = diff_json(&repo, &["diff", "--json", &commit.commit_sha]);
    let hunks = json["hunks"].as_array().expect("hunks should be array");
    let file_paths: Vec<&str> = hunks
        .iter()
        .filter_map(|h| h["file_path"].as_str())
        .collect();

    // Only the source file should appear
    assert!(
        file_paths.contains(&"src/app.ts"),
        "src/app.ts should appear in diff hunks"
    );
    // All ignored sources should be filtered
    assert!(
        !file_paths.contains(&"generated/out.ts"),
        "generated/out.ts should be filtered by linguist-generated"
    );
    assert!(
        !file_paths.contains(&"docs/api.md"),
        "docs/api.md should be filtered by .git-ai-ignore"
    );
    assert!(
        !file_paths.contains(&"Cargo.lock"),
        "Cargo.lock should be filtered by default ignores"
    );
}

#[test]
fn test_diff_terminal_respects_ignore_patterns() {
    let repo = TestRepo::new();

    write_file(&repo, "src/main.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, ".git-ai-ignore", "docs/**\n");
    write_file(&repo, "src/main.rs", "fn main() {}\nfn added() {}\n");
    write_file(&repo, "docs/guide.md", "# Guide\nLine 1\n");
    write_file(&repo, "Cargo.lock", "# lock\nline2\n");

    repo.git_ai(&["checkpoint", "mock_ai", "src/main.rs", "docs/guide.md"])
        .unwrap();
    let commit = repo
        .stage_all_and_commit("ignored in terminal mode")
        .unwrap();

    let output = diff_raw(&repo, &["diff", &commit.commit_sha]);

    assert!(
        output.contains("src/main.rs"),
        "Terminal diff should show src/main.rs"
    );
    assert!(
        !output.contains("docs/guide.md"),
        "Terminal diff should not show .git-ai-ignore'd docs/guide.md"
    );
    assert!(
        !output.contains("Cargo.lock"),
        "Terminal diff should not show default-ignored Cargo.lock"
    );
}

reuse_tests_in_worktree!(
    test_diff_json_ignores_binary_files_in_output,
    test_diff_terminal_handles_binary_files_without_error,
    test_diff_json_with_only_binary_changes,
    test_diff_json_handles_non_utf8_file_content,
    test_diff_json_handles_mixed_utf8_and_binary_content,
    test_diff_json_handles_complex_utf8_characters,
    test_diff_terminal_handles_non_utf8_without_error,
    test_diff_json_respects_default_ignore_patterns,
    test_diff_json_respects_gitattributes_linguist_generated,
    test_diff_json_respects_git_ai_ignore_file,
    test_diff_json_respects_union_of_all_ignore_sources,
    test_diff_terminal_respects_ignore_patterns,
);
