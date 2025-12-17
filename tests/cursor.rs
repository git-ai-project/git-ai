#[macro_use]
mod repos;
mod test_utils;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use rusqlite::{Connection, OpenFlags};
use serde_json;
use test_utils::fixture_path;

const TEST_CONVERSATION_ID: &str = "00812842-49fe-4699-afae-bb22cda3f6e1";

/// Helper function to open the test cursor database in read-only mode
fn open_test_db() -> Connection {
    let db_path = fixture_path("cursor_test.vscdb");
    Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("Failed to open test cursor database")
}

#[test]
fn test_can_open_cursor_test_database() {
    let conn = open_test_db();

    // Verify we can query the database
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM cursorDiskKV")
        .expect("Failed to prepare statement");

    let count: i64 = stmt
        .query_row([], |row| row.get(0))
        .expect("Failed to query");

    assert_eq!(count, 50, "Database should have exactly 50 records");
}

#[test]
fn test_cursor_database_has_composer_data() {
    let conn = open_test_db();

    // Check that we have the expected composer data
    let mut stmt = conn
        .prepare("SELECT key FROM cursorDiskKV WHERE key LIKE 'composerData:%'")
        .expect("Failed to prepare statement");

    let keys: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("Failed to query")
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to collect keys");

    assert!(!keys.is_empty(), "Should have at least one composer");
    assert!(
        keys.contains(&format!("composerData:{}", TEST_CONVERSATION_ID)),
        "Should contain the test conversation"
    );
}

#[test]
fn test_cursor_database_has_bubble_data() {
    let conn = open_test_db();

    // Check that we have bubble data for the test conversation
    let pattern = format!("bubbleId:{}:%", TEST_CONVERSATION_ID);
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM cursorDiskKV WHERE key LIKE ?")
        .expect("Failed to prepare statement");

    let count: i64 = stmt
        .query_row([&pattern], |row| row.get(0))
        .expect("Failed to query");

    assert_eq!(
        count, 42,
        "Should have exactly 42 bubbles for the test conversation"
    );
}

#[test]
fn test_fetch_composer_payload_from_test_db() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let db_path = fixture_path("cursor_test.vscdb");

    // Use the actual CursorPreset function
    let composer_payload = CursorPreset::fetch_composer_payload(&db_path, TEST_CONVERSATION_ID)
        .expect("Should fetch composer payload");

    // Verify the structure
    assert!(
        composer_payload
            .get("fullConversationHeadersOnly")
            .is_some(),
        "Should have fullConversationHeadersOnly field"
    );

    let headers = composer_payload
        .get("fullConversationHeadersOnly")
        .and_then(|v| v.as_array())
        .expect("fullConversationHeadersOnly should be an array");

    assert_eq!(
        headers.len(),
        42,
        "Should have exactly 42 conversation headers"
    );

    // Check that first header has bubbleId
    let first_header = &headers[0];
    assert!(
        first_header.get("bubbleId").is_some(),
        "Header should have bubbleId"
    );
}

#[test]
fn test_fetch_bubble_content_from_test_db() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let db_path = fixture_path("cursor_test.vscdb");

    // First, get a bubble ID from the composer data using actual function
    let composer_payload = CursorPreset::fetch_composer_payload(&db_path, TEST_CONVERSATION_ID)
        .expect("Should fetch composer payload");

    let headers = composer_payload
        .get("fullConversationHeadersOnly")
        .and_then(|v| v.as_array())
        .expect("Should have headers");

    let first_bubble_id = headers[0]
        .get("bubbleId")
        .and_then(|v| v.as_str())
        .expect("Should have bubble ID");

    // Use the actual CursorPreset function to fetch bubble content
    let bubble_data =
        CursorPreset::fetch_bubble_content_from_db(&db_path, TEST_CONVERSATION_ID, first_bubble_id)
            .expect("Should fetch bubble content")
            .expect("Bubble content should exist");

    // Verify bubble structure
    assert!(
        bubble_data.get("text").is_some() || bubble_data.get("content").is_some(),
        "Bubble should have text or content field"
    );
}

