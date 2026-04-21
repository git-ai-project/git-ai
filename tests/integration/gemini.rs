use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;

fn parse_gemini(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("gemini")?.parse(hook_input, "t_test")
}

#[test]
fn test_parse_example_gemini_json_with_model() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, model) = transcript_readers::read_gemini_json(fixture.as_path())
        .expect("Failed to parse Gemini JSON");

    assert!(!transcript.messages().is_empty());

    assert!(model.is_some());
    let model_name = model.unwrap();
    assert_eq!(model_name, "gemini-2.5-flash");
}

#[test]
fn test_gemini_parses_user_messages() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = transcript_readers::read_gemini_json(fixture.as_path())
        .expect("Failed to parse Gemini JSON");

    let user_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .collect::<Vec<_>>();

    assert_eq!(
        user_messages.len(),
        1,
        "Should have exactly one user message"
    );

    if let Message::User { text, timestamp } = user_messages[0] {
        assert!(text.contains("add another hello bob console log"));
        assert_eq!(timestamp.as_ref().unwrap(), "2025-12-06T18:25:18.042Z");
    }
}

#[test]
fn test_gemini_parses_assistant_messages() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = transcript_readers::read_gemini_json(fixture.as_path())
        .expect("Failed to parse Gemini JSON");

    let assistant_messages: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .collect();

    assert!(
        !assistant_messages.is_empty(),
        "Should have at least one assistant message"
    );

    if let Message::Assistant { text, .. } = assistant_messages[0] {
        assert!(text.contains("I will add"));
    }
}

#[test]
fn test_gemini_parses_tool_calls() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = transcript_readers::read_gemini_json(fixture.as_path())
        .expect("Failed to parse Gemini JSON");

    let tool_uses: Vec<&Message> = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolUse { .. }))
        .collect();

    assert!(!tool_uses.is_empty(), "Should have at least one tool call");

    for tool_use in &tool_uses {
        if let Message::ToolUse { name, input, .. } = tool_use {
            assert!(!name.is_empty());
            assert!(input.is_object() || input.is_string());
        }
    }

    let replace_tools: Vec<&Message> = tool_uses
        .iter()
        .filter(|m| {
            if let Message::ToolUse { name, .. } = *m {
                name == "replace"
            } else {
                false
            }
        })
        .copied()
        .collect();

    assert!(
        !replace_tools.is_empty(),
        "Should have at least one 'replace' tool call"
    );
}

#[test]
fn test_gemini_parses_tool_call_args() {
    let fixture = fixture_path("gemini-session-simple.json");
    let (transcript, _model) = transcript_readers::read_gemini_json(fixture.as_path())
        .expect("Failed to parse Gemini JSON");

    let replace_tool = transcript
        .messages()
        .iter()
        .find(|m| {
            if let Message::ToolUse { name, .. } = m {
                name == "replace"
            } else {
                false
            }
        })
        .expect("Should find a replace tool call");

    if let Message::ToolUse { input, .. } = replace_tool
        && let Some(args_obj) = input.as_object()
    {
        assert!(
            args_obj.contains_key("file_path") || args_obj.contains_key("old_string"),
            "Tool call args should contain file_path or old_string"
        );
    }
}

#[test]
fn test_gemini_handles_empty_content() {
    let sample = r##"{
        "sessionId": "test-session",
        "projectHash": "test-hash",
        "startTime": "2025-12-06T18:25:18.042Z",
        "lastUpdated": "2025-12-06T18:25:18.042Z",
        "messages": [
            {
                "id": "msg1",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "user",
                "content": "Hello"
            },
            {
                "id": "msg2",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "gemini",
                "content": "",
                "model": "gemini-2.5-flash"
            },
            {
                "id": "msg3",
                "timestamp": "2025-12-06T18:25:18.042Z",
                "type": "gemini",
                "content": "Response text",
                "model": "gemini-2.5-flash"
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, model) = transcript_readers::read_gemini_json(Path::new(temp_path))
        .expect("Failed to parse Gemini JSON");

    let user_count = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    let assistant_count = transcript
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .count();

    assert_eq!(user_count, 1);
    assert_eq!(assistant_count, 1, "Should skip empty content");
    assert_eq!(model, Some("gemini-2.5-flash".to_string()));
}

#[test]
fn test_gemini_skips_unknown_message_types() {
    let sample = r##"{
        "sessionId": "test-session",
        "projectHash": "test-hash",
        "startTime": "2025-12-06T18:25:18.042Z",
        "lastUpdated": "2025-12-06T18:25:18.042Z",
        "messages": [
            { "id": "msg1", "timestamp": "2025-12-06T18:25:18.042Z", "type": "user", "content": "Hello" },
            { "id": "msg2", "timestamp": "2025-12-06T18:25:18.042Z", "type": "info", "content": "Info message" },
            { "id": "msg3", "timestamp": "2025-12-06T18:25:18.042Z", "type": "error", "content": "Error message" },
            { "id": "msg4", "timestamp": "2025-12-06T18:25:18.042Z", "type": "gemini", "content": "Response", "model": "gemini-2.5-flash" }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let (transcript, _model) = transcript_readers::read_gemini_json(Path::new(temp_path))
        .expect("Failed to parse Gemini JSON");

    assert_eq!(transcript.messages().len(), 2);
}

#[test]
fn test_gemini_preset_extracts_edited_filepath() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts")),
                "Should contain edited filepath"
            );
        }
        _ => panic!("Expected PostFileEdit for AfterTool"),
    }
}

