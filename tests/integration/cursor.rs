use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, real_git_executable};
use crate::test_utils::fixture_path;

const TEST_CONVERSATION_ID: &str = "de751938-f32b-4441-8239-a31d60aa4cf0";

#[test]
fn test_cursor_jsonl_basic_parsing() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let fixture = fixture_path("cursor-session-simple.jsonl");
    let (transcript, model) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(fixture.to_str().unwrap())
            .expect("Should parse cursor JSONL");

    // Model should be None (comes from hook input, not JSONL)
    assert_eq!(model, None, "Model should be None for Cursor JSONL");

    // Real Cursor session: HBO shows generation
    // 1 user message, 3 assistant texts ([REDACTED]-only skipped), 10 tool_use
    let messages = transcript.messages();
    assert!(
        !messages.is_empty(),
        "Should have parsed messages from the fixture"
    );

    let user_count = messages
        .iter()
        .filter(|m| matches!(m, git_ai::authorship::transcript::Message::User { .. }))
        .count();
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, git_ai::authorship::transcript::Message::Assistant { .. }))
        .count();
    let tool_count = messages
        .iter()
        .filter(|m| matches!(m, git_ai::authorship::transcript::Message::ToolUse { .. }))
        .count();

    assert_eq!(user_count, 1, "Should have 1 user message");
    assert_eq!(assistant_count, 3, "Should have 3 assistant messages ([REDACTED]-only texts skipped)");
    assert_eq!(tool_count, 10, "Should have 10 tool_use messages (Read x3, WebSearch x4, WebFetch, Grep, Write)");
}

#[test]
fn test_cursor_jsonl_user_query_tag_stripping() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let fixture = fixture_path("cursor-session-simple.jsonl");
    let (transcript, _) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(fixture.to_str().unwrap())
            .expect("Should parse cursor JSONL");

    let messages = transcript.messages();
    let first_user = messages
        .iter()
        .find(|m| matches!(m, git_ai::authorship::transcript::Message::User { .. }))
        .expect("Should have at least one user message");

    if let git_ai::authorship::transcript::Message::User { text, .. } = first_user {
        assert!(
            !text.contains("<user_query>"),
            "User message should not contain <user_query> tag, got: {}",
            text
        );
        assert!(
            !text.contains("</user_query>"),
            "User message should not contain </user_query> tag"
        );
        assert_eq!(text, "Generate a file with all the HBO shows from the 90's in it");
    }
}

#[test]
fn test_cursor_jsonl_tool_normalization() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let fixture = fixture_path("cursor-session-simple.jsonl");
    let (transcript, _) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(fixture.to_str().unwrap())
            .expect("Should parse cursor JSONL");

    let messages = transcript.messages();
    let tool_messages: Vec<_> = messages
        .iter()
        .filter_map(|m| match m {
            git_ai::authorship::transcript::Message::ToolUse { name, input, .. } => {
                Some((name.as_str(), input))
            }
            _ => None,
        })
        .collect();

    // Write tool should have file_path, not path, and content should be stripped
    let write_tool = tool_messages
        .iter()
        .find(|(name, _)| *name == "Write")
        .expect("Should have a Write tool_use");
    assert!(
        write_tool.1.get("file_path").is_some(),
        "Write tool should have file_path (normalized from path)"
    );
    assert!(
        write_tool.1.get("content").is_none(),
        "Write tool should have content stripped (edit tool)"
    );
    assert!(
        write_tool.1.get("contents").is_none(),
        "Write tool should not have original 'contents' field"
    );

    // Read tool should have file_path normalized from path
    let read_tool = tool_messages
        .iter()
        .find(|(name, _)| *name == "Read")
        .expect("Should have a Read tool_use");
    assert!(
        read_tool.1.get("file_path").is_some(),
        "Read tool should have file_path (normalized from path)"
    );
    assert!(
        read_tool.1.get("path").is_none(),
        "Read tool should not have original 'path' field"
    );
}

