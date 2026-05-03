use crate::test_utils::fixture_path;
use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::commands::checkpoint_agent::transcript_readers;
use git_ai::commands::checkpoint_agent::transcript_readers::{
    extract_plan_from_tool_use, is_plan_file_path,
};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Write;

#[test]
fn test_parse_example_claude_code_jsonl_with_model() {
    let fixture = fixture_path("example-claude-code.jsonl");
    let (transcript, model) =
        transcript_readers::read_claude_jsonl(fixture.as_path()).expect("Failed to parse JSONL");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

    // Verify we extracted the model
    assert!(model.is_some());
    let model_name = model.unwrap();
    println!("Extracted model: {}", model_name);

    // Based on the example file, we should get claude-sonnet-4-20250514
    assert_eq!(model_name, "claude-sonnet-4-20250514");

    // Print the parsed transcript for inspection
    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => println!("{}: User: {}", i, text),
            Message::Assistant { text, .. } => println!("{}: Assistant: {}", i, text),
            Message::ToolUse { name, input, .. } => {
                println!("{}: ToolUse: {} with input: {:?}", i, name, input)
            }
            Message::Thinking { text, .. } => println!("{}: Thinking: {}", i, text),
            Message::Plan { text, .. } => println!("{}: Plan: {}", i, text),
        }
    }
}

#[test]
fn test_claude_preset_extracts_edited_filepath() {
    let hook_input = r##"{
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "permission_mode": "default",
        "session_id": "23aad27c-175d-427f-ac5f-a6830b8e6e65",
        "tool_input": {
            "file_path": "/Users/svarlamov/projects/testing-git/README.md",
            "new_string": "# Testing Git Repository",
            "old_string": "# Testing Git"
        },
        "tool_name": "Edit",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl"
    }"##;

    let events = resolve_preset("claude")
        .unwrap()
        .parse(hook_input, "t_test")
        .expect("Failed to run ClaudePreset");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(!e.file_paths.is_empty());
            assert!(
                e.file_paths
                    .iter()
                    .any(|p| p.to_string_lossy().contains("README.md"))
            );
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_claude_preset_no_filepath_when_tool_input_missing() {
    let hook_input = r##"{
        "cwd": "/Users/svarlamov/projects/testing-git",
        "hook_event_name": "PostToolUse",
        "session_id": "23aad27c-175d-427f-ac5f-a6830b8e6e65",
        "tool_name": "Read",
        "transcript_path": "tests/fixtures/example-claude-code.jsonl"
    }"##;

    let events = resolve_preset("claude")
        .unwrap()
        .parse(hook_input, "t_test")
        .expect("Failed to run ClaudePreset");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(e.file_paths.is_empty());
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_claude_preset_ignores_vscode_copilot_payload() {
    let hook_input = json!({
        "hookEventName": "PreToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "transcript_path": "/Users/test/Library/Application Support/Code/User/workspaceStorage/workspace-id/GitHub.copilot-chat/transcripts/copilot-session-1.jsonl",
        "toolInput": {
            "file_path": "/Users/test/project/src/main.ts"
        },
        "sessionId": "copilot-session-1",
        "model": "copilot/claude-sonnet-4"
    })
    .to_string();

    let result = resolve_preset("claude")
        .unwrap()
        .parse(&hook_input, "t_test");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Skipping VS Code hook payload in Claude preset")
    );
}

#[test]
fn test_claude_preset_ignores_cursor_payload() {
    let hook_input = json!({
        "conversation_id": "dff2bf79-6a53-446c-be41-f33512532fb0",
        "model": "default",
        "tool_name": "Write",
        "tool_input": {
            "file_path": "/Users/test/project/jokes.csv"
        },
        "transcript_path": "/Users/test/.cursor/projects/Users-test-project/agent-transcripts/dff2bf79-6a53-446c-be41-f33512532fb0/dff2bf79-6a53-446c-be41-f33512532fb0.jsonl",
        "hook_event_name": "postToolUse",
        "cursor_version": "2.5.26",
        "workspace_roots": ["/Users/test/project"]
    })
    .to_string();

    let result = resolve_preset("claude")
        .unwrap()
        .parse(&hook_input, "t_test");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Skipping Cursor hook payload in Claude preset")
    );
}

