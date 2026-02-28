#[macro_use]
mod repos;
mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::CheckpointKind;
use git_ai::commands::checkpoint_agent::agent_presets::{
    AgentCheckpointFlags, AgentCheckpointPreset,
};
use git_ai::commands::checkpoint_agent::amp_preset::AmpPreset;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use test_utils::fixture_path;

const AMP_SIMPLE_THREAD_ID: &str = "T-019ca1f6-7f21-77b5-a308-65416ebbdf48";
const AMP_SIMPLE_EDIT_TOOL_USE_ID: &str = "toolu_vrtx_01TJD3myjs6gdrDRVn6ZbNME";
const AMP_THINKING_THREAD_ID: &str = "T-019ca1ce-3ae2-7686-a41e-ccc078837f8a";

fn amp_threads_fixture_path() -> PathBuf {
    fixture_path("amp-threads")
}

fn amp_simple_thread_fixture_path() -> PathBuf {
    amp_threads_fixture_path().join(format!("{}.json", AMP_SIMPLE_THREAD_ID))
}

#[test]
fn test_parse_amp_thread_transcript() {
    let thread_path = amp_simple_thread_fixture_path();

    let (transcript, model, thread_id) =
        AmpPreset::transcript_and_model_from_thread_path(&thread_path)
            .expect("Failed to parse Amp thread JSON");

    assert_eq!(thread_id, AMP_SIMPLE_THREAD_ID);
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
    assert!(!transcript.messages().is_empty());

    let has_user = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::User { .. }));
    let has_assistant = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::Assistant { .. }));
    let has_tool_use = transcript
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolUse { .. }));

    assert!(has_user, "Expected at least one user message");
    assert!(has_assistant, "Expected at least one assistant message");
    assert!(has_tool_use, "Expected at least one tool use message");
}

#[test]
fn test_parse_amp_thread_with_thinking_blocks() {
    let thread_path = amp_threads_fixture_path().join(format!("{}.json", AMP_THINKING_THREAD_ID));

    let (transcript, model, thread_id) =
        AmpPreset::transcript_and_model_from_thread_path(&thread_path)
            .expect("Failed to parse Amp thread JSON");

    assert_eq!(thread_id, AMP_THINKING_THREAD_ID);
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));

    let contains_thinking_text = transcript
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { text, .. } => Some(text),
            _ => None,
        })
        .any(|text| text.contains("create a plan"));

    assert!(
        contains_thinking_text,
        "Assistant transcript should include converted thinking blocks"
    );
}

#[test]
#[serial_test::serial]
fn test_amp_preset_pretooluse_returns_human_checkpoint() {
    unsafe {
        std::env::set_var("GIT_AI_AMP_THREADS_PATH", amp_threads_fixture_path());
    }

    let hook_input = json!({
        "hook_event_name": "PreToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/jokes.csv"],
        "tool_input": {
            "path": "/Users/test/project/jokes.csv"
        }
    })
    .to_string();

    let result = AmpPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Amp preset should succeed");

    unsafe {
        std::env::remove_var("GIT_AI_AMP_THREADS_PATH");
    }

    assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
    assert!(result.transcript.is_none());
    assert_eq!(result.agent_id.tool, "amp");
    assert_eq!(result.agent_id.id, AMP_SIMPLE_THREAD_ID);
    assert_eq!(
        result.repo_working_dir.as_deref(),
        Some("/Users/test/project")
    );

    let will_edit = result
        .will_edit_filepaths
        .expect("will_edit_filepaths should be present");
    assert_eq!(will_edit, vec!["/Users/test/project/jokes.csv"]);
}

#[test]
#[serial_test::serial]
fn test_amp_preset_posttooluse_returns_ai_checkpoint() {
    unsafe {
        std::env::set_var("GIT_AI_AMP_THREADS_PATH", amp_threads_fixture_path());
    }

    let hook_input = json!({
        "hook_event_name": "PostToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": "/Users/test/project",
        "edited_filepaths": ["/Users/test/project/jokes.csv"],
        "tool_input": {
            "path": "/Users/test/project/jokes.csv"
        }
    })
    .to_string();

    let result = AmpPreset
        .run(AgentCheckpointFlags {
            hook_input: Some(hook_input),
        })
        .expect("Amp preset should succeed");

    unsafe {
        std::env::remove_var("GIT_AI_AMP_THREADS_PATH");
    }

    assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
    assert!(result.transcript.is_some());
    assert_eq!(result.agent_id.tool, "amp");
    assert_eq!(result.agent_id.id, AMP_SIMPLE_THREAD_ID);
    assert_eq!(result.agent_id.model, "claude-opus-4-6");

    let metadata = result
        .agent_metadata
        .expect("agent metadata should be present for amp checkpoints");
    let transcript_path = metadata
        .get("transcript_path")
        .expect("transcript_path should be present in metadata");
    assert!(
        transcript_path.ends_with(&format!("{}.json", AMP_SIMPLE_THREAD_ID)),
        "transcript_path should point to the matched Amp thread file"
    );
}

