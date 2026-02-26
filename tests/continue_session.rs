//! Integration tests for `git-ai continue` command
//!
//! These tests verify the continue command's context formatting, message filtering,
//! secret redaction, truncation, and output modes.
//!
//! Note: tests/continue_cli.rs tests the Continue CLI agent preset checkpoint flow.
//! This file tests the `git-ai continue` command itself.

#[macro_use]
mod repos;
mod test_utils;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use test_utils::fixture_path;

// ============================================================================
// Test helpers
// ============================================================================

/// Create an AI-attributed commit using the Continue CLI checkpoint flow
/// Returns the commit SHA and sets up the repo with AI-authored lines
fn create_ai_commit_with_transcript(
    repo: &TestRepo,
    transcript_fixture: &str,
) -> (String, std::path::PathBuf) {
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

    (commit.commit_sha, file_path)
}

fn create_external_diff_helper_script(repo: &TestRepo, marker: &str) -> std::path::PathBuf {
    let helper_path = repo.path().join(format!("continue-ext-helper-{marker}.sh"));
    fs::write(&helper_path, format!("#!/bin/sh\necho {marker}\nexit 0\n"))
        .expect("should write external diff helper");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&helper_path)
            .expect("helper metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&helper_path, perms).expect("helper should be executable");
    }
    helper_path
}

// ============================================================================
// Context Block Tests (Subtask 12.1)
// ============================================================================

#[test]
fn test_continue_by_commit_outputs_context() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    // Run continue command
    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue command should succeed");

    // Verify output contains expected sections
    assert!(
        output.contains("# Restored AI Session Context"),
        "Output should contain preamble header"
    );
    assert!(
        output.contains("## Session"),
        "Output should contain session headers"
    );
    assert!(
        output.contains("### Conversation"),
        "Output should contain conversation section"
    );
    assert!(
        output.contains("**User**:") || output.contains("**Assistant**:"),
        "Output should contain message role labels"
    );
}

#[test]
fn test_continue_by_file_outputs_context() {
    let repo = TestRepo::new();
    let (_, file_path) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    // Run continue command by file
    let output = repo
        .git_ai(&[
            "continue",
            "--file",
            file_path.file_name().unwrap().to_str().unwrap(),
        ])
        .expect("continue command should succeed");

    // Verify output contains expected sections
    assert!(
        output.contains("# Restored AI Session Context"),
        "Output should contain preamble header"
    );
    assert!(
        output.contains("### Conversation"),
        "Output should contain conversation section"
    );
}

#[test]
fn test_continue_by_prompt_id() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    // First, search to get the prompt ID
    let search_output = repo
        .git_ai(&["search", "--commit", &commit_sha, "--json"])
        .expect("search should succeed");

    // Parse JSON to extract prompt ID
    let search_result: serde_json::Value =
        serde_json::from_str(&search_output).expect("should parse search JSON");

    // prompts is a map (object), not an array
    let prompts = search_result["prompts"]
        .as_object()
        .expect("prompts should be an object");

    assert!(!prompts.is_empty(), "Should have at least one prompt");

    // Get the first prompt ID (the key in the map)
    let prompt_id = prompts.keys().next().expect("should have a prompt").clone();

    // Now test continue with prompt ID
    let output = repo
        .git_ai(&["continue", "--prompt-id", &prompt_id])
        .expect("continue with prompt-id should succeed");

    assert!(
        output.contains("# Restored AI Session Context"),
        "Output should contain preamble"
    );
}

#[test]
fn test_continue_context_preamble() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue should succeed");

    // Verify output starts with the expected preamble
    assert!(
        output.starts_with("# Restored AI Session Context"),
        "Output should start with the preamble header"
    );

    // Verify preamble description is present
    assert!(
        output.contains("restored from git-ai prompt history"),
        "Output should contain preamble description"
    );
}

#[test]
fn test_continue_context_ends_with_invitation() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue should succeed");

    // Verify output ends with the follow-up invitation
    assert!(
        output.contains("You can now ask follow-up questions"),
        "Output should contain follow-up invitation"
    );
}