#[test]
fn test_claude_preset_does_not_ignore_when_transcript_path_is_claude() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join(".claude").join("projects");
    fs::create_dir_all(&claude_dir).unwrap();

    let transcript_path = claude_dir.join("session.jsonl");
    let fixture = fixture_path("example-claude-code.jsonl");
    let mut dst = std::fs::File::create(&transcript_path).unwrap();
    let src = std::fs::read(fixture).unwrap();
    dst.write_all(&src).unwrap();

    let hook_input = json!({
        "hookEventName": "PostToolUse",
        "cwd": "/Users/test/project",
        "toolName": "copilot_replaceString",
        "toolInput": {
            "file_path": "/Users/test/project/src/main.ts"
        },
        "sessionId": "copilot-session-2",
        "transcript_path": transcript_path.to_string_lossy().to_string()
    })
    .to_string();

    let events = resolve_preset("claude")
        .unwrap()
        .parse(&hook_input, "t_test")
        .expect("Expected native Claude preset handling");

    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "claude");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_claude_e2e_prefers_latest_checkpoint_for_prompts() {
    use crate::repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();

    // Enable prompt sharing for all repositories (empty blacklist = no exclusions)
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]); // No exclusions = share everywhere
    });

    let repo_root = repo.canonical_path();

    // Create initial file and commit
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Use a stable transcript path so both checkpoints share the same agent_id
    let transcript_path = repo_root.join("claude-session.jsonl");

    // First checkpoint: empty transcript (simulates race where data isn't ready yet)
    fs::write(&transcript_path, "").unwrap();
    let hook_input = json!({
        "cwd": repo_root.to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    // First AI edit and checkpoint with empty transcript/model
    fs::write(&file_path, "fn main() {}\n// ai line one\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Second AI edit with the real transcript content
    let fixture = fixture_path("example-claude-code.jsonl");
    fs::copy(&fixture, &transcript_path).unwrap();
    fs::write(&file_path, "fn main() {}\n// ai line one\n// ai line two\n").unwrap();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .unwrap();

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add AI lines").unwrap();

    // We should have exactly one session record keyed by the claude agent_id
    assert_eq!(
        commit.authorship_log.metadata.sessions.len(),
        1,
        "Expected a single session record"
    );
    let session_record = commit
        .authorship_log
        .metadata
        .sessions
        .values()
        .next()
        .expect("Session record should exist");

    // The latest checkpoint (with the real transcript) should win
    assert_eq!(
        session_record.agent_id.model, "claude-sonnet-4-20250514",
        "Session record should use the model from the latest checkpoint transcript"
    );
}

#[test]
fn test_parse_claude_code_jsonl_with_thinking() {
    let fixture = fixture_path("claude-code-with-thinking.jsonl");
    let (transcript, model) =
        transcript_readers::read_claude_jsonl(fixture.as_path()).expect("Failed to parse JSONL");

    // Verify we parsed some messages
    assert!(!transcript.messages().is_empty());

    // Verify we extracted the model
    assert!(model.is_some());
    let model_name = model.unwrap();
    println!("Extracted model: {}", model_name);
    assert_eq!(model_name, "claude-sonnet-4-5-20250929");

    // Print the parsed transcript for inspection
    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => {
                println!(
                    "{}: User: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::Assistant { text, .. } => {
                println!(
                    "{}: Assistant: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::ToolUse { name, input, .. } => {
                println!("{}: ToolUse: {} with input: {:?}", i, name, input)
            }
            Message::Thinking { text, .. } => {
                println!(
                    "{}: Thinking: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Message::Plan { text, .. } => {
                println!(
                    "{}: Plan: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
        }
    }

    assert_eq!(
        transcript.messages().len(),
        6,
        "Expected 6 messages (1 user + 2 thinking + 2 text + 1 tool_use, tool_result skipped)"
    );

    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be User"
    );

    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant (thinking)"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[1] {
        assert!(
            text.contains("add another"),
            "Thinking message should contain thinking content"
        );
    }

    assert!(
        matches!(transcript.messages()[2], Message::Assistant { .. }),
        "Third message should be Assistant (text)"
    );

    assert!(
        matches!(transcript.messages()[3], Message::ToolUse { .. }),
        "Fourth message should be ToolUse"
    );
    if let Message::ToolUse { name, .. } = &transcript.messages()[3] {
        assert_eq!(name, "Edit", "Tool should be Edit");
    }

    assert!(
        matches!(transcript.messages()[4], Message::Assistant { .. }),
        "Fifth message should be Assistant (thinking)"
    );

    assert!(
        matches!(transcript.messages()[5], Message::Assistant { .. }),
        "Sixth message should be Assistant (text)"
    );
}

#[test]
fn test_tool_results_are_not_parsed_as_user_messages() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_123","type":"tool_result","content":"File created successfully"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"Done!"}]},"timestamp":"2025-01-01T00:00:01Z"}"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _model) =
        transcript_readers::read_claude_jsonl(temp_path).expect("Failed to parse JSONL");

    assert_eq!(
        transcript.messages().len(),
        1,
        "Tool results should not be parsed as user messages"
    );

    assert!(
        matches!(transcript.messages()[0], Message::Assistant { .. }),
        "Only message should be Assistant"
    );
    if let Message::Assistant { text, .. } = &transcript.messages()[0] {
        assert_eq!(text, "Done!");
    }
}

#[test]
fn test_user_text_content_blocks_are_parsed_correctly() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello, can you help me?"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"Of course!"}]},"timestamp":"2025-01-01T00:00:01Z"}"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _model) =
        transcript_readers::read_claude_jsonl(temp_path).expect("Failed to parse JSONL");

    assert_eq!(
        transcript.messages().len(),
        2,
        "Should have user and assistant messages"
    );

    assert!(
        matches!(transcript.messages()[0], Message::User { .. }),
        "First message should be User"
    );
    if let Message::User { text, .. } = &transcript.messages()[0] {
        assert_eq!(text, "Hello, can you help me?");
    }

    assert!(
        matches!(transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant"
    );
}

