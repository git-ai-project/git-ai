// DISABLED: transcripts::formats module removed
// //! Integration tests for Claude Code transcript reader.
// 
// use git_ai::transcripts::formats::claude::read_incremental;
// use git_ai::transcripts::watermark::ByteOffsetWatermark;
// use std::fs::{self, File};
// use std::io::Write;
// use std::path::PathBuf;
// use tempfile::TempDir;
// 
// fn fixture_path(name: &str) -> PathBuf {
//     PathBuf::from(env!("CARGO_MANIFEST_DIR"))
//         .join("tests")
//         .join("transcripts")
//         .join("fixtures")
//         .join(name)
// }
// 
// #[test]
// fn test_claude_reader_with_fixture() {
//     let path = fixture_path("claude_simple.jsonl");
//     let watermark = Box::new(ByteOffsetWatermark::new(0));
// 
//     let result = read_incremental(&path, watermark, "test-session").unwrap();
// 
//     // Should have 3 events: user message, assistant text, tool use
//     assert_eq!(result.events.len(), 3);
//     assert_eq!(result.model, Some("claude-sonnet-4".to_string()));
// 
//     // Event 0: User message
//     let event0 = &result.events[0];
//     assert_eq!(event0.event_type, Some(Some("user_message".to_string())));
//     assert_eq!(
//         event0.prompt_text,
//         Some(Some("Write a hello world function".to_string()))
//     );
//     assert!(event0.event_ts.is_some());
// 
//     // Event 1: Assistant text
//     let event1 = &result.events[1];
//     assert_eq!(
//         event1.event_type,
//         Some(Some("assistant_message".to_string()))
//     );
//     assert_eq!(
//         event1.response_text,
//         Some(Some(
//             "I'll create a hello world function for you.".to_string()
//         ))
//     );
// 
//     // Event 2: Tool use
//     let event2 = &result.events[2];
//     assert_eq!(event2.event_type, Some(Some("tool_use".to_string())));
//     assert_eq!(event2.tool_name, Some(Some("Write".to_string())));
//     assert_eq!(event2.tool_use_id, Some(Some("toolu_abc123".to_string())));
// }
// 
// #[test]
// fn test_claude_reader_watermark_resume() {
//     let temp_dir = TempDir::new().unwrap();
//     let file_path = temp_dir.path().join("transcript.jsonl");
// 
//     // Write initial content
//     let mut file = File::create(&file_path).unwrap();
//     writeln!(
//         file,
//         r#"{{"type":"user","message":{{"content":"First message"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
//     )
//     .unwrap();
//     file.flush().unwrap();
//     drop(file);
// 
//     // Read from start
//     let watermark1 = Box::new(ByteOffsetWatermark::new(0));
//     let result1 = read_incremental(&file_path, watermark1, "test-session").unwrap();
//     assert_eq!(result1.events.len(), 1);
// 
//     // Save watermark position
//     let offset_after_first = result1.new_watermark.serialize().parse::<u64>().unwrap();
// 
//     // Append more content
//     let mut file = fs::OpenOptions::new()
//         .append(true)
//         .open(&file_path)
//         .unwrap();
//     writeln!(
//         file,
//         r#"{{"type":"user","message":{{"content":"Second message"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
//     )
//     .unwrap();
//     file.flush().unwrap();
//     drop(file);
// 
//     // Read from watermark - should only get new line
//     let watermark2 = Box::new(ByteOffsetWatermark::new(offset_after_first));
//     let result2 = read_incremental(&file_path, watermark2, "test-session").unwrap();
//     assert_eq!(result2.events.len(), 1);
//     assert_eq!(
//         result2.events[0].prompt_text,
//         Some(Some("Second message".to_string()))
//     );
// 
//     // Verify watermark advanced
//     let offset_after_second = result2.new_watermark.serialize().parse::<u64>().unwrap();
//     assert!(offset_after_second > offset_after_first);
// }
// 
// #[test]
// fn test_claude_reader_handles_malformed_json() {
//     let temp_dir = TempDir::new().unwrap();
//     let file_path = temp_dir.path().join("malformed.jsonl");
// 
//     let mut file = File::create(&file_path).unwrap();
//     writeln!(file, "{{invalid json syntax}}").unwrap();
//     file.flush().unwrap();
// 
//     let watermark = Box::new(ByteOffsetWatermark::new(0));
//     let result = read_incremental(&file_path, watermark, "test-session");
// 
//     assert!(result.is_err());
//     if let Err(e) = result {
//         match e {
//             git_ai::transcripts::types::TranscriptError::Parse { line, message } => {
//                 assert_eq!(line, 1);
//                 assert!(message.contains("Invalid JSON"));
//             }
//             _ => panic!("Expected Parse error, got {:?}", e),
//         }
//     }
// }
// 
// #[test]
// fn test_claude_reader_file_not_found() {
//     let path = PathBuf::from("/nonexistent/transcript.jsonl");
//     let watermark = Box::new(ByteOffsetWatermark::new(0));
//     let result = read_incremental(&path, watermark, "test-session");
// 
//     assert!(result.is_err());
//     if let Err(e) = result {
//         match e {
//             git_ai::transcripts::types::TranscriptError::Fatal { message } => {
//                 assert!(message.contains("not found"));
//             }
//             _ => panic!("Expected Fatal error, got {:?}", e),
//         }
//     }
// }
// 
// #[test]
// fn test_claude_reader_thinking_blocks() {
//     let temp_dir = TempDir::new().unwrap();
//     let file_path = temp_dir.path().join("thinking.jsonl");
// 
//     let mut file = File::create(&file_path).unwrap();
//     writeln!(
//         file,
//         r#"{{"type":"assistant","message":{{"content":[{{"type":"thinking","thinking":"Let me think about this..."}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
//     )
//     .unwrap();
//     file.flush().unwrap();
// 
//     let watermark = Box::new(ByteOffsetWatermark::new(0));
//     let result = read_incremental(&file_path, watermark, "test-session").unwrap();
// 
//     assert_eq!(result.events.len(), 1);
//     assert_eq!(
//         result.events[0].event_type,
//         Some(Some("assistant_thinking".to_string()))
//     );
//     assert_eq!(
//         result.events[0].response_text,
//         Some(Some("Let me think about this...".to_string()))
//     );
// }
// 
// #[test]
// fn test_claude_reader_mixed_content() {
//     let temp_dir = TempDir::new().unwrap();
//     let file_path = temp_dir.path().join("mixed.jsonl");
// 
//     let mut file = File::create(&file_path).unwrap();
//     writeln!(
//         file,
//         r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Here's the code:"}},{{"type":"tool_use","name":"Write","id":"toolu_xyz"}},{{"type":"text","text":"Done!"}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
//     )
//     .unwrap();
//     file.flush().unwrap();
// 
//     let watermark = Box::new(ByteOffsetWatermark::new(0));
//     let result = read_incremental(&file_path, watermark, "test-session").unwrap();
// 
//     // Should have 3 events: text, tool_use, text
//     assert_eq!(result.events.len(), 3);
//     assert_eq!(
//         result.events[0].event_type,
//         Some(Some("assistant_message".to_string()))
//     );
//     assert_eq!(
//         result.events[1].event_type,
//         Some(Some("tool_use".to_string()))
//     );
//     assert_eq!(
//         result.events[2].event_type,
//         Some(Some("assistant_message".to_string()))
//     );
// }
