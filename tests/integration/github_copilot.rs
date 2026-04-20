use crate::test_utils::{fixture_path, load_fixture};
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::error::GitAiError;
use serde_json::json;
use std::path::Path;
use std::{fs, io::Write};

fn parse_copilot(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("github-copilot")?.parse(hook_input, "t_test")
}

/// Ensure CODESPACES and REMOTE_CONTAINERS are not set (they cause early return in transcript parsing)
fn ensure_clean_env() {
    unsafe {
        std::env::remove_var("CODESPACES");
        std::env::remove_var("REMOTE_CONTAINERS");
    }
}

#[test]
fn copilot_session_parsing_stub() {
    ensure_clean_env();
    let sample = r#"{"requests": []}"#;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(temp_path));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);
}

#[test]
fn copilot_session_parsing_simple() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(fixture_str));
    assert!(result.is_ok());
    let (tx, model, _edited_filepaths) = result.unwrap();

    let expected_messages = vec![
        Message::User {
            text: "What can you help me with?".to_string(),
            timestamp: Some("2025-10-07T13:51:13.835+00:00".to_string()),
        },
        Message::Assistant {
            text: "I'm GitHub Copilot, and I can help you with a wide range of programming and development tasks! Based on your current workspace, I can see you have a TypeScript project with an @/Users/svarlamov/projects/testing-git/index.ts file that prints \"Bonjour\" and numbers 1-10.\n\nHere are some ways I can assist you:\n\n## Code Development\n- **Write, edit, and refactor code** in various languages (TypeScript, JavaScript, Python, Java, C#, Go, Rust, and many more)\n- **Debug and fix issues** in your existing code\n- **Add new features** or modify existing functionality\n- **Optimize performance** and improve code quality\n\n## Project Management\n- **Set up new projects** and workspaces from scratch\n- **Configure build systems** and development environments\n- **Manage dependencies** and package installations\n- **Create and modify configuration files**\n\n## Git and Version Control\n- **Commit changes** and manage your Git workflow\n- **Create branches** and handle merges\n- **View git history** and resolve conflicts\n- **Work with pull requests** and issues\n\n## Development Tools\n- **Run terminal commands** and scripts\n- **Install and configure VS Code extensions**\n- **Set up debugging configurations**\n- **Work with databases** (PostgreSQL support available)\n\n## Documentation and Testing\n- **Write documentation** and README files\n- **Create and run tests**\n- **Generate code comments** and API documentation\n\nFor your current TypeScript project, I could help you:\n- Add error handling and input validation\n- Convert it to a proper CLI tool with argument parsing\n- Add TypeScript configurations and build scripts\n- Set up testing frameworks\n- Add more sophisticated functionality\n\nWhat specific task would you like help with today?".to_string(),
            timestamp: Some("2025-10-07T13:51:26.196+00:00".to_string()),
        },
        Message::User {
            text: "Change Bonjour World to hello world".to_string(),
            timestamp: Some("2025-10-07T13:51:41.282+00:00".to_string()),
        },
        Message::ToolUse {
            name: "prepareToolInvocation".to_string(),
            input: json!({ "kind": "prepareToolInvocation", "toolName": "copilot_replaceString" }),
            timestamp: None,
        },
        Message::ToolUse {
            name: "copilot_replaceString".to_string(),
            input: serde_json::Value::String("Using \"Replace String in File\"".to_string()),
            timestamp: None,
        },
        Message::ToolUse {
            name: "textEditGroup".to_string(),
            input: json!({
                "done": true,
                "edits": [[{ "range": { "endColumn": 30, "endLineNumber": 10, "startColumn": 1, "startLineNumber": 5 }, "text": "// Print numbers 1 through 10\nfor (let i = 1; i <= 10; i++) {\n\tconsole.log(i);\n}\n\nconsole.log('hello world');" }], []],
                "kind": "textEditGroup",
                "uri": { "$mid": 1, "external": "file:///Users/svarlamov/projects/testing-git/index.ts", "fsPath": "/Users/svarlamov/projects/testing-git/index.ts", "path": "/Users/svarlamov/projects/testing-git/index.ts", "scheme": "file" }
            }),
            timestamp: None,
        },
        Message::Assistant {
            text: "I'll change \"Bonjour World\" to \"hello world\" in your selected text. ``` @/Users/svarlamov/projects/testing-git/index.ts ``` Done! I've changed \"Bonjour World\" to \"hello world\" in line 10 of your @/Users/svarlamov/projects/testing-git/index.ts file.".to_string(),
            timestamp: Some("2025-10-07T13:51:50.791+00:00".to_string()),
        },
        Message::User {
            text: "Search for any other mentions of Bonjour".to_string(),
            timestamp: Some("2025-10-07T15:15:50.757+00:00".to_string()),
        },
        Message::ToolUse {
            name: "prepareToolInvocation".to_string(),
            input: json!({ "kind": "prepareToolInvocation", "toolName": "copilot_findTextInFiles" }),
            timestamp: None,
        },
        Message::ToolUse {
            name: "copilot_findTextInFiles".to_string(),
            input: serde_json::Value::String("Searching text for `Bonjour`".to_string()),
            timestamp: None,
        },
        Message::Assistant {
            text: "I'll search for any other mentions of \"Bonjour\" in your workspace. I found one more mention of \"Bonjour\" in your code! It's on line 1 of @/Users/svarlamov/projects/testing-git/index.ts :\n\n```typescript\nconsole.log('Bonjour');\n```\n\nWould you like me to change this one to \"hello\" as well to make it consistent?".to_string(),
            timestamp: Some("2025-10-07T15:15:59.544+00:00".to_string()),
        },
    ];

    assert_eq!(tx.messages, expected_messages);
    assert_eq!(model, Some("copilot/claude-sonnet-4".to_string()));
}

