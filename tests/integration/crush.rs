use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::presets::{ParsedHookEvent, resolve_preset};
use git_ai::error::GitAiError;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn parse_crush(hook_input: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
    resolve_preset("crush")?.parse(hook_input, "t_test")
}

/// Create a minimal Crush session database (as written by Crush under
/// `<project>/.crush/crush.db`) containing one assistant message for the
/// given session and model.
fn create_crush_db(dir: &Path, session_id: &str, model: &str) -> PathBuf {
    let crush_dir = dir.join(".crush");
    fs::create_dir_all(&crush_dir).unwrap();
    let db_path = crush_dir.join("crush.db");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (\
            id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', \
            message_count INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0\
         ); \
         CREATE TABLE IF NOT EXISTS messages (\
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, \
            parts TEXT NOT NULL DEFAULT '[]', model TEXT, \
            created_at INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL DEFAULT 0\
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (id, session_id, role, parts, model, created_at, updated_at) \
         VALUES (?1, ?2, 'assistant', '[]', ?3, 1000, 1000)",
        rusqlite::params!["msg-1", session_id, model],
    )
    .unwrap();
    db_path
}

fn make_hook_input(event: &str, tool_name: &str, cwd: &str, file_path: &str) -> String {
    json!({
        "event": event,
        "session_id": "crush-session-123",
        "cwd": cwd,
        "tool_name": tool_name,
        "tool_input": {"file_path": file_path, "content": "fn main() {}"}
    })
    .to_string()
}

#[test]
#[serial_test::serial]
fn test_crush_preset_pretooluse_returns_human_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    fs::create_dir_all(&project_dir).unwrap();
    create_crush_db(&project_dir, "crush-session-123", "glm-53-fp8");

    let hook_input = make_hook_input(
        "PreToolUse",
        "write",
        project_dir.to_str().unwrap(),
        project_dir.join("src/main.rs").to_str().unwrap(),
    );

    let events = parse_crush(&hook_input).expect("Failed to run CrushPreset");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "crush");
            assert_eq!(e.context.external_session_id, "crush-session-123");
            assert_eq!(e.context.cwd, project_dir.clone());
            assert_eq!(e.file_paths, vec![project_dir.join("src/main.rs")]);
            // Model is resolved from the project's crush.db at parse time
            assert_eq!(e.context.agent_id.model, "glm-53-fp8");
        }
        _ => panic!("Expected PreFileEdit for PreToolUse"),
    }
}

#[test]
#[serial_test::serial]
fn test_crush_preset_posttooluse_returns_ai_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    fs::create_dir_all(&project_dir).unwrap();
    create_crush_db(
        &project_dir,
        "crush-session-123",
        "custom-gateway/glm-53-fp8",
    );

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "crush-session-123",
        "cwd": project_dir.to_string_lossy().to_string(),
        "tool_name": "edit",
        "tool_input": {"file_path": project_dir.join("src/main.rs").to_string_lossy().to_string()}
    })
    .to_string();

    let events = parse_crush(&hook_input).expect("Failed to run CrushPreset");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PostFileEdit(e) => {
            assert_eq!(e.context.agent_id.tool, "crush");
            // Provider prefix is stripped
            assert_eq!(e.context.agent_id.model, "glm-53-fp8");
            assert!(!e.file_paths.is_empty());
            assert!(e.stream_source.is_none());
        }
        _ => panic!("Expected PostFileEdit for PostToolUse"),
    }
}

#[test]
#[serial_test::serial]
fn test_crush_preset_pretooluse_bash_returns_bash_call() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    fs::create_dir_all(&project_dir).unwrap();

    let hook_input = json!({
        "event": "PreToolUse",
        "session_id": "crush-session-123",
        "cwd": project_dir.to_string_lossy().to_string(),
        "tool_name": "bash",
        "tool_input": {"command": "echo line >> notes.txt"}
    })
    .to_string();

    let events = parse_crush(&hook_input).expect("Failed to run CrushPreset");

    assert_eq!(events.len(), 1);
    match &events[0] {
        ParsedHookEvent::PreBashCall(e) => {
            assert_eq!(e.command.as_deref(), Some("echo line >> notes.txt"));
        }
        _ => panic!("Expected PreBashCall for bash tool"),
    }
}