#[test]
fn test_continue_context_includes_source_info() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue should succeed");

    // Verify source section with commit info is present
    assert!(
        output.contains("## Source"),
        "Output should contain Source section"
    );
    assert!(
        output.contains("**Commit**:"),
        "Output should contain commit info"
    );
    // Check that short SHA is present (first 8 chars)
    assert!(
        output.contains(&commit_sha[..8]),
        "Output should contain short commit SHA"
    );
}

// ============================================================================
// Message Filtering Tests (Subtask 12.2)
// ============================================================================

#[test]
fn test_continue_excludes_tool_use_messages() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue should succeed");

    // The fixture has Read tool calls - verify they're not in output
    // ToolUse messages should be filtered out
    assert!(
        !output.contains("ToolUse"),
        "Output should not contain ToolUse markers"
    );

    // But regular content should still be present
    assert!(
        output.contains("**User**:") || output.contains("**Assistant**:"),
        "Output should still contain User/Assistant messages"
    );
}

#[test]
fn test_continue_includes_user_and_assistant() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha])
        .expect("continue should succeed");

    // Verify User messages are present
    assert!(
        output.contains("**User**:"),
        "Output should contain User messages"
    );

    // Verify Assistant messages are present
    assert!(
        output.contains("**Assistant**:"),
        "Output should contain Assistant messages"
    );
}

#[test]
fn test_continue_ignores_git_external_diff_env_for_internal_show() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let marker = "CONTINUE_EXT_DIFF_MARKER";
    let helper_path = create_external_diff_helper_script(&repo, marker);
    let helper_path_str = helper_path
        .to_str()
        .expect("helper path must be valid UTF-8")
        .replace('\\', "/")
        .to_string();

    // Sanity check: proxied git show honors external diff helper when explicitly enabled.
    let proxied_show = repo
        .git_with_env(
            &["show", "--ext-diff", "--format=", &commit_sha],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
            None,
        )
        .expect("proxied git show should succeed");
    assert!(
        proxied_show.contains(marker),
        "proxied git show should honor GIT_EXTERNAL_DIFF, got:\n{}",
        proxied_show
    );

    // Internal git show usage in `git-ai continue` must ignore external diff override.
    let output = repo
        .git_ai_with_env(
            &["continue", "--commit", &commit_sha],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
        )
        .expect("git-ai continue should succeed with external diff env configured");

    assert!(
        !output.contains(marker),
        "git-ai continue should ignore GIT_EXTERNAL_DIFF for internal show calls, got:\n{}",
        output
    );
    assert!(
        output.contains("diff --git"),
        "git-ai continue should still include normal patch output, got:\n{}",
        output
    );
}

// ============================================================================
// Truncation Tests (Subtask 12.4)
// ============================================================================

/// Helper to create a large transcript fixture with many messages
fn create_large_transcript_fixture(num_messages: usize) -> tempfile::NamedTempFile {
    let mut history = Vec::new();
    for i in 0..num_messages {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        let content = format!("Message number {} from {}", i, role);
        history.push(json!({
            "message": {
                "role": role,
                "content": content
            },
            "contextItems": []
        }));
    }

    let session = json!({
        "sessionId": "large-session",
        "title": "Large Session",
        "workspaceDirectory": "/test",
        "history": history
    });

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(serde_json::to_string(&session).unwrap().as_bytes())
        .unwrap();
    temp_file
}

#[test]
fn test_continue_max_messages_truncation() {
    let repo = TestRepo::new();

    // Create a large transcript
    let large_fixture = create_large_transcript_fixture(100);
    let fixture_path_str = large_fixture.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "large-session",
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

    // Run continue with max-messages limit
    let output = repo
        .git_ai(&[
            "continue",
            "--commit",
            &commit.commit_sha,
            "--max-messages",
            "10",
        ])
        .expect("continue should succeed");

    // Count messages in output (count **User**: and **Assistant**: markers)
    let user_count = output.matches("**User**:").count();
    let assistant_count = output.matches("**Assistant**:").count();
    let total_messages = user_count + assistant_count;

    assert!(
        total_messages <= 10,
        "Should have at most 10 messages, got {}",
        total_messages
    );
}