#[test]
fn test_copilot_extracts_edited_filepaths() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(fixture_str));
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/Users/svarlamov/projects/testing-git/index.ts");
}

#[test]
fn test_copilot_no_edited_filepaths_when_no_edits() {
    ensure_clean_env();
    let sample = r##"{"requests": [{"timestamp": 1728308673835, "message": {"text": "What can you help me with?"}, "response": [{"kind": "markdown", "value": "I can help with code!"}], "modelId": "copilot/claude-sonnet-4"}]}"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(temp_path));
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 0);
}

#[test]
fn test_copilot_deduplicates_edited_filepaths() {
    ensure_clean_env();
    let sample = r##"{"requests": [{"timestamp": 1728308673835, "message": {"text": "Edit the file"}, "response": [{"kind": "textEditGroup", "uri": {"fsPath": "/Users/test/file.ts"}}, {"kind": "textEditGroup", "uri": {"fsPath": "/Users/test/file.ts"}}, {"kind": "textEditGroup", "uri": {"fsPath": "/Users/test/other.ts"}}], "modelId": "copilot/claude-sonnet-4"}]}"##;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(temp_path));
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&"/Users/test/file.ts".to_string()));
    assert!(paths.contains(&"/Users/test/other.ts".to_string()));
}

#[test]
#[serial_test::serial]
fn test_copilot_returns_empty_transcript_in_codespaces() {
    let original_codespaces = std::env::var("CODESPACES").ok();
    unsafe {
        std::env::set_var("CODESPACES", "true");
    }

    let fixture = fixture_path("copilot_session_simple.json");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);

    unsafe {
        if let Some(original) = original_codespaces {
            std::env::set_var("CODESPACES", original);
        } else {
            std::env::remove_var("CODESPACES");
        }
    }
}

