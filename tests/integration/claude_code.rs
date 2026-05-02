use crate::test_utils::fixture_path;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::ClaudeAgent;
use git_ai::transcripts::agents::{extract_plan_from_tool_use, is_plan_file_path};
use git_ai::transcripts::watermark::ByteOffsetWatermark;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Write;

#[test]
fn test_parse_example_claude_code_jsonl_with_model() {
    let fixture = fixture_path("example-claude-code.jsonl");
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Verify we parsed some events (one per JSONL line)
    assert!(!result.events.is_empty());

    // Model is embedded in assistant message events, not on TranscriptBatch.
    // Find the first assistant event and check its model.
    let first_assistant = result
        .events
        .iter()
        .find(|e| e["type"].as_str() == Some("assistant"))
        .expect("Should have at least one assistant event");
    let model_name = first_assistant["message"]["model"]
        .as_str()
        .expect("Assistant event should have model");
    println!("Extracted model: {}", model_name);
    assert_eq!(model_name, "claude-sonnet-4-20250514");

    // Print the parsed events for inspection
    println!("Parsed {} events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        let event_type = event["type"].as_str().unwrap_or("unknown");
        let role = event["message"]["role"].as_str().unwrap_or("");
        println!("{}: type={}, role={}", i, event_type, role);
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

    // Model comes from preset AgentId (model resolution from transcript not wired up)
    assert_eq!(
        session_record.agent_id.model, "unknown",
        "Session record model comes from preset AgentId"
    );
}

#[test]
fn test_parse_claude_code_jsonl_with_thinking() {
    let fixture = fixture_path("claude-code-with-thinking.jsonl");
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Verify we parsed some events
    assert!(!result.events.is_empty());

    // Model is embedded in assistant events
    let first_assistant = result
        .events
        .iter()
        .find(|e| e["type"].as_str() == Some("assistant"))
        .expect("Should have at least one assistant event");
    let model_name = first_assistant["message"]["model"]
        .as_str()
        .expect("Assistant event should have model");
    println!("Extracted model: {}", model_name);
    assert_eq!(model_name, "claude-sonnet-4-5-20250929");

    // Print the parsed events for inspection
    println!("Parsed {} raw events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        let event_type = event["type"].as_str().unwrap_or("unknown");
        let role = event["message"]["role"].as_str().unwrap_or("");
        let content = event["message"]["content"].as_array();
        let content_types: Vec<&str> = content
            .map(|arr| arr.iter().filter_map(|c| c["type"].as_str()).collect())
            .unwrap_or_default();
        println!(
            "{}: type={}, role={}, content_types={:?}",
            i, event_type, role, content_types
        );
    }

    // The fixture has 10 lines:
    // 0: summary (no message)
    // 1: file-history-snapshot (no message)
    // 2: user (content is a string, not array)
    // 3: assistant with thinking content block
    // 4: assistant with text content block
    // 5: assistant with tool_use content block
    // 6: file-history-snapshot (no message)
    // 7: user with tool_result
    // 8: assistant with thinking content block
    // 9: assistant with text content block
    assert_eq!(
        result.events.len(),
        10,
        "Expected 10 raw events (one per JSONL line)"
    );

    // Line 2 (index 2) is user message
    assert_eq!(
        result.events[2]["type"].as_str(),
        Some("user"),
        "Third event should be user type"
    );

    // Line 3 (index 3) is assistant with thinking
    assert_eq!(
        result.events[3]["type"].as_str(),
        Some("assistant"),
        "Fourth event should be assistant type"
    );
    {
        let content = result.events[3]["message"]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(
            content[0]["type"].as_str(),
            Some("thinking"),
            "First content block should be thinking"
        );
        let thinking_text = content[0]["thinking"]
            .as_str()
            .expect("thinking block should have thinking text");
        assert!(
            thinking_text.contains("add another"),
            "Thinking event should contain thinking content"
        );
    }

    // Line 4 (index 4) is assistant with text
    assert_eq!(
        result.events[4]["type"].as_str(),
        Some("assistant"),
        "Fifth event should be assistant type"
    );
    assert_eq!(
        result.events[4]["message"]["content"][0]["type"].as_str(),
        Some("text"),
        "Content block should be text"
    );

    // Line 5 (index 5) is assistant with tool_use
    assert_eq!(
        result.events[5]["type"].as_str(),
        Some("assistant"),
        "Sixth event should be assistant type"
    );
    {
        let content = result.events[5]["message"]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(
            content[0]["type"].as_str(),
            Some("tool_use"),
            "Content block should be tool_use"
        );
        assert_eq!(
            content[0]["name"].as_str(),
            Some("Edit"),
            "Tool should be Edit"
        );
    }

    // Line 8 (index 8) is assistant with thinking (second thinking block)
    assert_eq!(
        result.events[8]["type"].as_str(),
        Some("assistant"),
        "Ninth event should be assistant type"
    );
    assert_eq!(
        result.events[8]["message"]["content"][0]["type"].as_str(),
        Some("thinking"),
        "Content block should be thinking"
    );

    // Line 9 (index 9) is assistant with text
    assert_eq!(
        result.events[9]["type"].as_str(),
        Some("assistant"),
        "Tenth event should be assistant type"
    );
    assert_eq!(
        result.events[9]["message"]["content"][0]["type"].as_str(),
        Some("text"),
        "Content block should be text"
    );
}

#[test]
fn test_tool_results_are_not_parsed_as_user_messages() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // With raw JSON events, tool_result lines ARE included as raw events.
    // The old behavior filtered them out at the AgentTraceValues level.
    // Now we just get raw JSONL lines.
    let jsonl_content = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_123","type":"tool_result","content":"File created successfully"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"Done!"}]},"timestamp":"2025-01-01T00:00:01Z"}"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .expect("Failed to parse JSONL");

    // Both lines are returned as raw events
    assert_eq!(
        result.events.len(),
        2,
        "Both JSONL lines should be returned as raw events"
    );

    assert_eq!(
        result.events[0]["type"].as_str(),
        Some("user"),
        "First event should be user type"
    );
    assert_eq!(
        result.events[1]["type"].as_str(),
        Some("assistant"),
        "Second event should be assistant type"
    );
    {
        let text = result.events[1]["message"]["content"][0]["text"]
            .as_str()
            .expect("Should have text");
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

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .expect("Failed to parse JSONL");

    assert_eq!(
        result.events.len(),
        2,
        "Should have 2 raw events (one per JSONL line)"
    );

    // First event is user type
    assert_eq!(
        result.events[0]["type"].as_str(),
        Some("user"),
        "First event should be user type"
    );
    {
        let text = result.events[0]["message"]["content"][0]["text"]
            .as_str()
            .expect("User message should have text");
        assert_eq!(text, "Hello, can you help me?");
    }

    // Second event is assistant type
    assert_eq!(
        result.events[1]["type"].as_str(),
        Some("assistant"),
        "Second event should be assistant type"
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
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    // Model is in assistant events
    let first_assistant = result
        .events
        .iter()
        .find(|e| e["type"].as_str() == Some("assistant"))
        .expect("Should have at least one assistant event");
    assert_eq!(
        first_assistant["message"]["model"].as_str().unwrap(),
        "claude-sonnet-4-20250514"
    );

    // The fixture has 10 lines (raw events):
    // 0: summary
    // 1: user (asking about authentication)
    // 2: assistant (text: "I'll create a plan...")
    // 3: assistant (tool_use: Write to plan file)
    // 4: user (tool_result)
    // 5: assistant (text: "Now let me update...")
    // 6: assistant (tool_use: Edit to plan file)
    // 7: user (tool_result)
    // 8: assistant (tool_use: Edit to main.rs - code edit)
    // 9: assistant (text: "I've created the plan...")
    println!("Parsed {} events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        let event_type = event["type"].as_str().unwrap_or("unknown");
        let role = event["message"]["role"].as_str().unwrap_or("");
        let content = event["message"]["content"].as_array();
        let content_types: Vec<&str> = content
            .map(|arr| arr.iter().filter_map(|c| c["type"].as_str()).collect())
            .unwrap_or_default();
        println!(
            "{}: type={}, role={}, content_types={:?}",
            i, event_type, role, content_types
        );
    }

    assert_eq!(
        result.events.len(),
        10,
        "Expected 10 raw events (one per JSONL line)"
    );

    // [1]: user message asking about authentication
    {
        let event = &result.events[1];
        assert_eq!(
            event["type"].as_str(),
            Some("user"),
            "Second event should be user type"
        );
        // User message content is a plain string in this fixture
        let content = event["message"]["content"]
            .as_str()
            .expect("User message should have string content");
        assert!(
            content.contains("authentication"),
            "User message should ask about authentication"
        );
    }

    // [2]: assistant message (text)
    assert_eq!(
        result.events[2]["type"].as_str(),
        Some("assistant"),
        "Third event should be assistant type"
    );

    // [3]: assistant with tool_use (Write to plan file)
    {
        let event = &result.events[3];
        assert_eq!(
            event["type"].as_str(),
            Some("assistant"),
            "Fourth event should be assistant type"
        );
        let content = event["message"]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"].as_str(), Some("tool_use"));
        assert_eq!(
            content[0]["name"].as_str(),
            Some("Write"),
            "Should be a Write tool use for the plan"
        );
    }

    // [5]: assistant message (text)
    assert_eq!(
        result.events[5]["type"].as_str(),
        Some("assistant"),
        "Sixth event should be assistant type"
    );

    // [6]: assistant with tool_use (Edit to plan file)
    {
        let event = &result.events[6];
        assert_eq!(
            event["type"].as_str(),
            Some("assistant"),
            "Seventh event should be assistant type"
        );
        let content = event["message"]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"].as_str(), Some("tool_use"));
        assert_eq!(
            content[0]["name"].as_str(),
            Some("Edit"),
            "Should be an Edit tool use for the plan"
        );
    }

    // [8]: assistant with tool_use (Edit to main.rs - code edit)
    {
        let event = &result.events[8];
        assert_eq!(
            event["type"].as_str(),
            Some("assistant"),
            "Ninth event should be assistant type"
        );
        let content = event["message"]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"].as_str(), Some("tool_use"));
        assert_eq!(
            content[0]["name"].as_str(),
            Some("Edit"),
            "Should be an Edit tool use for code"
        );
    }

    // [9]: assistant message (text)
    assert_eq!(
        result.events[9]["type"].as_str(),
        Some("assistant"),
        "Last event should be assistant type"
    );
}

#[test]
fn test_plan_write_emits_tool_use_event() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","content":"# Plan\n\n1. First step\n2. Second step"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .unwrap();

    assert_eq!(result.events.len(), 1);
    let event = &result.events[0];
    // Raw event is the assistant JSONL line
    assert_eq!(event["type"].as_str(), Some("assistant"));
    let content = event["message"]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(
        content[0]["type"].as_str(),
        Some("tool_use"),
        "Plan write should be a tool_use content block"
    );
    assert_eq!(
        content[0]["name"].as_str(),
        Some("Write"),
        "Tool name should be Write"
    );
}

#[test]
fn test_plan_edit_emits_tool_use_event() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","old_string":"1. First step","new_string":"1. First step (done)\n2. New step"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .unwrap();

    assert_eq!(result.events.len(), 1);
    let event = &result.events[0];
    assert_eq!(event["type"].as_str(), Some("assistant"));
    let content = event["message"]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(
        content[0]["type"].as_str(),
        Some("tool_use"),
        "Plan edit should be a tool_use content block"
    );
    assert_eq!(
        content[0]["name"].as_str(),
        Some("Edit"),
        "Tool name should be Edit"
    );
}