// ===== Plan detection tests =====

#[test]
fn test_is_plan_file_path_detects_plan_files() {
    assert!(is_plan_file_path(
        "/Users/dev/.claude/plans/abstract-frolicking-neumann.md"
    ));
    assert!(is_plan_file_path(
        "/home/user/.claude/plans/glistening-doodling-manatee.md"
    ));
    #[cfg(windows)]
    assert!(is_plan_file_path(
        r"C:\Users\dev\.claude\plans\tender-watching-thompson.md"
    ));
    assert!(is_plan_file_path("/Users/dev/.claude/plans/PLAN.MD"));

    assert!(!is_plan_file_path("/Users/dev/myproject/src/main.rs"));
    assert!(!is_plan_file_path("/Users/dev/myproject/README.md"));
    assert!(!is_plan_file_path("/Users/dev/myproject/index.ts"));
    assert!(!is_plan_file_path(
        "/Users/dev/.claude/projects/settings.json"
    ));

    assert!(!is_plan_file_path(
        "/Users/dev/.claude/projects/-Users-dev-myproject/plan.md"
    ));
    assert!(!is_plan_file_path("/tmp/claude-plan.md"));
    assert!(!is_plan_file_path("/home/user/.claude/plan.md"));
    assert!(!is_plan_file_path("plan.md"));
    assert!(!is_plan_file_path("/some/path/my-plan.md"));

    assert!(!is_plan_file_path("/some/path/plan.txt"));
    assert!(!is_plan_file_path("/some/path/plan.json"));
    assert!(!is_plan_file_path("/Users/dev/.claude/plans/plan.txt"));
}

#[test]
fn test_extract_plan_from_write_tool() {
    let mut plan_states = HashMap::new();
    let input = serde_json::json!({
        "file_path": "/Users/dev/.claude/plans/abstract-frolicking-neumann.md",
        "content": "# My Plan\n\n## Step 1\nDo something"
    });

    let result = extract_plan_from_tool_use("Write", &input, &mut plan_states);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "# My Plan\n\n## Step 1\nDo something");

    assert_eq!(
        plan_states.get("/Users/dev/.claude/plans/abstract-frolicking-neumann.md"),
        Some(&"# My Plan\n\n## Step 1\nDo something".to_string())
    );
}

#[test]
fn test_extract_plan_from_edit_tool_with_prior_state() {
    let plan_path = "/Users/dev/.claude/plans/abstract-frolicking-neumann.md";
    let mut plan_states = HashMap::new();

    let write_input = serde_json::json!({
        "file_path": plan_path,
        "content": "# My Plan\n\n## Step 1\nDo something\n\n## Step 2\nDo another thing"
    });
    let write_result = extract_plan_from_tool_use("Write", &write_input, &mut plan_states);
    assert!(write_result.is_some());

    let edit_input = serde_json::json!({
        "file_path": plan_path,
        "old_string": "## Step 1\nDo something",
        "new_string": "## Step 1\nDo something specific"
    });
    let result = extract_plan_from_tool_use("Edit", &edit_input, &mut plan_states);
    assert!(result.is_some());
    let text = result.unwrap();

    assert_eq!(
        text,
        "# My Plan\n\n## Step 1\nDo something specific\n\n## Step 2\nDo another thing"
    );
}