#[test]
#[serial_test::serial]
fn test_copilot_returns_empty_transcript_in_remote_containers() {
    let original = std::env::var("REMOTE_CONTAINERS").ok();
    unsafe {
        std::env::set_var("REMOTE_CONTAINERS", "true");
    }

    let fixture = fixture_path("copilot_session_simple.json");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);

    unsafe {
        if let Some(orig) = original {
            std::env::set_var("REMOTE_CONTAINERS", orig);
        } else {
            std::env::remove_var("REMOTE_CONTAINERS");
        }
    }
}

// ============================================================================
// Tests for before_edit / after_edit logic
// ============================================================================

#[test]
fn test_copilot_preset_before_edit_human_checkpoint_snake_case() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspace_folder": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"],
        "dirty_files": { "/Users/test/project/file.ts": "console.log('hello');" }
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(e.dirty_files.is_some());
            let dirty_files = e.dirty_files.as_ref().unwrap();
            assert_eq!(dirty_files.len(), 1);
            assert!(dirty_files.values().any(|v| v.contains("hello")));
            assert_eq!(e.context.agent_id.tool, "github-copilot");
        }
        _ => panic!("Expected PreFileEdit for before_edit"),
    }
}

#[test]
fn test_copilot_preset_before_edit_human_checkpoint_camel_case() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspaceFolder": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"],
        "dirtyFiles": { "/Users/test/project/file.ts": "console.log('hello');" }
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(e.dirty_files.is_some());
        }
        _ => panic!("Expected PreFileEdit for before_edit"),
    }
}

#[test]
fn test_copilot_preset_before_edit_requires_will_edit_filepaths() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspace_folder": "/Users/test/project",
        "dirty_files": {}
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("will_edit_filepaths is required")
    );
}

#[test]
fn test_copilot_preset_before_edit_requires_non_empty_filepaths() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspace_folder": "/Users/test/project",
        "will_edit_filepaths": [],
        "dirty_files": {}
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("will_edit_filepaths cannot be empty")
    );
}

#[test]
fn test_copilot_preset_after_edit_requires_session_id() {
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "workspace_folder": "/Users/test/project",
        "dirty_files": {}
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("chat_session_path or chatSessionPath not found")
    );
}

#[test]
fn test_copilot_preset_after_edit_requires_session_id_camel_case() {
    let hook_input = json!({
        "hook_event_name": "after_edit",
        "workspaceFolder": "/Users/test/project",
        "dirtyFiles": {}
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("chat_session_path or chatSessionPath not found")
    );
}

#[test]
fn test_copilot_preset_invalid_hook_event_name() {
    let hook_input = json!({
        "hook_event_name": "invalid_event",
        "workspace_folder": "/Users/test/project"
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid hook_event_name")
    );
}

#[test]
fn test_copilot_preset_before_edit_multiple_files_snake_case() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspace_folder": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file1.ts", "/Users/test/project/file2.ts", "/Users/test/project/file3.ts"],
        "dirty_files": { "/Users/test/project/file1.ts": "content1", "/Users/test/project/file2.ts": "content2" }
    }).to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 3);
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_copilot_preset_before_edit_multiple_files_camel_case() {
    let hook_input = json!({
        "hook_event_name": "before_edit",
        "workspaceFolder": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file1.ts", "/Users/test/project/file2.ts", "/Users/test/project/file3.ts"],
        "dirtyFiles": { "/Users/test/project/file1.ts": "content1", "/Users/test/project/file2.ts": "content2" }
    }).to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 3);
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_copilot_preset_after_edit_camel_case() {
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(r#"{"requests": []}"#.as_bytes())
        .unwrap();
    let temp_path = temp_file.path().to_str().unwrap().to_string();

    let hook_input = json!({
        "hook_event_name": "after_edit",
        "workspaceFolder": "/Users/test/project",
        "chatSessionPath": temp_path,
        "sessionId": "test-session-123",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "dirtyFiles": { "/Users/test/project/file.ts": "console.log('hello');" }
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.id, "test-session-123");
            assert_eq!(e.context.agent_id.tool, "github-copilot");
            assert!(!e.file_paths.is_empty());
            assert!(e.dirty_files.is_some());
        }
        _ => panic!("Expected PostFileEdit for after_edit"),
    }
}