#[test]
fn test_cursor_jsonl_read_tool_full_args() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let fixture = fixture_path("cursor-session-simple.jsonl");
    let (transcript, _) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(fixture.to_str().unwrap())
            .expect("Should parse cursor JSONL");

    let messages = transcript.messages();
    let read_tool = messages
        .iter()
        .find_map(|m| match m {
            git_ai::authorship::transcript::Message::ToolUse { name, input, .. }
                if name == "Read" =>
            {
                Some(input)
            }
            _ => None,
        })
        .expect("Should have a Read tool_use");

    // Read tool should preserve full args with normalized field name
    assert!(
        read_tool.get("file_path").is_some(),
        "Read tool should have file_path (normalized from path)"
    );
}

#[test]
fn test_cursor_jsonl_redacted_text_stripped() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let fixture = fixture_path("cursor-session-simple.jsonl");
    let (transcript, _) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(fixture.to_str().unwrap())
            .expect("Should parse cursor JSONL");

    let transcript_json =
        serde_json::to_string(&transcript.messages()).expect("Should serialize messages");
    assert!(
        !transcript_json.contains("[REDACTED]"),
        "[REDACTED] markers should be stripped from transcript"
    );

    // Most assistant texts are [REDACTED]-only and get skipped; real text is preserved without the marker
    let assistant_messages: Vec<_> = transcript
        .messages()
        .iter()
        .filter_map(|m| match m {
            git_ai::authorship::transcript::Message::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        assistant_messages
            .iter()
            .any(|t| t.contains("HBO")),
        "Should keep real content from messages that also had [REDACTED]"
    );
}

#[test]
fn test_cursor_jsonl_empty_file() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;
    use std::io::Write;
    use tempfile::NamedTempFile;

    let temp_file = NamedTempFile::new().expect("Should create temp file");
    // Write nothing — empty file
    let _ = temp_file.as_file().sync_all();

    let (transcript, model) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(temp_file.path().to_str().unwrap())
            .expect("Should handle empty file");

    assert!(transcript.messages().is_empty(), "Empty file should produce empty transcript");
    assert_eq!(model, None);
}

#[test]
fn test_cursor_jsonl_malformed_lines_skipped() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().expect("Should create temp file");
    writeln!(temp_file, r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"hello"}}]}}}}"#).unwrap();
    writeln!(temp_file, "this is not valid json").unwrap();
    writeln!(temp_file, r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"hi there"}}]}}}}"#).unwrap();
    temp_file.flush().unwrap();

    let (transcript, _) =
        CursorPreset::transcript_and_model_from_cursor_jsonl(temp_file.path().to_str().unwrap())
            .expect("Should handle malformed lines");

    assert_eq!(
        transcript.messages().len(),
        2,
        "Should have parsed 2 valid messages, skipping malformed line"
    );
}

