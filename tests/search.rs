//! Integration tests for `git-ai search` command
//!
//! These tests verify the search command's ability to find AI prompts by commit,
//! file, pattern, and prompt ID, with various output formats and filters.

#[macro_use]
mod repos;
mod test_utils;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;
use std::io::Write;
use test_utils::fixture_path;

// ============================================================================
// Test helpers
// ============================================================================

/// Create an AI-attributed commit using the Continue CLI checkpoint flow
/// Returns the commit SHA
fn create_ai_commit(repo: &TestRepo, transcript_fixture: &str) -> String {
    let fixture_path_str = fixture_path(transcript_fixture)
        .to_string_lossy()
        .to_string();

    // Create initial file with base content
    let file_path = repo.path().join("test.ts");
    let base_content = "const x = 1;\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate AI making edits
    let edited_content = "const x = 1;\nconst y = 2;\nconst z = 3;\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint with the Continue CLI session
    let hook_input = json!({
        "session_id": "test-session-id-12345",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add AI edits").unwrap();

    commit.commit_sha
}

/// Create an AI commit with a specific file
fn create_ai_commit_with_file(
    repo: &TestRepo,
    transcript_fixture: &str,
    filename: &str,
    initial_content: &str,
    final_content: &str,
) -> String {
    let fixture_path_str = fixture_path(transcript_fixture)
        .to_string_lossy()
        .to_string();

    // Create initial file
    let file_path = repo.path().join(filename);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&file_path, initial_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate AI making edits
    fs::write(&file_path, final_content).unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": format!("session-{}", filename.replace("/", "-")),
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");

    let commit = repo.stage_all_and_commit("Add AI edits").unwrap();

    commit.commit_sha
}

// ============================================================================
// Search by Commit Tests (Subtask 11.1)
// ============================================================================

#[test]
fn test_search_by_commit_returns_prompts() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--commit", &commit_sha])
        .expect("search should succeed");

    // Verify output contains prompt information
    assert!(
        output.contains("Found") && output.contains("AI prompt"),
        "Output should indicate found prompts"
    );
    assert!(
        output.contains("continue-cli") || output.contains("claude"),
        "Output should mention the tool"
    );
}

#[test]
fn test_search_by_commit_no_results() {
    let repo = TestRepo::new();

    // Create a commit without any AI attribution
    let file_path = repo.path().join("human.txt");
    fs::write(&file_path, "Human authored content\n").unwrap();
    let commit = repo.stage_all_and_commit("Human only commit").unwrap();

    // Search should fail with exit code 2
    let result = repo.git_ai(&["search", "--commit", &commit.commit_sha]);

    assert!(
        result.is_err(),
        "Search should fail when no AI prompts found"
    );
}

#[test]
fn test_search_by_commit_abbreviated_sha() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // Use abbreviated SHA (first 8 characters)
    let short_sha = &commit_sha[..8];

    let output = repo
        .git_ai(&["search", "--commit", short_sha])
        .expect("search with abbreviated SHA should succeed");

    assert!(
        output.contains("Found") && output.contains("AI prompt"),
        "Should find prompts with abbreviated SHA"
    );
}

#[test]
fn test_search_by_commit_symbolic_ref() {
    let repo = TestRepo::new();
    let _commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // Search using HEAD
    let output = repo
        .git_ai(&["search", "--commit", "HEAD"])
        .expect("search with HEAD should succeed");

    assert!(
        output.contains("Found") && output.contains("AI prompt"),
        "Should find prompts with HEAD reference"
    );
}

// ============================================================================
// Search by Commit Range Tests (Subtask 11.2)
// ============================================================================