#[test]
fn test_copilot_preset_after_edit_snake_case() {
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file
        .write_all(r#"{"requests": []}"#.as_bytes())
        .unwrap();
    let temp_path = temp_file.path().to_str().unwrap().to_string();

    let hook_input = json!({
        "hook_event_name": "after_edit",
        "workspace_folder": "/Users/test/project",
        "chat_session_path": temp_path,
        "session_id": "test-session-456",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "dirty_files": { "/Users/test/project/file.ts": "console.log('hello');" }
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.id, "test-session-456");
            assert_eq!(e.context.agent_id.tool, "github-copilot");
            assert!(!e.file_paths.is_empty());
            assert!(e.dirty_files.is_some());
        }
        _ => panic!("Expected PostFileEdit for after_edit"),
    }
}

// ============================================================================
// Tests for JSONL format support
// ============================================================================

#[test]
fn copilot_session_parsing_jsonl_stub() {
    ensure_clean_env();
    let sample = r#"{"kind":0,"v":{"requests": []}}"#;
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = transcript_readers::read_copilot_session_json(Path::new(temp_path));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);
}

#[test]
fn copilot_session_parsing_jsonl_simple() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_simple.jsonl");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, _) = result.unwrap();
    assert!(!tx.messages.is_empty());
    assert_eq!(model, Some("copilot/claude-sonnet-4".to_string()));
}

#[test]
fn test_copilot_extracts_edited_filepaths_jsonl() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_simple.jsonl");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (_, _, edited_filepaths) = result.unwrap();
    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/Users/svarlamov/projects/testing-git/index.ts");
}

#[test]
fn test_copilot_after_edit_with_jsonl_session() {
    ensure_clean_env();

    let mut temp_file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
    temp_file
        .write_all(r#"{"kind":0,"v":{"requests": []}}"#.as_bytes())
        .unwrap();
    let temp_path = temp_file.path().to_str().unwrap().to_string();

    let hook_input = json!({
        "hook_event_name": "after_edit",
        "workspace_folder": "/Users/test/project",
        "chat_session_path": temp_path,
        "session_id": "test-jsonl-session-789",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "dirty_files": { "/Users/test/project/file.ts": "console.log('hello');" }
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Should succeed");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.id, "test-jsonl-session-789");
            assert_eq!(e.context.agent_id.tool, "github-copilot");
            assert!(!e.file_paths.is_empty());
            assert!(e.dirty_files.is_some());
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn copilot_session_parsing_multiline_jsonl() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_multiline.jsonl");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();

    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::User { text, .. } if text.contains("follow up message")))
    );
    assert!(tx.messages.iter().any(
        |m| matches!(m, Message::Assistant { text, .. } if text.contains("7sadfh32u23gdaWF"))
    ));
    assert_eq!(model, Some("copilot/gpt-4o".to_string()));
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);
}

#[test]
fn copilot_session_jsonl_empty_snapshot_with_patch() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_empty_then_patched.jsonl");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();

    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::User { text, .. } if text.contains("meaning of life")))
    );
    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::Assistant { text, .. } if text.contains("42")))
    );
    assert_eq!(model, Some("copilot/gpt-4o".to_string()));
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);
}

#[test]
fn copilot_session_jsonl_model_from_input_state_no_requests() {
    ensure_clean_env();
    let sample = r#"{"kind":0,"v":{"requests":[],"inputState":{"selectedModel":{"identifier":"copilot/claude-sonnet-4"}}}}"#;
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let result = transcript_readers::read_copilot_session_json(temp_file.path());
    assert!(result.is_ok());
    let (tx, model, _) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert_eq!(model, Some("copilot/claude-sonnet-4".to_string()));
}