#[test]
fn test_gemini_preset_no_filepath_when_tool_input_missing() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(
                e.file_paths.is_empty(),
                "edited_filepaths should be empty when tool_input is missing"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_gemini_preset_human_checkpoint() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "BeforeTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts")),
                "Should have will_edit_filepaths"
            );
        }
        _ => panic!("Expected PreFileEdit for BeforeTool"),
    }
}

#[test]
fn test_gemini_preset_ai_checkpoint() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(e.transcript_source.is_some(), "Should have transcript");
            assert!(!e.file_paths.is_empty(), "Should have edited_filepaths");
        }
        _ => panic!("Expected PostFileEdit for AfterTool"),
    }
}

#[test]
fn test_gemini_preset_extracts_model() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            // Model comes from transcript which is lazily loaded, so it's "unknown" at parse time
            assert_eq!(e.context.agent_id.model, "unknown");
            assert_eq!(e.context.agent_id.tool, "gemini");
            assert_eq!(
                e.context.agent_id.id,
                "18f475c0-690f-4bc9-b84e-88a0a1e9518f"
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_gemini_preset_stores_transcript_path_in_metadata() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let events = parse_gemini(&hook_input).expect("Failed to run GeminiPreset");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(
                e.context.metadata.get("transcript_path"),
                Some(&"tests/fixtures/gemini-session-simple.json".to_string())
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_gemini_preset_handles_missing_transcript_path() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f"
    })
    .to_string();

    let result = parse_gemini(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("transcript_path not found")
    );
}

#[test]
fn test_gemini_preset_handles_invalid_json() {
    let result = parse_gemini("{ invalid json }");
    assert!(result.is_err());
}

#[test]
fn test_gemini_preset_handles_missing_session_id() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "transcript_path": "tests/fixtures/gemini-session-simple.json"
    })
    .to_string();

    let result = parse_gemini(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("session_id not found")
    );
}

#[test]
fn test_gemini_preset_handles_missing_file() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "AfterTool",
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "transcript_path": "tests/fixtures/nonexistent.json"
    })
    .to_string();

    let result = parse_gemini(&hook_input);
    // Should handle missing file gracefully
    assert!(result.is_ok());
    let events = result.unwrap();
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_gemini_e2e_with_attribution() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();

    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let file_path = repo.path().join("src/index.ts");
    let base_content = "console.log('Bonjour');\n\nconsole.log('hello world');\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let edited_content =
        "console.log('Bonjour');\n\nconsole.log('hello world');\nconsole.log('hello bob');\n";
    fs::write(&file_path, edited_content).unwrap();

    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("Add gemini edits").unwrap();

    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('Bonjour');".human(),
        "".human(),
        "console.log('hello world');".human(),
        "console.log('hello bob');".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
    assert!(!commit.authorship_log.metadata.sessions.is_empty());

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have at least one session record");

    assert!(!session_record.messages.is_empty());
    assert_eq!(session_record.agent_id.model, "gemini-2.5-flash");
}

#[test]
fn test_gemini_e2e_human_checkpoint() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();

    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let file_path = repo.path().join("src/index.ts");
    fs::write(&file_path, "console.log('hello');\n").unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "BeforeTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('human edit');\n",
    )
    .unwrap();

    let commit = repo.stage_all_and_commit("Human edit").unwrap();

    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('human edit');".human(),
    ]);

    assert_eq!(commit.authorship_log.attestations.len(), 0);
}

#[test]
fn test_gemini_e2e_multiple_tool_calls() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();

    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "const x = 1;\nconst y = 2;\nconst z = 3;\n").unwrap();

    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("Add multiple lines").unwrap();

    let mut file = repo.filename("test.ts");
    file.assert_lines_and_blame(crate::lines![
        "const x = 1;".human(),
        "const y = 2;".ai(),
        "const z = 3;".ai(),
    ]);

    assert!(!commit.authorship_log.attestations.is_empty());
}