#[test]
fn test_extract_transcript_from_test_conversation() {
    use git_ai::commands::checkpoint_agent::agent_presets::CursorPreset;

    let db_path = fixture_path("cursor_test.vscdb");

    // Use the actual CursorPreset function to extract transcript data
    let composer_payload = CursorPreset::fetch_composer_payload(&db_path, TEST_CONVERSATION_ID)
        .expect("Should fetch composer payload");

    let transcript_data = CursorPreset::transcript_data_from_composer_payload(
        &composer_payload,
        &db_path,
        TEST_CONVERSATION_ID,
    )
    .expect("Should extract transcript data")
    .expect("Should have transcript data");

    let (transcript, model) = transcript_data;

    // Verify exact message count
    assert_eq!(
        transcript.messages().len(),
        31,
        "Should extract exactly 31 messages from the conversation"
    );

    // Verify model extraction
    assert_eq!(model, "gpt-5", "Model should be 'gpt-5'");
}

#[test]
#[ignore]

fn test_cursor_preset_extracts_edited_filepath() {
    use git_ai::commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, CursorPreset,
    };

    let hook_input = r##"{
        "conversation_id": "test-conversation-id",
        "workspace_roots": ["/Users/test/workspace"],
        "hook_event_name": "afterFileEdit",
        "file_path": "/Users/test/workspace/src/main.rs",
        "model": "model-name-from-hook-test"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = CursorPreset;
    let result = preset.run(flags);

    // This test will fail because the conversation doesn't exist in the test DB
    // But we can verify the error occurs after filepath extraction logic
    // In a real scenario with valid conversation, edited_filepaths would be populated
    assert!(result.is_err());
}