#[test]
fn copilot_session_jsonl_per_request_model_overrides_input_state() {
    ensure_clean_env();
    let sample = r#"{"kind":0,"v":{"requests":[{"requestId":"r1","timestamp":1000000,"message":{"text":"hi"},"response":[{"value":"hello"}],"modelId":"copilot/gpt-4o"}],"inputState":{"selectedModel":{"identifier":"copilot/claude-sonnet-4"}}}}"#;
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let result = transcript_readers::read_copilot_session_json(temp_file.path());
    assert!(result.is_ok());
    let (_, model, _) = result.unwrap();
    assert_eq!(model, Some("copilot/gpt-4o".to_string()));
}

#[test]
fn copilot_session_jsonl_scalar_patch_applied() {
    ensure_clean_env();
    let sample = concat!(
        r#"{"kind":0,"v":{"requests":[],"inputState":{"selectedModel":{"identifier":"copilot/old-model"}}}}"#,
        "\n",
        r#"{"kind":1,"k":["inputState","selectedModel","identifier"],"v":"copilot/new-model"}"#,
    );
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let result = transcript_readers::read_copilot_session_json(temp_file.path());
    assert!(result.is_ok());
    let (_, model, _) = result.unwrap();
    assert_eq!(model, Some("copilot/new-model".to_string()));
}

#[test]
fn copilot_session_plain_json_unaffected() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_simple.json");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(!tx.messages.is_empty());
    assert_eq!(model, Some("copilot/claude-sonnet-4".to_string()));
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 1);
}

// ============================================================================
// VS Code PreToolUse / PostToolUse tests
// ============================================================================

#[test]
fn test_copilot_preset_vscode_pretooluse_human_checkpoint() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/ws-id/GitHub.copilot-chat/transcripts/session.jsonl",
        "toolInput": { "file_path": "src/main.ts" },
        "sessionId": "copilot-session-pre"
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Expected human checkpoint");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("src/main.ts"))
            );
        }
        _ => panic!("Expected PreFileEdit for PreToolUse"),
    }
}

#[test]
fn test_copilot_preset_vscode_create_file_tool_is_supported() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "create_file",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/ws-id/GitHub.copilot-chat/transcripts/session.jsonl",
        "toolInput": { "filePath": "/Users/test/project/src/new-file.ts", "content": "export const x = 1;\n" },
        "sessionId": "copilot-session-create"
    }).to_string();

    let events = parse_copilot(&hook_input).expect("Expected human checkpoint");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("new-file.ts"))
            );
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_apply_patch_tool_is_supported() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "apply_patch",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/ws-id/GitHub.copilot-chat/transcripts/session.jsonl",
        "toolInput": "*** Begin Patch\n*** Update File: src/main.ts\n@@\n-old\n+new\n*** End Patch",
        "sessionId": "copilot-session-apply-patch"
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Expected human checkpoint");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("src/main.ts"))
            );
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_editfiles_files_array_is_supported() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "editFiles",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/ws-id/GitHub.copilot-chat/transcripts/session.jsonl",
        "toolInput": { "files": ["src/main.ts", "/Users/test/project/src/other.ts"] },
        "sessionId": "copilot-session-editfiles"
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Expected human checkpoint");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.file_paths.len(), 2);
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_posttooluse_ai_checkpoint() {
    let temp_dir = tempfile::tempdir().unwrap();
    let transcripts_dir = temp_dir
        .path()
        .join("workspaceStorage")
        .join("workspace-id")
        .join("GitHub.copilot-chat")
        .join("transcripts");
    fs::create_dir_all(&transcripts_dir).unwrap();
    let transcript_path = transcripts_dir.join("copilot-session-post.jsonl");
    fs::write(&transcript_path, r#"{"requests": []}"#).unwrap();
    let session_path = transcript_path.to_string_lossy().to_string();

    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": { "file_path": "/Users/test/project/src/main.ts" },
        "sessionId": "copilot-session-post",
        "transcript_path": session_path
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Expected AI checkpoint");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "github-copilot");
            assert_eq!(e.context.agent_id.id, "copilot-session-post");
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("src/main.ts"))
            );
        }
        _ => panic!("Expected PostFileEdit for PostToolUse"),
    }
}