#[test]
fn test_extract_plan_from_edit_tool_without_prior_state() {
    let mut plan_states = HashMap::new();

    let edit_input = serde_json::json!({
        "file_path": "/Users/dev/.claude/plans/bright-inventing-crescent.md",
        "old_string": "old text",
        "new_string": "new text"
    });
    let result = extract_plan_from_tool_use("Edit", &edit_input, &mut plan_states);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "new text");
}

#[test]
fn test_extract_plan_returns_none_for_non_plan_files() {
    let mut plan_states = HashMap::new();
    let input = serde_json::json!({
        "file_path": "/Users/dev/myproject/src/main.rs",
        "content": "fn main() {}"
    });

    let result = extract_plan_from_tool_use("Write", &input, &mut plan_states);
    assert!(result.is_none());
}

#[test]
fn test_extract_plan_returns_none_for_non_write_edit_tools() {
    let mut plan_states = HashMap::new();
    let input = serde_json::json!({
        "file_path": "/Users/dev/.claude/plans/bright-inventing-crescent.md",
        "content": "# Plan"
    });

    let result = extract_plan_from_tool_use("Read", &input, &mut plan_states);
    assert!(result.is_none());
}

#[test]
fn test_extract_plan_returns_none_for_empty_content() {
    let mut plan_states = HashMap::new();
    let input = serde_json::json!({
        "file_path": "/Users/dev/.claude/plans/bright-inventing-crescent.md",
        "content": "   "
    });

    let result = extract_plan_from_tool_use("Write", &input, &mut plan_states);
    assert!(result.is_none());
}

#[test]
fn test_parse_claude_code_jsonl_with_plan() {
    let fixture = fixture_path("claude-code-with-plan.jsonl");
    let (transcript, model) =
        transcript_readers::read_claude_jsonl(fixture.as_path()).expect("Failed to parse JSONL");

    assert_eq!(model.unwrap(), "claude-sonnet-4-20250514");

    println!("Parsed {} messages:", transcript.messages().len());
    for (i, message) in transcript.messages().iter().enumerate() {
        match message {
            Message::User { text, .. } => {
                println!("{}: User: {}", i, text.chars().take(80).collect::<String>())
            }
            Message::Assistant { text, .. } => {
                println!(
                    "{}: Assistant: {}",
                    i,
                    text.chars().take(80).collect::<String>()
                )
            }
            Message::ToolUse { name, .. } => {
                println!("{}: ToolUse: {}", i, name)
            }
            Message::Thinking { text, .. } => {
                println!(
                    "{}: Thinking: {}",
                    i,
                    text.chars().take(80).collect::<String>()
                )
            }
            Message::Plan { text, .. } => {
                println!("{}: Plan: {}", i, text.chars().take(80).collect::<String>())
            }
        }
    }

    assert_eq!(
        transcript.messages().len(),
        7,
        "Expected 7 messages (1 user + 3 assistant + 2 plan + 1 tool_use)"
    );

    assert!(
        matches!(&transcript.messages()[0], Message::User { text, .. } if text.contains("authentication")),
        "First message should be User asking about authentication"
    );

    assert!(
        matches!(&transcript.messages()[1], Message::Assistant { .. }),
        "Second message should be Assistant"
    );

    match &transcript.messages()[2] {
        Message::Plan { text, timestamp } => {
            assert!(text.contains("Authentication Implementation Plan"));
            assert!(text.contains("Phase 1: Database Schema"));
            assert!(text.contains("POST /auth/register"));
            assert!(timestamp.is_some(), "Plan should have a timestamp");
        }
        other => panic!("Expected Plan message, got {:?}", other),
    }

    assert!(
        matches!(&transcript.messages()[3], Message::Assistant { .. }),
        "Fourth message should be Assistant"
    );

    match &transcript.messages()[4] {
        Message::Plan { text, .. } => {
            assert!(text.contains("Authentication Implementation Plan"));
            assert!(text.contains("id (UUID)"));
            assert!(text.contains("Add index on email for fast lookups"));
            assert!(text.contains("POST /auth/register"));
            assert!(!text.contains("--- old plan"));
        }
        other => panic!("Expected Plan message from Edit, got {:?}", other),
    }

    match &transcript.messages()[5] {
        Message::ToolUse { name, input, .. } => {
            assert_eq!(name, "Edit", "Should be an Edit tool use");
            assert!(
                input["file_path"].as_str().unwrap().contains("src/main.rs"),
                "Should be editing main.rs, not a plan file"
            );
        }
        other => panic!("Expected ToolUse for code edit, got {:?}", other),
    }

    assert!(
        matches!(&transcript.messages()[6], Message::Assistant { .. }),
        "Last message should be Assistant"
    );
}