#[test]
#[ignore]
fn test_cursor_preset_no_filepath_when_missing() {
    use git_ai::commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, CursorPreset,
    };

    let hook_input = r##"{
        "conversation_id": "test-conversation-id",
        "workspace_roots": ["/Users/test/workspace"],
        "hook_event_name": "afterFileEdit",
        "model": "model-name-from-hook-test"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = CursorPreset;
    let result = preset.run(flags);

    // This test will fail because the conversation doesn't exist in the test DB
    // But we can verify the error occurs after filepath extraction logic
    assert!(result.is_err());
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
        "hook_event_name": "beforeSubmitPrompt",
        "file_path": "/Users/test/workspace/src/main.rs",
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
fn test_cursor_e2e_with_attribution() {
    use std::fs;

    let repo = TestRepo::new();
    let db_path = fixture_path("cursor_test.vscdb");
    let db_path_str = db_path.to_string_lossy().to_string();

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

    // Run checkpoint with the cursor database environment variable
    // Use serde_json to properly escape paths (especially important on Windows)
    // Note: Using a test model name to verify it comes from hook input, not DB (DB has "gpt-5")
    let hook_input = serde_json::json!({
        "conversation_id": TEST_CONVERSATION_ID,
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "model": "model-name-from-hook-test"
    })
    .to_string();

    let result = repo
        .git_ai_with_env(
            &["checkpoint", "cursor", "--hook-input", &hook_input],
            &[("GIT_AI_CURSOR_GLOBAL_DB_PATH", &db_path_str)],
        )
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add cursor edits").unwrap();

    // Verify attribution using TestFile
    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    println!(\"Hello, World!\");".human(),
        "    // This is from Cursor".ai(),
        "    println!(\"Additional line from Cursor\");".ai(),
        "}".human(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        commit.authorship_log.attestations.len() > 0,
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        commit.authorship_log.metadata.prompts.len() > 0,
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

    // Verify that the prompt record has messages (transcript)
    assert!(
        prompt_record.messages.len() > 0,
        "Prompt record should contain messages from the cursor database"
    );

    // Based on the test database, we expect 31 messages
    assert_eq!(
        prompt_record.messages.len(),
        31,
        "Should have exactly 31 messages from the test conversation"
    );

    // Verify the model was extracted from hook input (not from the database which has "gpt-5")
    assert_eq!(
        prompt_record.agent_id.model, "model-name-from-hook-test",
        "Model should be 'model-name-from-hook-test' from hook input (not 'gpt-5' from database)"
    );
}

#[test]
fn test_cursor_e2e_with_resync() {
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    let repo = TestRepo::new();
    let db_path = fixture_path("cursor_test.vscdb");
    let db_path_str = db_path.to_string_lossy().to_string();

    // Create a temp directory for the modified database
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let temp_db_path = temp_dir.path().join("modified_cursor_test.vscdb");

    // Copy the fixture database to the temp location
    fs::copy(&db_path, &temp_db_path).expect("Failed to copy database");

    // Modify one of the messages in the temp database
    {
        let conn = Connection::open(&temp_db_path).expect("Failed to open temp database");

        // Find and update one of the bubble messages with recognizable text
        // First, get a bubble key
        let bubble_key: String = conn
            .query_row(
                "SELECT key FROM cursorDiskKV WHERE key LIKE 'bubbleId:00812842-49fe-4699-afae-bb22cda3f6e1:%' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("Should find at least one bubble");

        // Get the current value and parse it as JSON
        let current_value: String = conn
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = ?",
                [&bubble_key],
                |row| row.get(0),
            )
            .expect("Should get bubble value");

        let mut bubble_json: serde_json::Value =
            serde_json::from_str(&current_value).expect("Should parse bubble JSON");

        // Modify the text field with our recognizable marker
        if let Some(obj) = bubble_json.as_object_mut() {
            obj.insert(
                "text".to_string(),
                serde_json::Value::String(
                    "RESYNC_TEST_MESSAGE: This message was updated after checkpoint".to_string(),
                ),
            );
        }

        // Update the database with the modified JSON
        let updated_value = serde_json::to_string(&bubble_json).expect("Should serialize JSON");
        conn.execute(
            "UPDATE cursorDiskKV SET value = ? WHERE key = ?",
            [&updated_value, &bubble_key],
        )
        .expect("Should update bubble");
    }

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

    // Run checkpoint with the ORIGINAL database (not yet modified)
    // Note: Using a test model name to verify it comes from hook input, not DB (DB has "gpt-5")
    let hook_input = serde_json::json!({
        "conversation_id": TEST_CONVERSATION_ID,
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "model": "model-name-from-hook-test"
    })
    .to_string();

    let result = repo
        .git_ai_with_env(
            &["checkpoint", "cursor", "--hook-input", &hook_input],
            &[("GIT_AI_CURSOR_GLOBAL_DB_PATH", &db_path_str)],
        )
        .unwrap();

    println!("Checkpoint output: {}", result);

    // Now commit with the MODIFIED database - this tests the resync logic in post_commit
    let temp_db_path_str = temp_db_path.to_string_lossy().to_string();
    repo.git(&["add", "-A"]).expect("add --all should succeed");
    let commit = repo
        .commit_with_env(
            "Add cursor edits",
            &[("GIT_AI_CURSOR_GLOBAL_DB_PATH", &temp_db_path_str)],
            None,
        )
        .unwrap();

    // Verify attribution still works
    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    println!(\"Hello, World!\");".human(),
        "    // This is from Cursor".ai(),
        "    println!(\"Additional line from Cursor\");".ai(),
        "}".human(),
    ]);

    // Verify the authorship log contains attestations and prompts
    assert!(
        commit.authorship_log.attestations.len() > 0,
        "Should have at least one attestation"
    );

    // Verify the metadata has prompts with transcript data
    assert!(
        commit.authorship_log.metadata.prompts.len() > 0,
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

    // Verify that the resync logic picked up the updated message
    let transcript_json =
        serde_json::to_string(&prompt_record.messages).expect("Should serialize messages");

    assert!(
        transcript_json.contains("RESYNC_TEST_MESSAGE"),
        "Resync logic should have picked up the updated message from the modified database"
    );

    // The temp directory and database will be automatically cleaned up when temp_dir goes out of scope
}

#[test]
fn test_cursor_preset_before_tab_file_read() {
    use git_ai::authorship::working_log::CheckpointKind;
    use git_ai::commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, CursorPreset,
    };

    let hook_input = r##"{
        "conversation_id": "test-tab-conversation-id",
        "workspace_roots": ["/Users/test/workspace"],
        "hook_event_name": "beforeTabFileRead",
        "file_path": "/Users/test/workspace/src/main.rs",
        "content": "fn main() {\n    println!(\"Hello\");\n}",
        "model": "tab"
    }"##;

    let flags = AgentCheckpointFlags {
        hook_input: Some(hook_input.to_string()),
    };

    let preset = CursorPreset;
    let result = preset
        .run(flags)
        .expect("Should succeed for beforeTabFileRead");

    // Verify this is a human checkpoint
    assert_eq!(
        result.checkpoint_kind,
        CheckpointKind::Human,
        "Should be a human checkpoint"
    );

    // Verify will_edit_filepaths is set with the single file
    assert!(result.will_edit_filepaths.is_some(), "Should have will_edit_filepaths");
    let will_edit = result.will_edit_filepaths.unwrap();
    assert_eq!(will_edit.len(), 1, "Should have exactly one file");
    assert_eq!(will_edit[0], "/Users/test/workspace/src/main.rs");

    // Verify dirty_files contains the file content
    assert!(result.dirty_files.is_some(), "Should have dirty_files");
    let dirty_files = result.dirty_files.unwrap();
    assert_eq!(dirty_files.len(), 1, "Should have exactly one dirty file");
    assert!(
        dirty_files.contains_key("/Users/test/workspace/src/main.rs"),
        "Should contain the file path"
    );
    assert_eq!(
        dirty_files.get("/Users/test/workspace/src/main.rs").unwrap(),
        "fn main() {\n    println!(\"Hello\");\n}"
    );

    // Verify agent_id
    assert_eq!(result.agent_id.tool, "cursor");
    assert_eq!(result.agent_id.id, "test-tab-conversation-id");
    assert_eq!(result.agent_id.model, "tab");
}

