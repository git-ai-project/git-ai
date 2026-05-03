use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use serde_json::json;
use std::path::PathBuf;

fn parse_agent_v1(hook_input: &str) -> Result<Vec<ParsedHookEvent>, git_ai::error::GitAiError> {
    resolve_preset("agent-v1")?.parse(hook_input, "t_test")
}

#[test]
fn test_agent_v1_human_checkpoint_with_dirty_files() {
    let hook_input = json!({
        "type": "human",
        "repo_working_dir": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"],
        "dirty_files": {
            "/Users/test/project/file.ts": "console.log('hello');"
        }
    })
    .to_string();

    let events = parse_agent_v1(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "human");
            assert_eq!(e.context.agent_id.id, "human");
            assert_eq!(e.context.agent_id.model, "human");
            assert_eq!(e.context.cwd, PathBuf::from("/Users/test/project"));
            assert_eq!(
                e.file_paths,
                vec![PathBuf::from("/Users/test/project/file.ts")]
            );
            let dirty_files = e.dirty_files.as_ref().unwrap();
            assert_eq!(dirty_files.len(), 1);
            assert_eq!(
                dirty_files
                    .get(&PathBuf::from("/Users/test/project/file.ts"))
                    .unwrap(),
                "console.log('hello');"
            );
        }
        _ => panic!("Expected PreFileEdit for human checkpoint"),
    }
}

#[test]
fn test_agent_v1_ai_agent_checkpoint_with_dirty_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123",
        "dirty_files": {
            "/Users/test/project/file.ts": "console.log('hello');"
        }
    })
    .to_string();

    let events = parse_agent_v1(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "test-agent");
            assert_eq!(e.context.agent_id.id, "test-123");
            assert_eq!(e.context.agent_id.model, "test-model");
            assert_eq!(e.context.cwd, PathBuf::from("/Users/test/project"));
            assert_eq!(
                e.file_paths,
                vec![PathBuf::from("/Users/test/project/file.ts")]
            );
            let dirty_files = e.dirty_files.as_ref().unwrap();
            assert_eq!(dirty_files.len(), 1);
            assert_eq!(
                dirty_files
                    .get(&PathBuf::from("/Users/test/project/file.ts"))
                    .unwrap(),
                "console.log('hello');"
            );
            // Inline transcripts removed - should now be None
            assert!(e.transcript_source.is_none());
        }
        _ => panic!("Expected PostFileEdit for ai_agent checkpoint"),
    }
}

#[test]
fn test_agent_v1_human_checkpoint_without_dirty_files() {
    let hook_input = json!({
        "type": "human",
        "repo_working_dir": "/Users/test/project",
        "will_edit_filepaths": ["/Users/test/project/file.ts"]
    })
    .to_string();

    let events = parse_agent_v1(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert!(e.dirty_files.is_none());
            assert_eq!(
                e.file_paths,
                vec![PathBuf::from("/Users/test/project/file.ts")]
            );
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
fn test_agent_v1_ai_agent_checkpoint_without_dirty_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123"
    })
    .to_string();

    let events = parse_agent_v1(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert!(e.dirty_files.is_none());
            assert_eq!(
                e.file_paths,
                vec![PathBuf::from("/Users/test/project/file.ts")]
            );
            assert_eq!(e.context.agent_id.tool, "test-agent");
            assert_eq!(e.context.agent_id.id, "test-123");
            assert_eq!(e.context.agent_id.model, "test-model");
        }
        _ => panic!("Expected PostFileEdit"),
    }
}

#[test]
fn test_agent_v1_dirty_files_multiple_files() {
    let hook_input = json!({
        "type": "ai_agent",
        "repo_working_dir": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/file1.ts", "/Users/test/project/file2.ts"],
        "transcript": {"messages": []},
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-123",
        "dirty_files": {
            "/Users/test/project/file1.ts": "content1",
            "/Users/test/project/file2.ts": "content2"
        }
    })
    .to_string();

    let events = parse_agent_v1(&hook_input).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            let dirty_files = e.dirty_files.as_ref().unwrap();
            assert_eq!(dirty_files.len(), 2);
            assert_eq!(
                dirty_files
                    .get(&PathBuf::from("/Users/test/project/file1.ts"))
                    .unwrap(),
                "content1"
            );
            assert_eq!(
                dirty_files
                    .get(&PathBuf::from("/Users/test/project/file2.ts"))
                    .unwrap(),
                "content2"
            );
            assert_eq!(e.file_paths.len(), 2);
        }
        _ => panic!("Expected PostFileEdit"),
    }
}