#[test]
fn test_copilot_preset_vscode_apply_patch_posttooluse_ai_checkpoint() {
    let temp_dir = tempfile::tempdir().unwrap();
    let transcripts_dir = temp_dir
        .path()
        .join("workspaceStorage")
        .join("workspace-id")
        .join("GitHub.copilot-chat")
        .join("transcripts");
    fs::create_dir_all(&transcripts_dir).unwrap();
    let transcript_path = transcripts_dir.join("copilot-session-apply-patch-post.jsonl");
    fs::write(&transcript_path, r#"{"requests": []}"#).unwrap();
    let session_path = transcript_path.to_string_lossy().to_string();

    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "apply_patch",
        "toolInput": "*** Begin Patch\n*** Update File: src/main.ts\n@@\n-old\n+new\n*** End Patch",
        "sessionId": "copilot-session-apply-patch-post",
        "transcript_path": session_path
    })
    .to_string();

    let events = parse_copilot(&hook_input).expect("Expected AI checkpoint");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "github-copilot");
            assert_eq!(e.context.agent_id.id, "copilot-session-apply-patch-post");
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("src/main.ts"))
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_non_edit_tool_is_filtered() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_findTextInFiles",
        "toolInput": { "query": "hello" },
        "sessionId": "copilot-session-search"
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("unsupported tool_name")
    );
}

#[test]
fn test_copilot_preset_vscode_claude_transcript_path_is_rejected() {
    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": { "file_path": "/Users/test/project/src/main.ts" },
        "sessionId": "copilot-session-wrong",
        "transcript_path": "/Users/test/.claude/projects/session.jsonl"
    })
    .to_string();

    let result = parse_copilot(&hook_input);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Claude transcript path")
    );
}

#[test]
fn copilot_session_parsing_event_stream_jsonl() {
    ensure_clean_env();
    let fixture = fixture_path("copilot_session_event_stream.jsonl");
    let result =
        transcript_readers::read_copilot_session_json(Path::new(fixture.to_str().unwrap()));
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();

    assert!(model.is_none());
    assert!(!tx.messages.is_empty());
    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::User { .. }))
    );
    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::Assistant { .. }))
    );
    assert!(
        tx.messages
            .iter()
            .any(|m| matches!(m, Message::ToolUse { .. }))
    );

    assert!(edited_filepaths.is_some());
    assert_eq!(
        edited_filepaths.unwrap(),
        vec!["/Users/svarlamov/projects/testing-git-vscode-hooks/jokes.csv"]
    );
}

#[test]
fn copilot_session_event_stream_jsonl_model_hint_is_detected() {
    ensure_clean_env();
    let sample = r#"{"type":"session.start","data":{"sessionId":"event-session-2","modelId":"copilot/gpt-4o"},"id":"evt-1","timestamp":"2026-02-14T03:02:25.825Z","parentId":null}
{"type":"user.message","data":{"content":"hello"},"id":"evt-2","timestamp":"2026-02-14T03:02:26.000Z","parentId":"evt-1"}
{"type":"assistant.message","data":{"content":"hi"},"id":"evt-3","timestamp":"2026-02-14T03:02:27.000Z","parentId":"evt-2"}"#;

    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();

    let result = transcript_readers::read_copilot_session_json(temp_file.path());
    assert!(result.is_ok());
    let (_, model, _) = result.unwrap();
    assert_eq!(model, Some("copilot/gpt-4o".to_string()));
}

// ============================================================================
// VS Code model lookup tests
// ============================================================================

const VS_CODE_LOOKUP_SESSION_ID: &str = "fixture-session-id";

