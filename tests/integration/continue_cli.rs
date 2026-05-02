use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use crate::test_utils::fixture_path;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::ContinueAgent;
use git_ai::transcripts::watermark::RecordIndexWatermark;
use serde_json::json;
use std::fs;
use std::io::Write;

fn parse_continue(hook_input: &str) -> Result<Vec<ParsedHookEvent>, git_ai::error::GitAiError> {
    resolve_preset("continue-cli")?.parse(hook_input, "t_test")
}

#[test]
fn test_parse_example_continue_cli_json() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    assert!(!result.events.is_empty());

    println!("Parsed {} events:", result.events.len());
    for (i, e) in result.events.iter().enumerate() {
        if e.event_type == Some(Some("user_message".to_string())) {
            println!("{}: User: {:?}", i, e.prompt_text);
        } else if e.event_type == Some(Some("assistant_message".to_string())) {
            println!("{}: Assistant: {:?}", i, e.response_text);
        } else if e.event_type == Some(Some("tool_use".to_string())) {
            println!("{}: ToolUse: {:?}", i, e.tool_name);
        }
    }
}

#[test]
fn test_continue_cli_parses_user_messages() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    let user_messages: Vec<_> = result
        .events
        .iter()
        .filter(|e| e.event_type == Some(Some("user_message".to_string())))
        .collect();

    assert_eq!(
        user_messages.len(),
        1,
        "Should have exactly one user message"
    );

    if let Some(Some(ref text)) = user_messages[0].prompt_text {
        assert!(text.contains("Add another hello world line"));
    }
}

#[test]
fn test_continue_cli_parses_assistant_messages() {
    let fixture = fixture_path("continue-cli-session-simple.json");
    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    let assistant_messages: Vec<_> = result
        .events
        .iter()
        .filter(|e| e.event_type == Some(Some("assistant_message".to_string())))
        .collect();

    assert!(
        !assistant_messages.is_empty(),
        "Should have at least one assistant message"
    );

    if let Some(Some(ref text)) = assistant_messages[0].response_text {
        assert!(text.contains("I'll read the file first"));
    }
}

#[test]
fn test_continue_cli_parses_tool_calls_from_context_items() {
    // The ContinueAgent reader extracts tool calls from contextItems, not from
    // message.toolCalls. The simple fixture does not have contextItems with tool data,
    // so we create a temp fixture that includes them.
    let sample = r##"{
        "sessionId": "test-session",
        "title": "Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": { "role": "user", "content": "Read file" },
                "contextItems": []
            },
            {
                "message": { "role": "assistant", "content": "I'll read it" },
                "contextItems": [
                    {
                        "name": "Read",
                        "content": {"filepath": "/test/main.rs"}
                    }
                ]
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(temp_file.path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    let tool_uses: Vec<_> = result
        .events
        .iter()
        .filter(|e| e.event_type == Some(Some("tool_use".to_string())))
        .collect();

    assert!(!tool_uses.is_empty(), "Should have at least one tool call");

    if let Some(Some(ref name)) = tool_uses[0].tool_name {
        assert_eq!(name, "Read");
    }
}

#[test]
fn test_continue_cli_parses_tool_call_args_from_context_items() {
    let sample = r##"{
        "sessionId": "test-session",
        "title": "Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": { "role": "assistant", "content": "Reading" },
                "contextItems": [
                    {
                        "name": "Read",
                        "content": {"filepath": "/test/main.rs"}
                    }
                ]
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(temp_file.path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    let read_tool = result
        .events
        .iter()
        .find(|e| {
            e.event_type == Some(Some("tool_use".to_string()))
                && e.tool_name == Some(Some("Read".to_string()))
        })
        .expect("Should find a Read tool call");

    assert_eq!(
        read_tool.tool_name,
        Some(Some("Read".to_string())),
        "Tool call should have name 'Read'"
    );
}

#[test]
fn test_continue_cli_handles_empty_content() {
    let sample = r##"{
        "sessionId": "test-session",
        "title": "Test",
        "workspaceDirectory": "/test",
        "history": [
            {
                "message": {
                    "role": "user",
                    "content": "Hello"
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": ""
                },
                "contextItems": []
            },
            {
                "message": {
                    "role": "assistant",
                    "content": "Response text"
                },
                "contextItems": []
            }
        ]
    }"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let agent = ContinueAgent;
    let watermark = Box::new(RecordIndexWatermark::new(0));
    let result = agent
        .read_incremental(temp_file.path(), watermark, "test")
        .expect("Failed to parse Continue CLI JSON");

    let user_count = result
        .events
        .iter()
        .filter(|e| e.event_type == Some(Some("user_message".to_string())))
        .count();
    let assistant_count = result
        .events
        .iter()
        .filter(|e| e.event_type == Some(Some("assistant_message".to_string())))
        .count();

    assert_eq!(user_count, 1);
    assert_eq!(assistant_count, 1, "Should skip empty content");
}

#[test]
fn test_continue_cli_preset_extracts_model_from_hook_input() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "claude-3.5-sonnet");
            assert_eq!(e.context.agent_id.tool, "continue-cli");
            assert_eq!(e.context.session_id, "2dbfd673-096d-4773-b5f3-9023894a7355");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_continue_cli_preset_defaults_to_unknown_model() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_continue_cli_preset_extracts_edited_filepath() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts"))
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_continue_cli_preset_no_filepath_when_tool_input_missing() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(e.file_paths.is_empty());
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_continue_cli_preset_human_checkpoint() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PreToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("index.ts"))
            );
        }
        _ => panic!("Expected PreFileEdit for human checkpoint"),
    }
}