#[test]
fn test_cursor_preset_multi_root_workspace_detection() {
    use git_ai::authorship::working_log::CheckpointKind;
    use git_ai::commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, CursorPreset,
    };

    // Helper function to test workspace selection
    let test_workspace_selection =
        |workspace_roots: &[&str], file_path: &str, expected_workspace: &str, description: &str| {
            let workspace_roots_json: Vec<String> = workspace_roots
                .iter()
                .map(|s| format!("\"{}\"", s))
                .collect();

            let tool_input_json = if file_path.is_empty() {
                String::new()
            } else {
                format!(
                    ",\n        \"tool_input\": {{ \"file_path\": \"{}\" }}",
                    file_path
                )
            };

            let hook_input = format!(
                r##"{{
        "conversation_id": "test-conversation-id",
        "workspace_roots": [{}],
        "hook_event_name": "preToolUse",
        "tool_name": "Write"{},
        "model": "model-name-from-hook-test"
    }}"##,
                workspace_roots_json.join(", "),
                tool_input_json
            );

            let flags = AgentCheckpointFlags {
                hook_input: Some(hook_input),
            };

            let preset = CursorPreset;
            let result = preset
                .run(flags)
                .unwrap_or_else(|_| panic!("Should succeed for: {}", description));

            assert_eq!(
                result.repo_working_dir,
                Some(expected_workspace.to_string()),
                "{}",
                description
            );

            assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
        };

    // Test 1: File in second workspace root
    test_workspace_selection(
        &[
            "/Users/test/workspace1",
            "/Users/test/workspace2",
            "/Users/test/workspace3",
        ],
        "/Users/test/workspace2/src/main.rs",
        "/Users/test/workspace2",
        "Should select workspace2 as it contains the file path",
    );

    // Test 2: File in third workspace root
    test_workspace_selection(
        &[
            "/Users/test/workspace1",
            "/Users/test/workspace2",
            "/Users/test/workspace3",
        ],
        "/Users/test/workspace3/lib/utils.rs",
        "/Users/test/workspace3",
        "Should select workspace3 as it contains the file path",
    );

    // Test 3: File path doesn't match any workspace (should fall back to first)
    test_workspace_selection(
        &["/Users/test/workspace1", "/Users/test/workspace2"],
        "/Users/other/project/src/main.rs",
        "/Users/test/workspace1",
        "Should fall back to first workspace when file path doesn't match any workspace",
    );

    // Test 4: No file path provided (should use first workspace)
    test_workspace_selection(
        &["/Users/test/workspace1", "/Users/test/workspace2"],
        "",
        "/Users/test/workspace1",
        "Should use first workspace when no file path is provided",
    );

    // Test 5: Workspace root with trailing slash
    test_workspace_selection(
        &["/Users/test/workspace1/", "/Users/test/workspace2/"],
        "/Users/test/workspace2/src/main.rs",
        "/Users/test/workspace2/",
        "Should handle workspace roots with trailing slashes",
    );

    // Test 6: File path without leading separator after workspace root
    test_workspace_selection(
        &["/Users/test/workspace1", "/Users/test/workspace2"],
        "/Users/test/workspace2/main.rs",
        "/Users/test/workspace2",
        "Should correctly match workspace even with immediate file after root",
    );

    // Test 7: Ambiguous prefix (workspace1 is prefix of workspace10)
    test_workspace_selection(
        &["/Users/test/workspace1", "/Users/test/workspace10"],
        "/Users/test/workspace10/src/main.rs",
        "/Users/test/workspace10",
        "Should correctly distinguish workspace10 from workspace1",
    );
}

#[test]
fn test_cursor_preset_human_checkpoint_no_filepath() {
    use git_ai::authorship::working_log::CheckpointKind;
    use git_ai::commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, CursorPreset,
    };

    let hook_input = r##"{
        "conversation_id": "test-conversation-id",
        "workspace_roots": ["/Users/test/workspace"],
        "hook_event_name": "preToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": "/Users/test/workspace/src/main.rs" },
        "model": "model-name-from-hook-test"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = CursorPreset;
    let result = preset
        .run(flags)
        .expect("Should succeed for human checkpoint");

    // Verify this is a human checkpoint
    assert!(
        result.checkpoint_kind == CheckpointKind::Human,
        "Should be a human checkpoint"
    );
    // Human checkpoints should not have edited_filepaths even if file_path is present
    assert!(result.edited_filepaths.is_none());
}

#[test]
fn test_cursor_checkpoint_stdin_with_utf8_bom() {
    let repo = TestRepo::new();
    let hook_input = format!(
        "\u{feff}{}",
        serde_json::json!({
            "conversation_id": "test-conversation-id",
            "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "model": "model-name-from-hook-test"
        })
    );

    let output = repo
        .git_ai_with_stdin(
            &["checkpoint", "cursor", "--hook-input", "stdin"],
            hook_input.as_bytes(),
        )
        .expect("checkpoint should parse stdin payload with UTF-8 BOM");

    assert!(
        !output.contains("Invalid JSON in hook_input"),
        "Should not fail JSON parsing when stdin has UTF-8 BOM. Output: {output}"
    );
}