#[test]
fn test_continue_truncation_notice() {
    let repo = TestRepo::new();

    // Create a large transcript
    let large_fixture = create_large_transcript_fixture(50);
    let fixture_path_str = large_fixture.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "large-session",
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

    // Run continue with max-messages limit
    let output = repo
        .git_ai(&[
            "continue",
            "--commit",
            &commit.commit_sha,
            "--max-messages",
            "10",
        ])
        .expect("continue should succeed");

    // Verify truncation notice is present
    assert!(
        output.contains("earlier messages omitted"),
        "Output should contain truncation notice when messages are omitted"
    );
}

// ============================================================================
// Output Mode Tests (Subtask 12.5)
// ============================================================================

#[test]
fn test_continue_json_output() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha, "--json"])
        .expect("continue with --json should succeed");

    // Verify output is valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify expected structure
    assert!(
        parsed.get("prompts").is_some(),
        "JSON should contain prompts field"
    );

    // Verify prompts is an array
    assert!(
        parsed["prompts"].is_array(),
        "prompts field should be an array"
    );
}

#[test]
fn test_continue_json_schema() {
    let repo = TestRepo::new();
    let (commit_sha, _) =
        create_ai_commit_with_transcript(&repo, "continue-cli-session-simple.json");

    let output = repo
        .git_ai(&["continue", "--commit", &commit_sha, "--json"])
        .expect("continue with --json should succeed");

    let parsed: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify source field exists (can be null)
    assert!(
        parsed.get("source").is_some(),
        "JSON should contain source field"
    );

    // If source is not null, verify its schema
    if !parsed["source"].is_null() {
        let source = &parsed["source"];
        assert!(source.get("sha").is_some(), "source should have sha");
        assert!(source.get("author").is_some(), "source should have author");
        assert!(
            source.get("message").is_some(),
            "source should have message"
        );
    }

    // Verify prompts array structure
    let prompts = parsed["prompts"]
        .as_array()
        .expect("prompts should be array");
    if !prompts.is_empty() {
        let prompt = &prompts[0];
        assert!(prompt.get("id").is_some(), "prompt should have id");
        assert!(prompt.get("tool").is_some(), "prompt should have tool");
        assert!(prompt.get("model").is_some(), "prompt should have model");
        assert!(
            prompt.get("messages").is_some(),
            "prompt should have messages"
        );

        // Verify messages structure
        if let Some(messages) = prompt["messages"].as_array() {
            if !messages.is_empty() {
                let msg = &messages[0];
                assert!(msg.get("role").is_some(), "message should have role");
                assert!(msg.get("text").is_some(), "message should have text");
            }
        }
    }
}

#[test]
fn test_continue_no_results() {
    let repo = TestRepo::new();

    // Create a commit without any AI attribution
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    let commit = repo.stage_all_and_commit("Human only commit").unwrap();

    // Run continue - should fail with exit code 2
    let result = repo.git_ai(&["continue", "--commit", &commit.commit_sha]);

    assert!(
        result.is_err(),
        "continue should fail when no AI prompts found"
    );

    let error = result.unwrap_err();
    assert!(
        error.contains("No AI prompt history found") || error.contains("no prompts"),
        "Error should indicate no prompts found"
    );
}

// ============================================================================
// Secret Redaction Tests (Subtask 12.3)
// ============================================================================

#[test]
fn test_continue_redacts_secrets() {
    let repo = TestRepo::new();

    // Create a session with a high-entropy API key-like string
    // This follows AWS access key pattern - high entropy, alphanumeric
    let secret_key = "AKIAIOSFODNN7EXAMPLEKEY123456789012345";
    let session_with_secret = json!({
        "sessionId": "secret-session",
        "title": "Secret Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": {
                    "role": "user",
                    "content": format!("Here's my API key: {}", secret_key)
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": "I see you've shared an API key. Let me help you with that."
                },
                "contextItems": []
            }
        ]
    });

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(
            serde_json::to_string(&session_with_secret)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let fixture_path_str = temp_file.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "secret-session",
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

    // Run continue
    let output = repo
        .git_ai(&["continue", "--commit", &commit.commit_sha])
        .expect("continue should succeed");

    // The full secret should NOT appear in the output
    assert!(
        !output.contains(secret_key),
        "Output should NOT contain the full secret key"
    );

    // The redaction marker (********) should appear
    assert!(
        output.contains("********"),
        "Output should contain redaction markers"
    );

    // The prefix of the secret (first 4 chars) might be visible
    // per the redaction format: prefix********suffix
    assert!(
        output.contains("AKIA"),
        "Output should contain visible prefix of redacted secret"
    );
}

