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
fn test_claude_code_raw_event_fidelity() {
    let fixture = fixture_path("example-claude-code.jsonl");
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    let expected: Vec<serde_json::Value> = std::fs::read_to_string(&fixture)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(result.events, expected);
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

    // Model is extracted from the real transcript fixture copied in the second checkpoint
    assert_eq!(
        session_record.agent_id.model, "claude-sonnet-4-20250514",
        "Session record model should come from the latest checkpoint's transcript"
    );
}

#[test]
fn test_claude_code_thinking_raw_event_fidelity() {
    let fixture = fixture_path("claude-code-with-thinking.jsonl");
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    let expected: Vec<serde_json::Value> = std::fs::read_to_string(&fixture)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(result.events, expected);
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
fn test_claude_code_plan_raw_event_fidelity() {
    let fixture = fixture_path("claude-code-with-plan.jsonl");
    let agent = ClaudeAgent;
    let watermark = Box::new(ByteOffsetWatermark::new(0));
    let result = agent
        .read_incremental(fixture.as_path(), watermark, "test")
        .expect("Failed to parse JSONL");

    let expected: Vec<serde_json::Value> = std::fs::read_to_string(&fixture)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(result.events, expected);
}

crate::reuse_tests_in_worktree!(
    test_claude_code_raw_event_fidelity,
    test_claude_code_thinking_raw_event_fidelity,
    test_claude_code_plan_raw_event_fidelity,
    test_claude_preset_extracts_edited_filepath,
    test_claude_preset_no_filepath_when_tool_input_missing,
    test_claude_preset_ignores_vscode_copilot_payload,
    test_claude_preset_ignores_cursor_payload,
    test_claude_preset_does_not_ignore_when_transcript_path_is_claude,
    test_claude_e2e_prefers_latest_checkpoint_for_prompts,
    test_is_plan_file_path_detects_plan_files,
    test_extract_plan_from_write_tool,
    test_extract_plan_from_edit_tool_with_prior_state,
    test_extract_plan_from_edit_tool_without_prior_state,
    test_extract_plan_returns_none_for_non_plan_files,
    test_extract_plan_returns_none_for_non_write_edit_tools,
    test_extract_plan_returns_none_for_empty_content,
);