#[test]
fn test_cursor_tab_e2e_workflow() {
    use std::fs;

    let repo = TestRepo::new();

    // Create parent directory for the test file
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Create initial file with some base content
    let file_path = repo.path().join("src/main.rs");
    let base_content = "fn main() {\n    println!(\"Hello, World!\");\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Step 1: beforeTabFileRead - simulate Tab reading the file
    let before_read_hook = serde_json::json!({
        "conversation_id": "test-tab-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    println!("Before read checkpoint output: {}", result);

    // Step 2: afterTabFileEdit - simulate Tab completing the edit (unsaved)
    let after_edit_hook = serde_json::json!({
        "conversation_id": "test-tab-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "    // Added by Tab AI\n    println!(\"Tab was here!\");\n",
            "range": {
                "start_line_number": 3,
                "start_column": 1,
                "end_line_number": 3,
                "end_column": 1
            },
            "old_line": "",
            "new_line": "    // Added by Tab AI"
        }],
        "model": "tab"
    })
    .to_string();

    let result = repo
        .git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    println!("After edit checkpoint output: {}", result);

    // User saves the file before committing
    let edited_content = "fn main() {\n    println!(\"Hello, World!\");\n    // Added by Tab AI\n    println!(\"Tab was here!\");\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    // Commit the changes
    let commit = repo.stage_all_and_commit("Add Tab AI edits").unwrap();

    // Verify attribution using TestFile
    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(lines![
        "fn main() {".human(),
        "    println!(\"Hello, World!\");".human(),
        "    // Added by Tab AI".ai(),
        "    println!(\"Tab was here!\");".ai(),
        "}".human(),
    ]);

    // Verify the authorship log contains attestations
    assert!(
        commit.authorship_log.attestations.len() > 0,
        "Should have at least one attestation"
    );

    // Verify the agent metadata
    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Should have at least one prompt record");

    // Verify the model is "tab"
    assert_eq!(
        prompt_record.agent_id.model, "tab",
        "Model should be 'tab' from Tab AI"
    );

    // Verify the tool is "cursor"
    assert_eq!(
        prompt_record.agent_id.tool, "cursor",
        "Tool should be 'cursor'"
    );
}