#[test]
fn test_continue_cli_preset_ai_checkpoint() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/index.ts"
        },
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(e.transcript_source.is_some());
            assert!(!e.file_paths.is_empty());
        }
        _ => panic!("Expected PostFileEdit for AI checkpoint"),
    }
}

#[test]
fn test_continue_cli_preset_stores_transcript_path_in_metadata() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let events = parse_continue(&hook_input).expect("Failed to run ContinueCliPreset");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(
                e.context.metadata.get("transcript_path"),
                Some(&"tests/fixtures/continue-cli-session-simple.json".to_string())
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_continue_cli_preset_handles_missing_transcript_path() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet"
    })
    .to_string();

    let result = parse_continue(&hook_input);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("transcript_path"));
}

#[test]
fn test_continue_cli_preset_handles_invalid_json() {
    let result = parse_continue("{ invalid json }");
    assert!(result.is_err());
}

#[test]
fn test_continue_cli_preset_handles_missing_session_id() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/continue-cli-session-simple.json"
    })
    .to_string();

    let result = parse_continue(&hook_input);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("session_id"));
}

#[test]
fn test_continue_cli_preset_handles_missing_file() {
    let hook_input = json!({
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "model": "claude-3.5-sonnet",
        "transcript_path": "tests/fixtures/nonexistent.json"
    })
    .to_string();

    // The new parse() API succeeds (transcript is lazy via TranscriptSource::Path)
    let events = parse_continue(&hook_input).expect("Parse should succeed with lazy transcript");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "claude-3.5-sonnet");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

// ============================================================================
// End-to-end tests using TestRepo
// ============================================================================

#[test]
fn test_continue_cli_e2e_with_attribution() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let file_path = repo.path().join("src/index.ts");
    let base_content = "console.log('Bonjour');\n\nconsole.log('hello world');\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let edited_content =
        "console.log('Bonjour');\n\nconsole.log('hello world');\nconsole.log('hello world');\n";
    fs::write(&file_path, edited_content).unwrap();

    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    let commit = repo.stage_all_and_commit("Add continue-cli edits").unwrap();

    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('Bonjour');".human(),
        "".human(),
        "console.log('hello world');".human(),
        "console.log('hello world');".ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have at least one attestation"
    );

    assert!(
        !commit.authorship_log.metadata.sessions.is_empty(),
        "Should have at least one session record in metadata"
    );

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have at least one session record");

    assert_eq!(
        session_record.agent_id.model, "claude-3.5-sonnet",
        "Model should be 'claude-3.5-sonnet'"
    );
}

#[test]
fn test_continue_cli_e2e_human_checkpoint() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let file_path = repo.path().join("src/index.ts");
    let base_content = "console.log('hello');\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PreToolUse",
        "model": "claude-3.5-sonnet",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    let human_content = "console.log('hello');\nconsole.log('human edit');\n";
    fs::write(&file_path, human_content).unwrap();

    let commit = repo.stage_all_and_commit("Human edit").unwrap();

    let mut file = repo.filename("src/index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('human edit');".human(),
    ]);

    assert_eq!(
        commit.authorship_log.attestations.len(),
        0,
        "Human checkpoint should not create AI attestations"
    );
}

#[test]
fn test_continue_cli_e2e_multiple_tool_calls() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    let file_path = repo.path().join("test.ts");
    let base_content = "const x = 1;\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let edited_content = "const x = 1;\nconst y = 2;\nconst z = 3;\n";
    fs::write(&file_path, edited_content).unwrap();

    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
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
fn test_continue_cli_e2e_preserves_model_on_commit() {
    let repo = TestRepo::new();
    let fixture_path_str = fixture_path("continue-cli-session-simple.json")
        .to_string_lossy()
        .to_string();

    let file_path = repo.path().join("test.ts");
    fs::write(&file_path, "const x = 1;\n").unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    fs::write(&file_path, "const x = 1;\nconst y = 2;\n").unwrap();

    let hook_input = json!({
        "session_id": "2dbfd673-096d-4773-b5f3-9023894a7355",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "model": "claude-opus-4",
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        },
        "transcript_path": fixture_path_str
    })
    .to_string();

    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &hook_input])
        .unwrap();

    let commit = repo.stage_all_and_commit("Add line").unwrap();

    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Should have a session record");

    assert_eq!(
        session_record.agent_id.model, "claude-opus-4",
        "Model should be preserved from hook_input"
    );
    assert_eq!(session_record.agent_id.tool, "continue-cli");
}

crate::reuse_tests_in_worktree!(
    test_parse_example_continue_cli_json,
    test_continue_cli_parses_user_messages,
    test_continue_cli_parses_assistant_messages,
    test_continue_cli_parses_tool_calls_from_context_items,
    test_continue_cli_parses_tool_call_args_from_context_items,
    test_continue_cli_handles_empty_content,
    test_continue_cli_preset_extracts_model_from_hook_input,
    test_continue_cli_preset_defaults_to_unknown_model,
    test_continue_cli_preset_extracts_edited_filepath,
    test_continue_cli_preset_no_filepath_when_tool_input_missing,
    test_continue_cli_preset_human_checkpoint,
    test_continue_cli_preset_ai_checkpoint,
    test_continue_cli_preset_stores_transcript_path_in_metadata,
    test_continue_cli_preset_handles_missing_transcript_path,
    test_continue_cli_preset_handles_invalid_json,
    test_continue_cli_preset_handles_missing_session_id,
    test_continue_cli_preset_handles_missing_file,
    test_continue_cli_e2e_with_attribution,
    test_continue_cli_e2e_human_checkpoint,
    test_continue_cli_e2e_multiple_tool_calls,
    test_continue_cli_e2e_preserves_model_on_commit,
);