#[test]
fn test_crush_preset_rejects_unsupported_tool() {
    let hook_input = json!({
        "event": "PreToolUse",
        "session_id": "crush-session-123",
        "cwd": "/tmp/project",
        "tool_name": "web_fetch",
        "tool_input": {}
    })
    .to_string();

    assert!(parse_crush(&hook_input).is_err());
}

#[test]
fn test_crush_preset_rejects_unsupported_event() {
    let hook_input = json!({
        "hook_event_name": "Stop",
        "session_id": "crush-session-123",
        "cwd": "/tmp/project",
        "tool_name": "write",
        "tool_input": {"file_path": "/tmp/project/a.txt"}
    })
    .to_string();

    assert!(parse_crush(&hook_input).is_err());
}

#[test]
fn test_crush_preset_unknown_model_without_db() {
    let hook_input = make_hook_input(
        "PreToolUse",
        "write",
        "/definitely/not/a/crush/project",
        "/definitely/not/a/crush/project/a.txt",
    );

    let events = parse_crush(&hook_input).expect("Failed to run CrushPreset");
    match &events[0] {
        ParsedHookEvent::PreFileEdit(e) => {
            assert_eq!(e.context.agent_id.model, "unknown");
        }
        _ => panic!("Expected PreFileEdit"),
    }
}

#[test]
#[serial_test::serial]
fn test_crush_e2e_pre_and_post_checkpoints_attribute_edit_to_ai() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });
    let repo_root = repo.canonical_path();

    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Crush writes its session DB under the project root
    create_crush_db(&repo_root, "crush-session-123", "glm-53-fp8");

    let pre_hook_input = make_hook_input(
        "PreToolUse",
        "write",
        repo_root.to_str().unwrap(),
        file_path.to_str().unwrap(),
    );
    repo.git_ai(&["checkpoint", "crush", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(&file_path, "fn main() { println!(\"crush\"); }\n").unwrap();

    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "crush-session-123",
        "cwd": repo_root.to_string_lossy().to_string(),
        "tool_name": "write",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "crush", "--hook-input", &post_hook_input])
        .unwrap();
    repo.sync_daemon_force();

    let commit = repo.stage_all_and_commit("Add crush AI line").unwrap();

    // Crush has no transcript stream source, so no session records are
    // emitted; attribution correctness is verified through blame below.
    assert!(!commit.authorship_log.attestations.is_empty());

    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(crate::lines!["fn main() { println!(\"crush\"); }".ai(),]);
}

#[test]
#[serial_test::serial]
fn test_crush_pre_checkpoint_keeps_subsequent_manual_edits_human() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();

    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    create_crush_db(&repo_root, "crush-session-123", "glm-53-fp8");

    let pre_hook_input = make_hook_input(
        "PreToolUse",
        "write",
        repo_root.to_str().unwrap(),
        file_path.to_str().unwrap(),
    );
    repo.git_ai(&["checkpoint", "crush", "--hook-input", &pre_hook_input])
        .unwrap();

    // No PostToolUse checkpoint: the edit stays human-authored. The Human
    // pre-mark may record a human prompt attestation (h_... hash), but the
    // lines must never be attributed to the AI agent.
    fs::write(&file_path, "fn main() { println!(\"human\"); }\n").unwrap();
    let commit = repo
        .stage_all_and_commit("Human edit after crush pre-mark")
        .unwrap();

    for attestation in &commit.authorship_log.attestations {
        for entry in &attestation.entries {
            assert!(
                entry.hash.starts_with("h_"),
                "All attestations should be human prompts, got {}",
                entry.hash
            );
        }
    }

    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(crate::lines!["fn main() { println!(\"human\"); }".human(),]);
}