#[test]
#[serial_test::serial]
fn test_amp_e2e_checkpoint_and_commit() {
    use repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("src").join("main.ts");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let temp_threads = tempfile::tempdir().unwrap();
    copy_dir_all(&amp_threads_fixture_path(), temp_threads.path()).unwrap();

    unsafe {
        std::env::set_var("GIT_AI_AMP_THREADS_PATH", temp_threads.path());
    }

    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": repo_root.to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "tool_input": {
            "path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(&file_path, "// initial\n// Hello from amp\n").unwrap();

    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": repo_root.to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "tool_input": {
            "path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &post_hook_input])
        .unwrap();

    unsafe {
        std::env::remove_var("GIT_AI_AMP_THREADS_PATH");
    }

    let commit = repo.stage_all_and_commit("Add amp-authored line").unwrap();

    assert!(
        !commit.authorship_log.metadata.prompts.is_empty(),
        "Expected a prompt record after amp checkpoint + commit"
    );

    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("prompt record should exist");

    assert_eq!(prompt_record.agent_id.tool, "amp");
    assert_eq!(prompt_record.agent_id.model, "claude-opus-4-6");
    assert!(
        !prompt_record.messages.is_empty(),
        "Prompt record should include transcript messages"
    );
}

#[test]
#[serial_test::serial]
fn test_amp_post_commit_resyncs_latest_thread_transcript() {
    use repos::test_repo::TestRepo;

    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
    });

    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("src").join("main.ts");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "// initial\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let temp_threads = tempfile::tempdir().unwrap();
    copy_dir_all(&amp_threads_fixture_path(), temp_threads.path()).unwrap();
    let thread_path = temp_threads
        .path()
        .join(format!("{}.json", AMP_SIMPLE_THREAD_ID));

    unsafe {
        std::env::set_var("GIT_AI_AMP_THREADS_PATH", temp_threads.path());
    }

    let pre_hook_input = json!({
        "hook_event_name": "PreToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": repo_root.to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()]
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &pre_hook_input])
        .unwrap();

    fs::write(&file_path, "// initial\n// ai edit\n").unwrap();

    let post_hook_input = json!({
        "hook_event_name": "PostToolUse",
        "tool_use_id": AMP_SIMPLE_EDIT_TOOL_USE_ID,
        "cwd": repo_root.to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()]
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &post_hook_input])
        .unwrap();

    append_assistant_message(
        &thread_path,
        "RESYNC_TEST_MESSAGE: This message was appended after checkpoint",
    );

    unsafe {
        std::env::remove_var("GIT_AI_AMP_THREADS_PATH");
    }

    let commit = repo
        .stage_all_and_commit("Commit with amp transcript resync")
        .unwrap();

    let prompt_record = commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("Expected a prompt record");

    let has_resync_message = prompt_record.messages.iter().any(|msg| match msg {
        Message::Assistant { text, .. } => text.contains("RESYNC_TEST_MESSAGE"),
        _ => false,
    });

    assert!(
        has_resync_message,
        "Post-commit should refresh amp transcript to include latest thread message"
    );
}

fn append_assistant_message(thread_path: &Path, text: &str) {
    let content = fs::read_to_string(thread_path).expect("Failed to read thread file");
    let mut value: serde_json::Value =
        serde_json::from_str(&content).expect("Failed to parse thread JSON");

    let next_message_id = value
        .get("nextMessageId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let new_message = json!({
        "role": "assistant",
        "messageId": next_message_id,
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "state": {
            "type": "complete",
            "stopReason": "end_turn"
        },
        "usage": {
            "model": "claude-opus-4-6",
            "timestamp": "2026-02-28T02:00:00.000Z"
        }
    });

    value
        .get_mut("messages")
        .and_then(|messages| messages.as_array_mut())
        .expect("Thread should contain a messages array")
        .push(new_message);

    value["nextMessageId"] = json!(next_message_id + 1);

    let serialized = serde_json::to_string_pretty(&value).expect("Failed to serialize thread JSON");
    fs::write(thread_path, serialized).expect("Failed to write updated thread file");
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
