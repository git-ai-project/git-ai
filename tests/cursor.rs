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

            let file_path_json = if file_path.is_empty() {
                String::new()
            } else {
                format!(",\n        \"file_path\": \"{}\"", file_path)
            };

            let hook_input = format!(
                r##"{{
        "conversation_id": "test-conversation-id",
        "workspace_roots": [{}],
        "hook_event_name": "beforeSubmitPrompt"{},
        "model": "model-name-from-hook-test"
    }}"##,
                workspace_roots_json.join(", "),
                file_path_json
            );

            let flags = AgentCheckpointFlags {
                hook_input: Some(hook_input),
            };

            let preset = CursorPreset;
            let result = preset
                .run(flags)
                .expect(&format!("Should succeed for: {}", description));

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