#[test]
fn test_non_plan_edit_remains_tool_use() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Edit","input":{"file_path":"/home/user/project/src/main.rs","old_string":"old code","new_string":"new code"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .unwrap();

    assert_eq!(result.events.len(), 1);
    let event = &result.events[0];
    assert_eq!(event["type"].as_str(), Some("assistant"));
    let content = event["message"]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(
        content[0]["type"].as_str(),
        Some("tool_use"),
        "Non-plan Edit should be tool_use"
    );
    assert_eq!(content[0]["name"].as_str(), Some("Edit"));
}

#[test]
fn test_mixed_plan_and_code_edits_in_single_assistant_message() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Single JSONL line with two tool_use content blocks
    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","content":"# Plan\nStep 1"}},{"type":"tool_use","id":"toolu_2","name":"Write","input":{"file_path":"/home/user/project/src/lib.rs","content":"pub fn hello() {}"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .unwrap();

    // Single JSONL line = single raw event
    assert_eq!(result.events.len(), 1);

    // The single event has two tool_use content blocks
    let content = result.events[0]["message"]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(content.len(), 2, "Should have 2 content blocks");

    assert_eq!(content[0]["type"].as_str(), Some("tool_use"));
    assert_eq!(content[0]["name"].as_str(), Some("Write"));

    assert_eq!(content[1]["type"].as_str(), Some("tool_use"));
    assert_eq!(content[1]["name"].as_str(), Some("Write"));
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
    test_plan_write_emits_tool_use_event,
    test_plan_edit_emits_tool_use_event,
    test_non_plan_edit_remains_tool_use,
    test_mixed_plan_and_code_edits_in_single_assistant_message,
);
