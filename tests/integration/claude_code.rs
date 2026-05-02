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

    // Verify we parsed some events
    assert!(!result.events.is_empty());

    // Verify we extracted the model
    assert!(result.model.is_some());
    let model_name = result.model.unwrap();
    println!("Extracted model: {}", model_name);

    // Based on the example file, we should get claude-sonnet-4-20250514
    assert_eq!(model_name, "claude-sonnet-4-20250514");

    // Print the parsed events for inspection
    println!("Parsed {} events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        match event.event_type.as_ref().and_then(|v| v.as_deref()) {
            Some("user_message") => {
                let text = event
                    .prompt_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: User: {}", i, text);
            }
            Some("assistant_message") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: Assistant: {}", i, text);
            }
            Some("tool_use") => {
                let name = event
                    .tool_name
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: ToolUse: {}", i, name);
            }
            Some("assistant_thinking") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: Thinking: {}", i, text);
            }
            Some(other) => {
                println!("{}: {}", i, other);
            }
            None => {
                println!("{}: (no event type)", i);
            }
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

    // Verify we extracted the model
    assert!(result.model.is_some());
    let model_name = result.model.unwrap();
    println!("Extracted model: {}", model_name);
    assert_eq!(model_name, "claude-sonnet-4-5-20250929");

    // Print the parsed events for inspection
    println!("Parsed {} events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        match event.event_type.as_ref().and_then(|v| v.as_deref()) {
            Some("user_message") => {
                let text = event
                    .prompt_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!(
                    "{}: User: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Some("assistant_message") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!(
                    "{}: Assistant: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Some("tool_use") => {
                let name = event
                    .tool_name
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: ToolUse: {}", i, name)
            }
            Some("assistant_thinking") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!(
                    "{}: Thinking: {}",
                    i,
                    text.chars().take(100).collect::<String>()
                )
            }
            Some(other) => println!("{}: {}", i, other),
            None => println!("{}: (no event type)", i),
        }
    }

    assert_eq!(
        result.events.len(),
        6,
        "Expected 6 events (1 user_message + 2 assistant_thinking + 2 assistant_message + 1 tool_use)"
    );

    assert_eq!(
        result.events[0]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("user_message"),
        "First event should be user_message"
    );

    assert_eq!(
        result.events[1]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_thinking"),
        "Second event should be assistant_thinking"
    );
    {
        let text = result.events[1]
            .response_text
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert!(
            text.contains("add another"),
            "Thinking event should contain thinking content"
        );
    }

    assert_eq!(
        result.events[2]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Third event should be assistant_message"
    );

    assert_eq!(
        result.events[3]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("tool_use"),
        "Fourth event should be tool_use"
    );
    {
        let name = result.events[3]
            .tool_name
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert_eq!(name, "Edit", "Tool should be Edit");
    }

    assert_eq!(
        result.events[4]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_thinking"),
        "Fifth event should be assistant_thinking"
    );

    assert_eq!(
        result.events[5]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Sixth event should be assistant_message"
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

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .expect("Failed to parse JSONL");

    assert_eq!(
        result.events.len(),
        1,
        "Tool results should not be parsed as user messages"
    );

    assert_eq!(
        result.events[0]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Only event should be assistant_message"
    );
    {
        let text = result.events[0]
            .response_text
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
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
        "Should have user_message and assistant_message events"
    );

    assert_eq!(
        result.events[0]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("user_message"),
        "First event should be user_message"
    );
    {
        let text = result.events[0]
            .prompt_text
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert_eq!(text, "Hello, can you help me?");
    }

    assert_eq!(
        result.events[1]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Second event should be assistant_message"
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

    assert_eq!(result.model.unwrap(), "claude-sonnet-4-20250514");

    println!("Parsed {} events:", result.events.len());
    for (i, event) in result.events.iter().enumerate() {
        match event.event_type.as_ref().and_then(|v| v.as_deref()) {
            Some("user_message") => {
                let text = event
                    .prompt_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: User: {}", i, text.chars().take(80).collect::<String>())
            }
            Some("assistant_message") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!(
                    "{}: Assistant: {}",
                    i,
                    text.chars().take(80).collect::<String>()
                )
            }
            Some("tool_use") => {
                let name = event
                    .tool_name
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!("{}: ToolUse: {}", i, name)
            }
            Some("assistant_thinking") => {
                let text = event
                    .response_text
                    .as_ref()
                    .and_then(|v| v.as_deref())
                    .unwrap_or("");
                println!(
                    "{}: Thinking: {}",
                    i,
                    text.chars().take(80).collect::<String>()
                )
            }
            Some(other) => println!("{}: {}", i, other),
            None => println!("{}: (no event type)", i),
        }
    }

    // The new ClaudeAgent emits tool_use events for plan file writes/edits
    // instead of separate Plan events. Plan extraction is now handled
    // separately via is_plan_file_path and extract_plan_from_tool_use.
    assert_eq!(
        result.events.len(),
        7,
        "Expected 7 events (1 user_message + 3 assistant_message + 3 tool_use)"
    );

    // [0]: user_message asking about authentication
    {
        let event = &result.events[0];
        assert_eq!(
            event.event_type.as_ref().and_then(|v| v.as_deref()),
            Some("user_message"),
            "First event should be user_message"
        );
        let text = event
            .prompt_text
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert!(
            text.contains("authentication"),
            "User message should ask about authentication"
        );
    }

    // [1]: assistant_message
    assert_eq!(
        result.events[1]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Second event should be assistant_message"
    );

    // [2]: tool_use (Write to plan file) - was previously Plan
    {
        let event = &result.events[2];
        assert_eq!(
            event.event_type.as_ref().and_then(|v| v.as_deref()),
            Some("tool_use"),
            "Third event should be tool_use (plan Write)"
        );
        let name = event
            .tool_name
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert_eq!(name, "Write", "Should be a Write tool use for the plan");
    }

    // [3]: assistant_message
    assert_eq!(
        result.events[3]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Fourth event should be assistant_message"
    );

    // [4]: tool_use (Edit to plan file) - was previously Plan
    {
        let event = &result.events[4];
        assert_eq!(
            event.event_type.as_ref().and_then(|v| v.as_deref()),
            Some("tool_use"),
            "Fifth event should be tool_use (plan Edit)"
        );
        let name = event
            .tool_name
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert_eq!(name, "Edit", "Should be an Edit tool use for the plan");
    }

    // [5]: tool_use (Edit to main.rs - a code edit, not a plan)
    {
        let event = &result.events[5];
        assert_eq!(
            event.event_type.as_ref().and_then(|v| v.as_deref()),
            Some("tool_use"),
            "Sixth event should be tool_use (code Edit)"
        );
        let name = event
            .tool_name
            .as_ref()
            .and_then(|v| v.as_deref())
            .unwrap_or("");
        assert_eq!(name, "Edit", "Should be an Edit tool use for code");
    }

    // [6]: assistant_message
    assert_eq!(
        result.events[6]
            .event_type
            .as_ref()
            .and_then(|v| v.as_deref()),
        Some("assistant_message"),
        "Last event should be assistant_message"
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
    // The new ClaudeAgent emits tool_use events for plan file writes
    let event = &result.events[0];
    assert_eq!(
        event.event_type.as_ref().and_then(|v| v.as_deref()),
        Some("tool_use"),
        "Plan write should be emitted as tool_use"
    );
    assert_eq!(
        event.tool_name.as_ref().and_then(|v| v.as_deref()),
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
    // The new ClaudeAgent emits tool_use events for plan file edits
    let event = &result.events[0];
    assert_eq!(
        event.event_type.as_ref().and_then(|v| v.as_deref()),
        Some("tool_use"),
        "Plan edit should be emitted as tool_use"
    );
    assert_eq!(
        event.tool_name.as_ref().and_then(|v| v.as_deref()),
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
    assert_eq!(
        event.event_type.as_ref().and_then(|v| v.as_deref()),
        Some("tool_use"),
        "Non-plan Edit should be tool_use"
    );
    assert_eq!(
        event.tool_name.as_ref().and_then(|v| v.as_deref()),
        Some("Edit"),
    );
}

#[test]
fn test_mixed_plan_and_code_edits_in_single_assistant_message() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let jsonl_content = r##"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Write","input":{"file_path":"/home/user/.claude/plans/tender-watching-thompson.md","content":"# Plan\nStep 1"}},{"type":"tool_use","id":"toolu_2","name":"Write","input":{"file_path":"/home/user/project/src/lib.rs","content":"pub fn hello() {}"}}]},"timestamp":"2025-01-01T00:00:00Z"}"##;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(jsonl_content.as_bytes()).unwrap();
    let temp_path = temp_file.path();

    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(temp_path, watermark, "test")
        .unwrap();

    assert_eq!(result.events.len(), 2);

    // Both are tool_use events in the new API (plan detection is separate)
    let event0 = &result.events[0];
    assert_eq!(
        event0.event_type.as_ref().and_then(|v| v.as_deref()),
        Some("tool_use"),
        "First tool_use should be tool_use (plan Write)"
    );
    assert_eq!(
        event0.tool_name.as_ref().and_then(|v| v.as_deref()),
        Some("Write"),
    );

    let event1 = &result.events[1];
    assert_eq!(
        event1.event_type.as_ref().and_then(|v| v.as_deref()),
        Some("tool_use"),
        "Second tool_use should be tool_use (code Write)"
    );
    assert_eq!(
        event1.tool_name.as_ref().and_then(|v| v.as_deref()),
        Some("Write"),
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
    test_plan_write_emits_tool_use_event,
    test_plan_edit_emits_tool_use_event,
    test_non_plan_edit_remains_tool_use,
    test_mixed_plan_and_code_edits_in_single_assistant_message,
);