#[test]
fn test_cursor_tab_multiple_edits_in_one_session() {
    use std::fs;

    let repo = TestRepo::new();

    // Create initial file with base content
    let file_path = repo.path().join("index.ts");
    let base_content = "function hello() {\n    console.log('hello world');\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Step 1: beforeTabFileRead
    let before_read_hook = serde_json::json!({
        "conversation_id": "test-multi-edit-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // Step 2: afterTabFileEdit with multiple edits (unsaved)
    let after_edit_hook = serde_json::json!({
        "conversation_id": "test-multi-edit-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [
            {
                "old_string": "",
                "new_string": "for (let i = 0; i < 10; i++) {\n        ",
                "range": {
                    "start_line_number": 2,
                    "start_column": 5,
                    "end_line_number": 2,
                    "end_column": 5
                },
                "old_line": "    console.log('hello world');",
                "new_line": "    for (let i = 0; i < 10; i++) {"
            },
            {
                "old_string": "",
                "new_string": "\n    }",
                "range": {
                    "start_line_number": 2,
                    "start_column": 36,
                    "end_line_number": 2,
                    "end_column": 36
                },
                "old_line": "    console.log('hello world');",
                "new_line": "        console.log('hello world');"
            }
        ],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    // User saves the file before committing
    let edited_content = "function hello() {\n    for (let i = 0; i < 10; i++) {\n        console.log('hello world');\n    }\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    // Commit the changes
    repo.stage_all_and_commit("Tab wraps code in for loop").unwrap();

    // Verify attribution - the for loop lines should be attributed to AI
    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(lines![
        "function hello() {".human(),
        "    for (let i = 0; i < 10; i++) {".ai(),
        "        console.log('hello world');".human(),
        "    }".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_cursor_tab_serial_edits_before_save() {
    use std::fs;

    let repo = TestRepo::new();

    let file_path = repo.path().join("stacked.ts");
    let base_content = "function calc(n: number) {\n    return n * 2;\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Initial read
    let before_read_hook = serde_json::json!({
        "conversation_id": "stacked-tab-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // First tab edit (unsaved)
    let after_edit_hook_one = serde_json::json!({
        "conversation_id": "stacked-tab-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "if (n < 0) {\n        return 0;\n    }\n    ",
            "range": {
                "start_line_number": 2,
                "start_column": 5,
                "end_line_number": 2,
                "end_column": 5
            },
            "old_line": "    return n * 2;",
            "new_line": "    if (n < 0) {"
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook_one])
        .unwrap();

    // Second tab edit (still unsaved)
    let after_edit_hook_two = serde_json::json!({
        "conversation_id": "stacked-tab-session",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "    const result = n * 2;\n    console.log(result);\n    return result;",
            "range": {
                "start_line_number": 5,
                "start_column": 1,
                "end_line_number": 5,
                "end_column": 100
            },
            "old_line": "    return n * 2;",
            "new_line": "    const result = n * 2;"
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook_two])
        .unwrap();

    // User finally saves the file before committing
    let final_content = "function calc(n: number) {\n    if (n < 0) {\n        return 0;\n    }\n    const result = n * 2;\n    console.log(result);\n    return result;\n}\n";
    fs::write(&file_path, final_content).unwrap();

    repo.stage_all_and_commit("Stacked tab edits before save")
        .unwrap();

    let mut file = repo.filename("stacked.ts");
    file.assert_lines_and_blame(lines![
        "function calc(n: number) {".human(),
        "    if (n < 0) {".ai(),
        "        return 0;".ai(),
        "    }".ai(),
        "    const result = n * 2;".ai(),
        "    console.log(result);".ai(),
        "    return result;".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_cursor_tab_edit_at_beginning_of_file() {
    use std::fs;

    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("config.ts");
    let base_content = "export const API_URL = 'https://api.example.com';\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // beforeTabFileRead
    let before_read_hook = serde_json::json!({
        "conversation_id": "test-beginning-edit",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // afterTabFileEdit (unsaved)
    let after_edit_hook = serde_json::json!({
        "conversation_id": "test-beginning-edit",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "// API Configuration\n",
            "range": {
                "start_line_number": 1,
                "start_column": 1,
                "end_line_number": 1,
                "end_column": 1
            },
            "old_line": "",
            "new_line": "// API Configuration"
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    // User saves the file before committing
    let edited_content = "// API Configuration\nexport const API_URL = 'https://api.example.com';\n";
    fs::write(&file_path, edited_content).unwrap();

    repo.stage_all_and_commit("Tab adds comment at beginning")
        .unwrap();

    // Verify blame
    let mut file = repo.filename("config.ts");
    file.assert_lines_and_blame(lines![
        "// API Configuration".ai(),
        "export const API_URL = 'https://api.example.com';".human(),
    ]);
}

#[test]
fn test_cursor_tab_edit_at_end_of_file() {
    use std::fs;

    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("utils.ts");
    let base_content = "export function add(a: number, b: number) {\n    return a + b;\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // beforeTabFileRead
    let before_read_hook = serde_json::json!({
        "conversation_id": "test-end-edit",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // afterTabFileEdit (unsaved)
    let after_edit_hook = serde_json::json!({
        "conversation_id": "test-end-edit",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "\nexport function subtract(a: number, b: number) {\n    return a - b;\n}\n",
            "range": {
                "start_line_number": 4,
                "start_column": 1,
                "end_line_number": 4,
                "end_column": 1
            },
            "old_line": "",
            "new_line": ""
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    // User saves the file before committing
    let edited_content = "export function add(a: number, b: number) {\n    return a + b;\n}\n\nexport function subtract(a: number, b: number) {\n    return a - b;\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    repo.stage_all_and_commit("Tab adds subtract function")
        .unwrap();

    // Verify blame
    let mut file = repo.filename("utils.ts");
    file.assert_lines_and_blame(lines![
        "export function add(a: number, b: number) {".human(),
        "    return a + b;".human(),
        "}".human(),
        "".ai(),
        "export function subtract(a: number, b: number) {".ai(),
        "    return a - b;".ai(),
        "}".ai(),
    ]);
}

#[test]
fn test_cursor_tab_inline_completion() {
    use std::fs;

    let repo = TestRepo::new();

    // Create initial file with incomplete line
    let file_path = repo.path().join("greeting.ts");
    let base_content = "function greet(name: string) {\n    console.log(\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // beforeTabFileRead
    let before_read_hook = serde_json::json!({
        "conversation_id": "test-inline-completion",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // afterTabFileEdit - inline completion on same line
    let after_edit_hook = serde_json::json!({
        "conversation_id": "test-inline-completion",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "`Hello, ${name}!`);",
            "range": {
                "start_line_number": 2,
                "start_column": 17,
                "end_line_number": 2,
                "end_column": 17
            },
            "old_line": "    console.log(",
            "new_line": "    console.log(`Hello, ${name}!`);"
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    // User saves the file before committing
    let edited_content = "function greet(name: string) {\n    console.log(`Hello, ${name}!`);\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    repo.stage_all_and_commit("Tab completes console.log")
        .unwrap();

    // Verify blame - inline completion modifies an existing line
    let mut file = repo.filename("greeting.ts");
    file.assert_lines_and_blame(lines![
        "function greet(name: string) {".human(),
        "    console.log(`Hello, ${name}!`);".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_cursor_tab_import_and_body_rewrite() {
    use std::fs;

    let repo = TestRepo::new();

    let file_path = repo.path().join("greet.ts");
    let base_content = "export function greet(name: string) {\n    return `Hi ${name}`;\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // beforeTabFileRead
    let before_read_hook = serde_json::json!({
        "conversation_id": "import-body-rewrite",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_hook])
        .unwrap();

    // afterTabFileEdit (unsaved) with two edits
    let after_edit_hook = serde_json::json!({
        "conversation_id": "import-body-rewrite",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [
            {
                "old_string": "",
                "new_string": "import { logger } from './logger';\n\n",
                "range": {
                    "start_line_number": 1,
                    "start_column": 1,
                    "end_line_number": 1,
                    "end_column": 1
                },
                "old_line": "",
                "new_line": "import { logger } from './logger';"
            },
            {
                "old_string": "",
                "new_string": "return `Hello, ${name}!`;\n    logger.log('greet called');\n    ",
                "range": {
                    "start_line_number": 2,
                    "start_column": 5,
                    "end_line_number": 2,
                    "end_column": 100
                },
                "old_line": "    return `Hi ${name}`;",
                "new_line": "    return `Hello, ${name}!`;"
            }
        ],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_hook])
        .unwrap();

    // User saves the file before committing
    let edited_content = "import { logger } from './logger';\n\nexport function greet(name: string) {\n    return `Hello, ${name}!`;\n    logger.log('greet called');\n}\n";
    fs::write(&file_path, edited_content).unwrap();

    repo.stage_all_and_commit("Tab adds import and rewrites body")
        .unwrap();

    let mut file = repo.filename("greet.ts");
    file.assert_lines_and_blame(lines![
        "import { logger } from './logger';".ai(),
        "".ai(),
        "export function greet(name: string) {".human(),
        "    return `Hello, ${name}!`;".ai(),
        "    logger.log('greet called');".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_cursor_tab_multiple_sessions_same_file() {
    use std::fs;

    let repo = TestRepo::new();

    // Create initial file
    let file_path = repo.path().join("math.ts");
    let base_content = "export function multiply(a: number, b: number) {\n    return a * b;\n}\n";
    fs::write(&file_path, base_content).unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // First Tab session - add a comment
    let before_read_1 = serde_json::json!({
        "conversation_id": "session-1",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": base_content,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_1])
        .unwrap();

    let after_edit_1 = serde_json::json!({
        "conversation_id": "session-1",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "// Multiplication function\n",
            "range": {
                "start_line_number": 1,
                "start_column": 1,
                "end_line_number": 1,
                "end_column": 1
            },
            "old_line": "",
            "new_line": "// Multiplication function"
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_1])
        .unwrap();

    // User saves the file before committing
    let content_after_1 = "// Multiplication function\nexport function multiply(a: number, b: number) {\n    return a * b;\n}\n";
    fs::write(&file_path, content_after_1).unwrap();

    repo.stage_all_and_commit("Tab adds comment").unwrap();

    // Second Tab session - add another function
    let before_read_2 = serde_json::json!({
        "conversation_id": "session-2",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "beforeTabFileRead",
        "file_path": file_path.to_string_lossy().to_string(),
        "content": content_after_1,
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &before_read_2])
        .unwrap();

    let after_edit_2 = serde_json::json!({
        "conversation_id": "session-2",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "hook_event_name": "afterTabFileEdit",
        "file_path": file_path.to_string_lossy().to_string(),
        "edits": [{
            "old_string": "",
            "new_string": "\n// Division function\nexport function divide(a: number, b: number) {\n    return a / b;\n}\n",
            "range": {
                "start_line_number": 5,
                "start_column": 1,
                "end_line_number": 5,
                "end_column": 1
            },
            "old_line": "",
            "new_line": ""
        }],
        "model": "tab"
    })
    .to_string();

    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &after_edit_2])
        .unwrap();

    // User saves the file before committing
    let content_after_2 = "// Multiplication function\nexport function multiply(a: number, b: number) {\n    return a * b;\n}\n\n// Division function\nexport function divide(a: number, b: number) {\n    return a / b;\n}\n";
    fs::write(&file_path, content_after_2).unwrap();

    repo.stage_all_and_commit("Tab adds divide function")
        .unwrap();

    // Verify blame - both Tab sessions' contributions should be attributed
    let mut file = repo.filename("math.ts");
    file.assert_lines_and_blame(lines![
        "// Multiplication function".ai(),
        "export function multiply(a: number, b: number) {".human(),
        "    return a * b;".human(),
        "}".human(),
        "".ai(),
        "// Division function".ai(),
        "export function divide(a: number, b: number) {".ai(),
        "    return a / b;".ai(),
        "}".ai(),
    ]);
}