#[test]
fn test_cursor_e2e_with_attribution() {
    use std::fs;

    let repo = TestRepo::new();
    let jsonl_fixture = fixture_path("cursor-session-simple.jsonl");
    let jsonl_path_str = jsonl_fixture.to_string_lossy().to_string();

    // Create parent directory for the test file
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create initial file with some base content
    let file_path = repo.path().join("src/main.rs");
    let base_content = "fn main() {\n    println!(\"Hello, World!\");\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate cursor making edits to the file
    let edited_content = "fn main() {\n    println!(\"Hello, World!\");\n    // This is from Cursor\n    println!(\"Additional line from Cursor\");\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint with transcript_path pointing to JSONL fixture
    let hook_input = serde_json::json!({
        "conversation_id": TEST_CONVERSATION_ID,
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy().to_string() },
        "model": "model-name-from-hook-test",
        "transcript_path": jsonl_path_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "cursor", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add cursor edits").unwrap();

    // Verify attribution using TestFile
    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() {".human(),
        "    println!(\"Hello, World!\");".human(),
        "    // This is from Cursor".ai(),
        "    println!(\"Additional line from Cursor\");".ai(),
        "}".human(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Should have at least one prompt record in metadata"
    );

    // Get the first prompt record
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have at least one prompt record");

    // Verify that the prompt record has messages (transcript from JSONL)
    assert!(
        !prompt_record.messages.is_empty(),
        "Prompt record should contain messages from the JSONL transcript"
    );

    // The JSONL fixture has 14 messages (1 user + 3 assistant + 10 tool_use; [REDACTED]-only texts skipped)
    assert_eq!(
        prompt_record.messages.len(),
        14,
        "Should have exactly 14 messages from the JSONL fixture"
    );

    // Verify the model was extracted from hook input
    assert_eq!(
        prompt_record.agent_id.model, "model-name-from-hook-test",
        "Model should be 'model-name-from-hook-test' from hook input"
    );
}

#[test]
fn test_cursor_e2e_with_resync() {
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    let repo = TestRepo::new();

    // Create a temp JSONL file that we can modify
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let temp_jsonl_path = temp_dir.path().join("cursor-session.jsonl");
    let fixture_content = fs::read_to_string(fixture_path("cursor-session-simple.jsonl"))
        .expect("Should read fixture");
    fs::write(&temp_jsonl_path, &fixture_content).expect("Should write temp JSONL");
    let temp_jsonl_str = temp_jsonl_path.to_string_lossy().to_string();

    // Create parent directory for the test file
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create initial file with some base content
    let file_path = repo.path().join("src/main.rs");
    let base_content = "fn main() {\n    println!(\"Hello, World!\");\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Simulate cursor making edits to the file
    let edited_content = "fn main() {\n    println!(\"Hello, World!\");\n    // This is from Cursor\n    println!(\"Additional line from Cursor\");\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    // Run checkpoint with the UNMODIFIED temp JSONL
    let hook_input = serde_json::json!({
        "conversation_id": TEST_CONVERSATION_ID,
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy().to_string() },
        "model": "model-name-from-hook-test",
        "transcript_path": temp_jsonl_str
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "cursor", "--hook-input", &hook_input])
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Now append a new message to the JSONL file (simulating Cursor adding more data)
    {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&temp_jsonl_path)
            .expect("Should open temp JSONL for appending");
        writeln!(
            file,
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"RESYNC_TEST_MESSAGE: This was added after the checkpoint"}}]}}}}"#
        )
        .expect("Should append to JSONL");
    }

    // Commit — post-commit hook will re-read the JSONL via transcript_path in metadata
    repo.git(&["add", "-A"]).expect("add --all should succeed");
    let commit = repo.commit("Add cursor edits").unwrap();

    // Verify attribution still works
    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() {".human(),
        "    println!(\"Hello, World!\");".human(),
        "    // This is from Cursor".ai(),
        "    println!(\"Additional line from Cursor\");".ai(),
        "}".human(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Should have at least one prompt record in metadata"
    );

    // Get the first prompt record
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have at least one prompt record");

    // Verify that the resync logic picked up the appended message
    let transcript_json =
        serde_json::to_string(&prompt_record.messages).expect("Should serialize messages");

    assert!(
        transcript_json.contains("RESYNC_TEST_MESSAGE"),
        "Resync logic should have picked up the appended message from the modified JSONL file"
    );
}

