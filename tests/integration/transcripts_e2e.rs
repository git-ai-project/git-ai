//! End-to-end integration tests for transcript processing system.
//!
//! Tests the database integration and session record management.
//! The actual transcript processing and metrics emission are tested via
//! daemon tests and manual verification.

use git_ai::transcripts::agent::Agent;
use git_ai::transcripts::agents::ClaudeAgent;
use git_ai::transcripts::watermark::ByteOffsetWatermark;
use git_ai::transcripts::{SessionRecord, TranscriptsDatabase};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

#[allow(dead_code)]
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("transcripts")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_session_database_basic() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("transcripts.db");
    let db = TranscriptsDatabase::open(&db_path).unwrap();

    let now = chrono::Utc::now().timestamp();
    let session = SessionRecord {
        session_id: "s_test_123".to_string(),
        agent_type: "claude".to_string(),
        transcript_path: "/path/to/transcript.jsonl".to_string(),
        transcript_format: "claude-jsonl".to_string(),
        watermark_type: "byte_offset".to_string(),
        watermark_value: "0".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        tool: Some("claude-code".to_string()),
        external_thread_id: None,
        first_seen_at: now,
        last_processed_at: now,
        last_known_size: 0,
        last_modified: None,
        processing_errors: 0,
        last_error: None,
    };

    // Insert
    db.insert_session(&session).unwrap();

    // Read
    let retrieved = db.get_session("s_test_123").unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.session_id, "s_test_123");
    assert_eq!(retrieved.agent_type, "claude");
    assert_eq!(retrieved.processing_errors, 0);

    // Update watermark
    let new_watermark = ByteOffsetWatermark::new(100);
    db.update_watermark("s_test_123", &new_watermark).unwrap();
    let retrieved_updated = db.get_session("s_test_123").unwrap().unwrap();
    assert_eq!(retrieved_updated.watermark_value, "100");

    // List all sessions
    let all_sessions = db.all_sessions().unwrap();
    assert_eq!(all_sessions.len(), 1);
    assert_eq!(all_sessions[0].session_id, "s_test_123");
}

#[test]
fn test_watermark_integration() {
    let temp_dir = TempDir::new().unwrap();
    let transcript_file = temp_dir.path().join("watermark_test.jsonl");

    // Write initial content
    let mut file = File::create(&transcript_file).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","message":{{"content":"First"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();
    drop(file);

    // Read from start
    let agent = ClaudeAgent::new();
    let watermark1 = Box::new(ByteOffsetWatermark::new(0));
    let result1 = agent
        .read_incremental(&transcript_file, watermark1, "s_test")
        .unwrap();
    assert_eq!(result1.events.len(), 1);

    let offset1: u64 = result1.new_watermark.serialize().parse().unwrap();
    assert!(offset1 > 0, "Watermark should advance");

    // Append more content
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript_file)
        .unwrap();
    writeln!(
        file,
        r#"{{"type":"user","message":{{"content":"Second"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
    )
    .unwrap();
    file.flush().unwrap();
    drop(file);

    // Read from watermark - should only get new line
    let watermark2 = Box::new(ByteOffsetWatermark::new(offset1));
    let result2 = agent
        .read_incremental(&transcript_file, watermark2, "s_test")
        .unwrap();
    assert_eq!(result2.events.len(), 1);
    assert_eq!(
        result2.events[0]["message"]["content"].as_str(),
        Some("Second")
    );

    let offset2: u64 = result2.new_watermark.serialize().parse().unwrap();
    assert!(offset2 > offset1, "Watermark should continue advancing");
}

#[test]
fn test_multiple_sessions_isolation() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("transcripts.db");
    let db = TranscriptsDatabase::open(&db_path).unwrap();

    let now = chrono::Utc::now().timestamp();

    // Create multiple sessions
    for i in 0..5 {
        let session = SessionRecord {
            session_id: format!("s_session_{}", i),
            agent_type: "claude".to_string(),
            transcript_path: format!("/path/to/transcript_{}.jsonl", i),
            transcript_format: "claude-jsonl".to_string(),
            watermark_type: "byte_offset".to_string(),
            watermark_value: (i * 10).to_string(),
            model: Some("claude-sonnet-4".to_string()),
            tool: Some("claude-code".to_string()),
            external_thread_id: None,
            first_seen_at: now,
            last_processed_at: now,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
        };
        db.insert_session(&session).unwrap();
    }

    // Verify all sessions exist independently
    let all_sessions = db.all_sessions().unwrap();
    assert_eq!(all_sessions.len(), 5);

    // Verify each session has correct data
    for i in 0..5 {
        let session = db
            .get_session(&format!("s_session_{}", i))
            .unwrap()
            .unwrap();
        assert_eq!(session.watermark_value, (i * 10).to_string());
        assert_eq!(session.processing_errors, 0);
    }
}

#[test]
fn test_database_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("transcripts.db");

    let now = chrono::Utc::now().timestamp();

    // Create and close database
    {
        let db = TranscriptsDatabase::open(&db_path).unwrap();
        let session = SessionRecord {
            session_id: "s_persist".to_string(),
            agent_type: "claude".to_string(),
            transcript_path: "/path/to/transcript.jsonl".to_string(),
            transcript_format: "claude-jsonl".to_string(),
            watermark_type: "byte_offset".to_string(),
            watermark_value: "42".to_string(),
            model: Some("claude-sonnet-4".to_string()),
            tool: Some("claude-code".to_string()),
            external_thread_id: None,
            first_seen_at: now,
            last_processed_at: now,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
        };
        db.insert_session(&session).unwrap();
    }

    // Reopen database
    {
        let db = TranscriptsDatabase::open(&db_path).unwrap();
        let retrieved = db.get_session("s_persist").unwrap().unwrap();
        assert_eq!(retrieved.session_id, "s_persist");
        assert_eq!(retrieved.watermark_value, "42");
        assert_eq!(retrieved.processing_errors, 0);
    }
}

#[test]
fn test_error_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("transcripts.db");
    let db = TranscriptsDatabase::open(&db_path).unwrap();

    let now = chrono::Utc::now().timestamp();
    let session = SessionRecord {
        session_id: "s_errors".to_string(),
        agent_type: "claude".to_string(),
        transcript_path: "/path/to/transcript.jsonl".to_string(),
        transcript_format: "claude-jsonl".to_string(),
        watermark_type: "byte_offset".to_string(),
        watermark_value: "0".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        tool: Some("claude-code".to_string()),
        external_thread_id: None,
        first_seen_at: now,
        last_processed_at: now,
        last_known_size: 0,
        last_modified: None,
        processing_errors: 0,
        last_error: None,
    };

    db.insert_session(&session).unwrap();

    // Simulate errors
    db.record_error("s_errors", "First error").unwrap();
    let retrieved = db.get_session("s_errors").unwrap().unwrap();
    assert_eq!(retrieved.processing_errors, 1);
    assert_eq!(retrieved.last_error, Some("First error".to_string()));

    // More errors
    db.record_error("s_errors", "Second error").unwrap();
    let retrieved2 = db.get_session("s_errors").unwrap().unwrap();
    assert_eq!(retrieved2.processing_errors, 2);
    assert_eq!(retrieved2.last_error, Some("Second error".to_string()));
}