#[test]
fn test_continue_redacts_before_format() {
    let repo = TestRepo::new();

    // Use the same high-entropy pattern that worked in the previous test
    // AWS-style key pattern with high entropy
    let secret_token = "AKIAZ5MNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTU";
    let session_with_secret = json!({
        "sessionId": "secret-format-session",
        "title": "Secret Format Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": {
                    "role": "user",
                    "content": format!("My secret token is: {}", secret_token)
                },
                "contextItems": []
            }
        ]
    });

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(
            serde_json::to_string(&session_with_secret)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let fixture_path_str = temp_file.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "secret-format-session",
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

    // Test default output format
    let output_default = repo
        .git_ai(&["continue", "--commit", &commit.commit_sha])
        .expect("continue should succeed");

    // Test JSON output format
    let output_json = repo
        .git_ai(&["continue", "--commit", &commit.commit_sha, "--json"])
        .expect("continue with --json should succeed");

    // Full secret should NOT appear in either format
    assert!(
        !output_default.contains(secret_token),
        "Default output should NOT contain secret"
    );
    assert!(
        !output_json.contains(secret_token),
        "JSON output should NOT contain secret"
    );

    // Both outputs should contain redaction markers
    assert!(
        output_default.contains("********"),
        "Default output should contain redaction"
    );
    assert!(
        output_json.contains("********"),
        "JSON output should contain redaction"
    );
}

// ============================================================================
// Edge Case Tests (Subtask 12.5 continued)
// ============================================================================

#[test]
fn test_continue_unicode_content() {
    let repo = TestRepo::new();

    // Create fixture with unicode content
    let unicode_session = json!({
        "sessionId": "unicode-session",
        "title": "Unicode Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": {
                    "role": "user",
                    "content": "请帮我写代码 (Chinese) 🚀 مرحبا (Arabic)"
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": "Here's code with emoji: 🎉 and Japanese: こんにちは"
                },
                "contextItems": []
            }
        ]
    });

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(serde_json::to_string(&unicode_session).unwrap().as_bytes())
        .unwrap();
    let fixture_path_str = temp_file.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "unicode-session",
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

    // Run continue
    let output = repo
        .git_ai(&["continue", "--commit", &commit.commit_sha])
        .expect("continue should succeed with unicode content");

    // Verify unicode content is preserved
    assert!(
        output.contains("请帮我写代码"),
        "Chinese text should be preserved"
    );
    assert!(output.contains("🚀"), "Emoji should be preserved");
    assert!(output.contains("مرحبا"), "Arabic text should be preserved");
    assert!(
        output.contains("こんにちは"),
        "Japanese text should be preserved"
    );
}

#[test]
fn test_continue_empty_transcript() {
    let repo = TestRepo::new();

    // Create fixture with empty history
    let empty_session = json!({
        "sessionId": "empty-session",
        "title": "Empty Session",
        "workspaceDirectory": "/test",
        "history": []
    });

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(serde_json::to_string(&empty_session).unwrap().as_bytes())
        .unwrap();
    let fixture_path_str = temp_file.path().to_string_lossy().to_string();

    // Create initial file
    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Make AI edits
    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    // Run checkpoint
    let hook_input = json!({
        "session_id": "empty-session",
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

    // Run continue - should handle empty transcript gracefully
    let output = repo
        .git_ai(&["continue", "--commit", &commit.commit_sha])
        .expect("continue should handle empty transcript");

    // Should still produce valid output structure
    assert!(
        output.contains("# Restored AI Session Context"),
        "Output should still have preamble"
    );
}