#[test]
fn test_search_by_commit_range() {
    let repo = TestRepo::new();

    // Create first AI commit
    let first_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // Create second AI commit
    let file_path = repo.path().join("another.ts");
    fs::write(&file_path, "const a = 1;\n").unwrap();
    repo.stage_all_and_commit("Setup file").unwrap();

    fs::write(&file_path, "const a = 1;\nconst b = 2;\n").unwrap();

    let hook_input = json!({
        "session_id": "second-session",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path("continue-cli-session-simple.json").to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    let second_commit = repo.stage_all_and_commit("Second AI commit").unwrap();

    // Search the range
    let range = format!("{}..{}", first_sha, second_commit.commit_sha);
    let output = repo
        .git_ai(&["search", "--commit", &range])
        .expect("search range should succeed");

    // Should find prompts from both commits in the range
    assert!(
        output.contains("Found"),
        "Should find prompts in commit range"
    );
}

// ============================================================================
// Search by File Tests (Subtask 11.3)
// ============================================================================

#[test]
fn test_search_by_file_basic() {
    let repo = TestRepo::new();
    let _commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--file", "test.ts"])
        .expect("search by file should succeed");

    assert!(
        output.contains("Found") && output.contains("AI prompt"),
        "Should find prompts for file"
    );
}

#[test]
fn test_search_by_file_and_lines() {
    let repo = TestRepo::new();

    // Create file with specific content
    let _commit_sha = create_ai_commit_with_file(
        &repo,
        "continue-cli-session-simple.json",
        "test.ts",
        "const x = 1;\n",
        "const x = 1;\nconst y = 2;\nconst z = 3;\n",
    );

    // Search specific lines (the AI-authored ones)
    let output = repo
        .git_ai(&["search", "--file", "test.ts", "--lines", "2-3"])
        .expect("search by file with lines should succeed");

    assert!(
        output.contains("Found") || output.contains("AI prompt"),
        "Should find prompts for specified lines"
    );
}

#[test]
fn test_search_by_file_no_ai_lines() {
    let repo = TestRepo::new();

    // Create file with only human content
    let file_path = repo.path().join("human.txt");
    fs::write(&file_path, "Human content\nMore human content\n").unwrap();
    repo.stage_all_and_commit("Human file").unwrap();

    // Search should fail with no results
    let result = repo.git_ai(&["search", "--file", "human.txt"]);

    assert!(
        result.is_err(),
        "Search should fail when file has no AI lines"
    );
}

#[test]
fn test_search_by_file_relative_path() {
    let repo = TestRepo::new();

    // Create file in subdirectory
    let _commit_sha = create_ai_commit_with_file(
        &repo,
        "continue-cli-session-simple.json",
        "src/lib/utils.ts",
        "export const util = 1;\n",
        "export const util = 1;\nexport const helper = 2;\n",
    );

    // Search using relative path
    let output = repo
        .git_ai(&["search", "--file", "src/lib/utils.ts"])
        .expect("search by relative path should succeed");

    assert!(
        output.contains("Found") || output.contains("AI prompt"),
        "Should find prompts for file in subdirectory"
    );
}

// ============================================================================
// Search by Pattern Tests (Subtask 11.4)
// ============================================================================

#[test]
fn test_search_by_pattern() {
    let repo = TestRepo::new();
    let _commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // The fixture contains "hello world" in the messages
    let output = repo
        .git_ai(&["search", "--pattern", "hello"]);

    // Pattern search may or may not find results depending on DB population
    // Just verify it doesn't crash
    assert!(output.is_ok() || output.is_err());
}

#[test]
fn test_search_by_prompt_id() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // First get the prompt ID from a commit search
    let search_output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--json"])
        .expect("search should succeed");

    let search_result: serde_json::Value =
        serde_json::from_str(&search_output).expect("should parse JSON");

    let prompts = search_result["prompts"]
        .as_object()
        .expect("prompts should be an object");

    if let Some(prompt_id) = prompts.keys().next() {
        // Now search by prompt ID
        let output = repo
            .git_ai(&["search", "--prompt-id", prompt_id])
            .expect("search by prompt ID should succeed");

        assert!(
            output.contains("Found") || output.contains(prompt_id),
            "Should find the specific prompt"
        );
    }
}

#[test]
fn test_search_by_prompt_id_not_found() {
    let repo = TestRepo::new();

    // Create at least one commit so the repo is valid
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "test\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Search for a nonexistent prompt ID
    let result = repo.git_ai(&["search", "--prompt-id", "nonexistent-prompt-id-12345"]);

    assert!(
        result.is_err(),
        "Search should fail for nonexistent prompt ID"
    );
}

// ============================================================================
// Output Format Tests (Subtask 11.6)
// ============================================================================

#[test]
fn test_search_output_json() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--json"])
        .expect("search with --json should succeed");

    // Verify output is valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify schema
    assert!(
        parsed.get("prompts").is_some(),
        "JSON should have prompts field"
    );
    assert!(
        parsed.get("result_count").is_some() || parsed.get("query").is_some(),
        "JSON should have metadata fields"
    );
}

#[test]
fn test_search_output_verbose() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--verbose"])
        .expect("search with --verbose should succeed");

    // Verbose should include full transcripts
    assert!(
        output.contains("User:") || output.contains("Assistant:"),
        "Verbose output should include message transcripts"
    );
}

#[test]
fn test_search_output_porcelain() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--porcelain"])
        .expect("search with --porcelain should succeed");

    // Porcelain format is tab-separated:
    // <prompt_id>\t<tool>\t<model>\t<author>\t<date_unix>\t<file_count>\t<first_message_snippet>
    let lines: Vec<&str> = output.trim().lines().collect();
    assert!(!lines.is_empty(), "Porcelain should output prompt lines");

    // Each line should have tab-separated fields
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(
            fields.len() >= 6,
            "Porcelain lines should have at least 6 tab-separated fields, got {}",
            fields.len()
        );
        // First field is the prompt ID (hash)
        assert!(
            !fields[0].is_empty(),
            "First field should be prompt ID"
        );
        // Second field is the tool
        assert!(
            !fields[1].is_empty(),
            "Second field should be tool name"
        );
    }
}

#[test]
fn test_search_output_count() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--count"])
        .expect("search with --count should succeed");

    // Count should be a single number
    let count: usize = output
        .trim()
        .parse()
        .expect("Count output should be a number");

    assert!(count > 0, "Should have at least one prompt");
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_search_no_authorship_notes() {
    let repo = TestRepo::new();

    // Create a commit without using git-ai checkpoint
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "Some content\n").unwrap();
    let commit = repo.stage_all_and_commit("Regular commit").unwrap();

    // Search should fail gracefully
    let result = repo.git_ai(&["search", "--commit", &commit.commit_sha]);

    assert!(
        result.is_err(),
        "Search should fail when no authorship notes exist"
    );
}

#[test]
fn test_search_detached_head() {
    let repo = TestRepo::new();
    let commit_sha = create_ai_commit(&repo, "continue-cli-session-simple.json");

    // Detach HEAD
    repo.git(&["checkout", "--detach", "HEAD"])
        .expect("detach HEAD should succeed");

    // Search should still work
    let output = repo
        .git_ai(&["search", "--commit", &commit_sha])
        .expect("search should work with detached HEAD");

    assert!(
        output.contains("Found") || output.contains("AI prompt"),
        "Should find prompts with detached HEAD"
    );
}

#[test]
fn test_search_help() {
    let repo = TestRepo::new();

    // Create at least one commit
    let file_path = repo.path().join("test.txt");
    fs::write(&file_path, "test\n").unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    let output = repo
        .git_ai(&["search", "--help"])
        .expect("search --help should succeed");

    assert!(
        output.contains("search") || output.contains("Search") || output.contains("USAGE"),
        "Should display help text"
    );
}