#[test]
fn test_gemini_e2e_with_resync() {
    use tempfile::TempDir;

    let repo = TestRepo::new();
    let fixture_path_original = fixture_path("gemini-session-simple.json");

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let temp_session_path = temp_dir.path().join("modified_gemini_session.json");

    fs::copy(&fixture_path_original, &temp_session_path).expect("Failed to copy session file");

    let mut session_content: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&temp_session_path).unwrap()).unwrap();

    if let Some(messages) = session_content
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
    {
        let new_message = json!({
            "id": "new-msg-id",
            "timestamp": "2025-12-06T18:30:00.000Z",
            "type": "gemini",
            "content": "RESYNC_TEST_MESSAGE: This message was added after checkpoint",
            "model": "gemini-2.5-flash"
        });
        messages.push(new_message);
    }

    fs::write(
        &temp_session_path,
        serde_json::to_string_pretty(&session_content).unwrap(),
    )
    .expect("Failed to write modified session");

    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_original.to_string_lossy().to_string()
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    let temp_session_path_str = temp_session_path.to_string_lossy().to_string();
    repo.git(&["add", "-A"]).expect("add --all should succeed");
    let commit = repo
        .commit_with_env(
            "Add gemini edits",
            &[("GIT_AI_GEMINI_SESSION_PATH", &temp_session_path_str)],
            None,
        )
        .unwrap();

    let mut file = repo.filename("test.ts");
    file.assert_lines_and_blame(crate::lines!["const x = 1;".human(), "const y = 2;".ai(),]);

    assert!(!commit.authorship_log.metadata.sessions.is_empty());

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have at least one session record");

    assert!(!session_record.messages.is_empty());
}

#[test]
fn test_gemini_e2e_partial_staging() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();

    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "line1\nline2\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "line1\nline2\nai_line3\nai_line4\n").unwrap();

    repo.git(&["add", "test.ts"]).unwrap();

    fs::write(&file_path, "line1\nline2\nai_line3\nai_line4\nai_line5\n").unwrap();

    let hook_input = json!({
        "session_id": "18f475c0-690f-4bc9-b84e-88a0a1e9518f",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "AfterTool",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.commit("Partial staging").unwrap();

    assert!(!commit.authorship_log.attestations.is_empty());

    let mut file = repo.filename("test.ts");
    file.assert_committed_lines(crate::lines![
        "line1".human(),
        "line2".human(),
        "ai_line3".ai(),
        "ai_line4".ai(),
    ]);
}

#[test]
fn test_gemini_preset_bash_tool_aftertool_detects_changes() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("gemini-session-simple.json")
        .to_string_lossy()
        .to_string();
    let cwd = repo.canonical_path().to_string_lossy().to_string();
    let session_id = "gemini-bash-test-session";
    let tool_use_id = "tool-call-001";

    let file_path = repo.path().join("script.sh");
    fs::write(&file_path, "#!/bin/sh\necho hello\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // BeforeTool via CLI
    let pre_hook_input = json!({
        "session_id": session_id,
        "tool_use_id": tool_use_id,
        "cwd": cwd,
        "hook_event_name": "BeforeTool",
        "tool_name": "shell",
        "tool_input": { "command": "echo modified > output.txt" },
        "transcript_path": fixture_path_str,
    })
    .to_string();
    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &pre_hook_input])
        .unwrap();

    // Simulate the bash tool writing a new file.
    let output_path = repo.path().join("output.txt");
    fs::write(&output_path, "modified\n").unwrap();

    // AfterTool via CLI
    let post_hook_input = json!({
        "session_id": session_id,
        "tool_use_id": tool_use_id,
        "cwd": cwd,
        "hook_event_name": "AfterTool",
        "tool_name": "shell",
        "tool_input": { "command": "echo modified > output.txt" },
        "transcript_path": fixture_path_str,
    })
    .to_string();
    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &post_hook_input])
        .unwrap();

    // Verify changes were detected by committing and checking attribution
    let commit = repo.stage_all_and_commit("Gemini bash edit").unwrap();
    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "AfterTool with shell should produce AI attestations"
    );
}

crate::reuse_tests_in_worktree!(
    test_parse_example_gemini_json_with_model,
    test_gemini_parses_user_messages,
    test_gemini_parses_assistant_messages,
    test_gemini_parses_tool_calls,
    test_gemini_parses_tool_call_args,
    test_gemini_handles_empty_content,
    test_gemini_skips_unknown_message_types,
    test_gemini_preset_extracts_edited_filepath,
    test_gemini_preset_no_filepath_when_tool_input_missing,
    test_gemini_preset_human_checkpoint,
    test_gemini_preset_ai_checkpoint,
    test_gemini_preset_extracts_model,
    test_gemini_preset_stores_transcript_path_in_metadata,
    test_gemini_preset_handles_missing_transcript_path,
    test_gemini_preset_handles_invalid_json,
    test_gemini_preset_handles_missing_session_id,
    test_gemini_preset_handles_missing_file,
    test_gemini_e2e_with_attribution,
    test_gemini_e2e_human_checkpoint,
    test_gemini_e2e_multiple_tool_calls,
    test_gemini_e2e_with_resync,
    test_gemini_e2e_partial_staging,
    test_gemini_preset_bash_tool_aftertool_detects_changes,
);