#[test]
fn test_cursor_checkpoint_routes_nested_worktree_file_to_worktree_repo() {
    use git_ai::git::repository::find_repository_in_path;
    use std::fs;
    use std::process::Command;

    let repo = TestRepo::new();
    let jsonl_fixture = fixture_path("cursor-session-simple.jsonl");
    let jsonl_path_str = jsonl_fixture.to_string_lossy().to_string();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Parent Repo"]);
    repo.stage_all_and_commit("initial commit").unwrap();

    let worktree_path = repo.path().join("hbd-worktree");
    let worktree_output = Command::new(real_git_executable())
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "worktree",
            "add",
            "-b",
            "hbd-cli",
            worktree_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to create nested linked worktree");
    assert!(
        worktree_output.status.success(),
        "failed to create nested linked worktree:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&worktree_output.stdout),
        String::from_utf8_lossy(&worktree_output.stderr)
    );

    let file_path = worktree_path.join("main.go");
    fs::write(
        &file_path,
        "package main\n\nfunc main() {\n\tprintln(\"hbd\")\n}\n",
    )
    .unwrap();

    let hook_input = serde_json::json!({
        "conversation_id": TEST_CONVERSATION_ID,
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy().to_string() },
        "model": "model-name-from-hook-test",
        "transcript_path": jsonl_path_str
    })
    .to_string();

    let output = repo
        .git_ai(&["checkpoint", "cursor", "--hook-input", &hook_input])
        .expect("cursor checkpoint should succeed");
    println!("Checkpoint output: {}", output);

    repo.sync_daemon_force();

    let parent_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("find parent repo");
    let parent_base = parent_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let parent_working_log = parent_repo
        .storage
        .working_log_for_base_commit(&parent_base)
        .expect("parent working log");

    assert!(
        parent_working_log
            .all_ai_touched_files()
            .unwrap_or_default()
            .is_empty(),
        "checkpoint must not stay on the parent repo when the edited file lives in a nested linked worktree"
    );

    let worktree_repo =
        find_repository_in_path(worktree_path.to_str().unwrap()).expect("find worktree repo");
    let worktree_base = worktree_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let worktree_working_log = worktree_repo
        .storage
        .working_log_for_base_commit(&worktree_base)
        .expect("worktree working log");

    let touched_files = worktree_working_log
        .all_ai_touched_files()
        .expect("read worktree touched files");
    assert!(
        touched_files.contains("main.go"),
        "cursor checkpoint should be recorded in the linked worktree working log when only the parent repo is listed in workspace_roots; found {:?}",
        touched_files
    );

    let checkpoints = worktree_working_log
        .read_all_checkpoints()
        .expect("read worktree checkpoints");
    assert!(
        !checkpoints.is_empty(),
        "worktree checkpoint log should not be empty for a nested linked worktree edit"
    );
}

crate::reuse_tests_in_worktree!(
    test_cursor_jsonl_basic_parsing,
    test_cursor_jsonl_user_query_tag_stripping,
    test_cursor_jsonl_tool_normalization,
    test_cursor_preset_multi_root_workspace_detection,
    test_cursor_preset_human_checkpoint_no_filepath,
    test_cursor_e2e_with_attribution,
    test_cursor_e2e_with_resync,
);