fn setup_vscode_model_lookup_workspace(chat_session_fixture: &str) -> (tempfile::TempDir, String) {
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace_storage = temp_dir
        .path()
        .join("workspaceStorage")
        .join("workspace-model");
    let transcripts_dir = workspace_storage
        .join("GitHub.copilot-chat")
        .join("transcripts");
    let chat_sessions_dir = workspace_storage.join("chatSessions");
    fs::create_dir_all(&transcripts_dir).unwrap();
    fs::create_dir_all(&chat_sessions_dir).unwrap();

    let transcript_path = transcripts_dir.join(format!("{}.jsonl", VS_CODE_LOOKUP_SESSION_ID));
    fs::write(
        &transcript_path,
        load_fixture("copilot_transcript_session_lookup.jsonl"),
    )
    .unwrap();

    let fixture_p = fixture_path(chat_session_fixture);
    let ext = fixture_p
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("jsonl");
    let chat_session_path = chat_sessions_dir.join(format!("session-lookup.{}", ext));
    fs::write(chat_session_path, load_fixture(chat_session_fixture)).unwrap();

    (temp_dir, transcript_path.to_string_lossy().to_string())
}

fn vscode_post_tool_use_hook_input(transcript_path: &str) -> String {
    json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": { "file_path": "/Users/test/project/src/main.ts" },
        "sessionId": VS_CODE_LOOKUP_SESSION_ID,
        "transcript_path": transcript_path
    })
    .to_string()
}

#[test]
fn test_copilot_preset_vscode_model_uses_auto_model_id_when_present() {
    ensure_clean_env();
    let (_temp_dir, transcript_path) =
        setup_vscode_model_lookup_workspace("copilot_chat_session_lookup_auto.jsonl");
    let events = parse_copilot(&vscode_post_tool_use_hook_input(&transcript_path))
        .expect("Expected AI checkpoint");
    match &events[0] {
        // Model is lazily resolved from transcript, so at parse time it's "unknown"
        ParsedHookEvent::PostFileEdit(e) => assert_eq!(e.context.agent_id.model, "unknown"),
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_model_prefers_non_auto_model_id_from_chat_sessions() {
    ensure_clean_env();
    let (_temp_dir, transcript_path) =
        setup_vscode_model_lookup_workspace("copilot_chat_session_lookup_non_auto.jsonl");
    let events = parse_copilot(&vscode_post_tool_use_hook_input(&transcript_path))
        .expect("Expected AI checkpoint");
    match &events[0] {
        // Model is lazily resolved from transcript, so at parse time it's "unknown"
        ParsedHookEvent::PostFileEdit(e) => assert_eq!(e.context.agent_id.model, "unknown"),
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_model_falls_back_to_selected_model_id() {
    ensure_clean_env();
    let (_temp_dir, transcript_path) =
        setup_vscode_model_lookup_workspace("copilot_chat_session_lookup_selected_model.jsonl");
    let events = parse_copilot(&vscode_post_tool_use_hook_input(&transcript_path))
        .expect("Expected AI checkpoint");
    match &events[0] {
        // Model is lazily resolved from transcript, so at parse time it's "unknown"
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown")
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_model_lookup_supports_json_chat_session_file() {
    ensure_clean_env();
    let (_temp_dir, transcript_path) =
        setup_vscode_model_lookup_workspace("copilot_chat_session_lookup_json_file.json");
    let events = parse_copilot(&vscode_post_tool_use_hook_input(&transcript_path))
        .expect("Expected AI checkpoint");
    match &events[0] {
        // Model is lazily resolved from transcript, so at parse time it's "unknown"
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown")
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_copilot_preset_vscode_does_not_use_details_as_model_fallback() {
    ensure_clean_env();
    let (_temp_dir, transcript_path) =
        setup_vscode_model_lookup_workspace("copilot_chat_session_lookup_details_only.jsonl");
    let events = parse_copilot(&vscode_post_tool_use_hook_input(&transcript_path))
        .expect("Expected AI checkpoint");
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => assert_eq!(e.context.agent_id.model, "unknown"),
        _ => panic!("Expected PostFileEdit"),
    }
}