#[test]
fn test_plan_write_with_inline_jsonl() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","content":"# Plan\n\n1. First step\n2. Second step"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _) = transcript_readers::read_claude_jsonl(temp_path).unwrap();

    assert_eq!(transcript.messages().len(), 1);
    match &transcript.messages()[0] {
        Message::Plan { text, .. } => {
            assert_eq!(text, "# Plan\n\n1. First step\n2. Second step");
        }
        other => panic!("Expected Plan, got {:?}", other),
    }
}

#[test]
fn test_plan_edit_with_inline_jsonl() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","old_string":"1. First step","new_string":"1. First step (done)\n2. New step"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _) = transcript_readers::read_claude_jsonl(temp_path).unwrap();

    assert_eq!(transcript.messages().len(), 1);
    match &transcript.messages()[0] {
        Message::Plan { text, .. } => {
            assert_eq!(text, "1. First step (done)\n2. New step");
        }
        other => panic!("Expected Plan, got {:?}", other),
    }
}

#[test]
fn test_non_plan_edit_remains_tool_use() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/home/user/project/src/main.rs","old_string":"old code","new_string":"new code"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _) = transcript_readers::read_claude_jsonl(temp_path).unwrap();

    assert_eq!(transcript.messages().len(), 1);
    assert!(
        matches!(&transcript.messages()[0], Message::ToolUse { name, .. } if name == "Edit"),
        "Non-plan Edit should remain as ToolUse"
    );
}

#[test]
fn test_plan_message_serialization_roundtrip() {
    let plan_msg = Message::Plan {
        text: "# My Plan\n\n## Step 1\nDo something".to_string(),
        timestamp: Some("2025-01-01T00:00:00Z".to_string()),
    };

    let serialized = serde_json::to_string(&plan_msg).unwrap();
    let deserialized: Message = serde_json::from_str(&serialized).unwrap();

    assert_eq!(plan_msg, deserialized);
    assert!(serialized.contains(r#""type":"plan""#));
}

#[test]
fn test_mixed_plan_and_code_edits_in_single_assistant_message() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","content":"# Plan\nStep 1"}},{"type":"tool_use","id":"toolu_2","name":"Write","input":{"file_path":"/home/user/project/src/lib.rs","content":"pub fn hello() {}"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let (transcript, _) = transcript_readers::read_claude_jsonl(temp_path).unwrap();

    assert_eq!(transcript.messages().len(), 2);

    assert!(
        matches!(&transcript.messages()[0], Message::Plan { text, .. } if text.contains("Step 1")),
        "First tool_use should become Plan"
    );

    assert!(
        matches!(&transcript.messages()[1], Message::ToolUse { name, .. } if name == "Write"),
        "Second tool_use should remain ToolUse"
    );
}

crate::reuse_tests_in_worktree!(
    test_parse_example_claude_code_jsonl_with_model,
    test_claude_preset_extracts_edited_filepath,
    test_claude_preset_no_filepath_when_tool_input_missing,
    test_claude_preset_ignores_vscode_copilot_payload,
    test_claude_preset_ignores_cursor_payload,
    test_claude_preset_does_not_ignore_when_transcript_path_is_claude,
    test_claude_e2e_prefers_latest_checkpoint_for_prompts,
    test_parse_claude_code_jsonl_with_thinking,
    test_tool_results_are_not_parsed_as_user_messages,
    test_user_text_content_blocks_are_parsed_correctly,
    test_is_plan_file_path_detects_plan_files,
    test_extract_plan_from_write_tool,
    test_extract_plan_from_edit_tool_with_prior_state,
    test_extract_plan_from_edit_tool_without_prior_state,
    test_extract_plan_returns_none_for_non_plan_files,
    test_extract_plan_returns_none_for_non_write_edit_tools,
    test_extract_plan_returns_none_for_empty_content,
    test_parse_claude_code_jsonl_with_plan,
    test_plan_write_with_inline_jsonl,
    test_plan_edit_with_inline_jsonl,
    test_non_plan_edit_remains_tool_use,
    test_plan_message_serialization_roundtrip,
    test_mixed_plan_and_code_edits_in_single_assistant_message,
);
