use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// =============================================================================
// Cursor
// =============================================================================

#[test]
fn test_cursor_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.rs");

    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-cursor-123",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "fn main() {}\nfn ai_added() {}\n").unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-cursor-123",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("AI edit").unwrap();

    let mut file = repo.filename("main.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() {}".human(),
        "fn ai_added() {}".ai(),
    ]);
}

// =============================================================================
// Claude
// =============================================================================

#[test]
fn test_claude_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("lib.rs");

    fs::write(&file_path, "pub fn greet() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-claude-001",
        "tool_name": "Edit",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "cwd": repo.canonical_path().to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "pub fn greet() {}\npub fn farewell() {}\n").unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-claude-001",
        "tool_name": "Edit",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "cwd": repo.canonical_path().to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Claude edit").unwrap();

    let mut file = repo.filename("lib.rs");
    file.assert_lines_and_blame(crate::lines![
        "pub fn greet() {}".human(),
        "pub fn farewell() {}".ai(),
    ]);
}

// =============================================================================
// Gemini
// =============================================================================

#[test]
fn test_gemini_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("index.ts");

    fs::write(&file_path, "console.log('hello');\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "index.ts"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "BeforeTool",
        "session_id": "gemini-session-001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-gemini-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "console.log('hello');\nconsole.log('from gemini');\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "AfterTool",
        "session_id": "gemini-session-001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-gemini-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "gemini", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Gemini edit").unwrap();

    let mut file = repo.filename("index.ts");
    file.assert_lines_and_blame(crate::lines![
        "console.log('hello');".human(),
        "console.log('from gemini');".ai(),
    ]);
}

// =============================================================================
// Codex
// =============================================================================

#[test]
fn test_codex_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("app.py");

    fs::write(&file_path, "def main():\n    pass\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "app.py"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "codex-session-001",
        "tool_name": "apply_patch",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-codex-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "def main():\n    pass\n\ndef ai_func():\n    return 42\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "codex-session-001",
        "tool_name": "apply_patch",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-codex-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Codex edit").unwrap();

    let mut file = repo.filename("app.py");
    file.assert_lines_and_blame(crate::lines![
        "def main():".human(),
        "    pass".human(),
        "".ai(),
        "def ai_func():".ai(),
        "    return 42".ai(),
    ]);
}

// =============================================================================
// GitHub Copilot
// =============================================================================

#[test]
fn test_github_copilot_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("component.tsx");

    fs::write(&file_path, "export const App = () => {};\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "component.tsx"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "before_edit",
        "workspaceFolder": repo.canonical_path().to_string_lossy().to_string(),
        "will_edit_filepaths": [file_path.to_string_lossy().to_string()],
        "dirty_files": {
            file_path.to_string_lossy().to_string(): "export const App = () => {};\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "github-copilot", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "export const App = () => {};\nexport const Helper = () => {};\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "after_edit",
        "workspaceFolder": repo.canonical_path().to_string_lossy().to_string(),
        "sessionId": "copilot-session-001",
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "dirty_files": {
            file_path.to_string_lossy().to_string(): "export const App = () => {};\nexport const Helper = () => {};\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "github-copilot", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Copilot edit").unwrap();

    let mut file = repo.filename("component.tsx");
    file.assert_lines_and_blame(crate::lines![
        "export const App = () => {};".human(),
        "export const Helper = () => {};".ai(),
    ]);
}

// =============================================================================
// Windsurf
// =============================================================================

#[test]
fn test_windsurf_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("server.ts");

    fs::write(&file_path, "const port = 3000;\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Windsurf post_write_code alone is sufficient for AI attribution
    // (diffs against last committed state)
    fs::write(
        &file_path,
        "const port = 3000;\nconst host = 'localhost';\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "trajectory_id": "traj-windsurf-001",
        "agent_action_name": "post_write_code",
        "model_name": "GPT 4.1",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_info": {
            "file_path": file_path.to_string_lossy().to_string(),
            "transcript_path": "/tmp/fake-windsurf-transcript.jsonl"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Windsurf edit").unwrap();

    let mut file = repo.filename("server.ts");
    file.assert_lines_and_blame(crate::lines![
        "const port = 3000;".human(),
        "const host = 'localhost';".ai(),
    ]);
}

// =============================================================================
// Amp
// =============================================================================

#[test]
fn test_amp_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("handler.ts");

    fs::write(&file_path, "export function handle() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "handler.ts"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_use_id": "toolu_amp_001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "tool_input": {
            "path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "export function handle() {}\nexport function process() {}\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_use_id": "toolu_amp_001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "tool_input": {
            "path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "amp", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Amp edit").unwrap();

    let mut file = repo.filename("handler.ts");
    file.assert_lines_and_blame(crate::lines![
        "export function handle() {}".human(),
        "export function process() {}".ai(),
    ]);
}

// =============================================================================
// Continue CLI
// =============================================================================

#[test]
fn test_continue_cli_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("utils.ts");

    fs::write(&file_path, "export const VERSION = '1.0';\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "utils.ts"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "continue-session-001",
        "model": "claude-3-5-sonnet",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-continue-transcript.json"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "export const VERSION = '1.0';\nexport const NAME = 'app';\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "continue-session-001",
        "model": "claude-3-5-sonnet",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcript_path": "/tmp/fake-continue-transcript.json"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "continue-cli", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Continue CLI edit").unwrap();

    let mut file = repo.filename("utils.ts");
    file.assert_lines_and_blame(crate::lines![
        "export const VERSION = '1.0';".human(),
        "export const NAME = 'app';".ai(),
    ]);
}

// =============================================================================
// Droid
// =============================================================================

#[test]
fn test_droid_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("config.ts");

    fs::write(&file_path, "export const DEBUG = false;\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "config.ts"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Droid requires a transcript file + settings file
    let transcript_path = repo_root.join("droid-session.jsonl");
    let settings_path = repo_root.join("droid-session.settings.json");
    fs::write(&transcript_path, "").unwrap();
    fs::write(&settings_path, r#"{"model":"test-droid-model"}"#).unwrap();

    let pre_hook = serde_json::json!({
        "hookEventName": "PreToolUse",
        "sessionId": "droid-session-001",
        "cwd": repo_root.to_string_lossy().to_string(),
        "toolName": "ApplyPatch",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcriptPath": transcript_path.to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "export const DEBUG = false;\nexport const LOG_LEVEL = 'info';\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hookEventName": "PostToolUse",
        "sessionId": "droid-session-001",
        "cwd": repo_root.to_string_lossy().to_string(),
        "toolName": "ApplyPatch",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "transcriptPath": transcript_path.to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "droid", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Droid edit").unwrap();

    let mut file = repo.filename("config.ts");
    file.assert_lines_and_blame(crate::lines![
        "export const DEBUG = false;".human(),
        "export const LOG_LEVEL = 'info';".ai(),
    ]);
}

// =============================================================================
// Firebender
// =============================================================================

#[test]
fn test_firebender_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.rs");

    fs::write(&file_path, "fn main() { println!(\"hi\"); }\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "preToolUse",
        "model": "gpt-5",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "tool_name": "Write",
        "tool_input": {"file_path": "main.rs"},
        "completion_id": "fb-completion-001"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "firebender", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "fn main() { println!(\"hi\"); }\nfn helper() {}\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "postToolUse",
        "model": "gpt-5",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "tool_name": "Write",
        "tool_input": {"file_path": "main.rs"},
        "completion_id": "fb-completion-001"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "firebender", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Firebender edit").unwrap();

    let mut file = repo.filename("main.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() { println!(\"hi\"); }".human(),
        "fn helper() {}".ai(),
    ]);
}

// =============================================================================
// OpenCode
// =============================================================================

#[test]
fn test_opencode_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("service.go");

    fs::write(&file_path, "package main\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "service.go"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "opencode-session-001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"filePath": file_path.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "package main\n\nfunc init() {}\n").unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "opencode-session-001",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_input": {"filePath": file_path.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "opencode", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("OpenCode edit").unwrap();

    let mut file = repo.filename("service.go");
    file.assert_lines_and_blame(crate::lines![
        "package main".human(),
        "".ai(),
        "func init() {}".ai(),
    ]);
}

// =============================================================================
// Pi
// =============================================================================

#[test]
#[ignore] // Pi tests disabled (transcript enrichment removed)
fn test_pi_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let src_dir = repo_root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("main.rs");

    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/main.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "hook_event_name": "before_edit",
        "session_id": "pi-session-e2e-001",
        "session_path": "/tmp/fake-pi-session.jsonl",
        "cwd": repo_root.to_string_lossy().to_string(),
        "model": "anthropic/claude-sonnet-4-5",
        "tool_name": "edit",
        "tool_name_raw": "edit",
        "will_edit_filepaths": [file_path.to_string_lossy().to_string()],
        "dirty_files": {
            file_path.to_string_lossy().to_string(): "fn main() {}\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "pi", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "fn main() {}\nfn pi_added() {}\n").unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "after_edit",
        "session_id": "pi-session-e2e-001",
        "session_path": "/tmp/fake-pi-session.jsonl",
        "cwd": repo_root.to_string_lossy().to_string(),
        "model": "anthropic/claude-sonnet-4-5",
        "tool_name": "edit",
        "tool_name_raw": "edit",
        "edited_filepaths": [file_path.to_string_lossy().to_string()],
        "dirty_files": {
            file_path.to_string_lossy().to_string(): "fn main() {}\nfn pi_added() {}\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "pi", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Pi edit").unwrap();

    let mut file = repo.filename("src/main.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() {}".human(),
        "fn pi_added() {}".ai(),
    ]);
}

// =============================================================================
// AI Tab
// =============================================================================

#[test]
fn test_ai_tab_preset_e2e_checkpoint_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.canonical_path().join("editor.ts");

    fs::write(&file_path, "const editor = 'vscode';\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "editor.ts"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let file_path_str = file_path.to_string_lossy().to_string();
    let base_content = "const editor = 'vscode';\n".to_string();

    let pre_hook = serde_json::json!({
        "hook_event_name": "before_edit",
        "tool": "github-copilot-tab",
        "model": "default",
        "repo_working_dir": repo.canonical_path().to_string_lossy().to_string(),
        "will_edit_filepaths": [file_path_str.clone()],
        "completion_id": "aitab-completion-001",
        "dirty_files": {
            file_path_str.clone(): base_content
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "ai_tab", "--hook-input", &pre_hook])
        .unwrap();

    let ai_content = "const editor = 'vscode';\nconst theme = 'dark';\n".to_string();
    fs::write(&file_path, &ai_content).unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "after_edit",
        "tool": "github-copilot-tab",
        "model": "default",
        "repo_working_dir": repo.canonical_path().to_string_lossy().to_string(),
        "edited_filepaths": [file_path_str.clone()],
        "completion_id": "aitab-completion-001",
        "dirty_files": {
            file_path_str.clone(): ai_content
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "ai_tab", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("AI tab edit").unwrap();

    let mut file = repo.filename("editor.ts");
    file.assert_lines_and_blame(crate::lines![
        "const editor = 'vscode';".human(),
        "const theme = 'dark';".ai(),
    ]);
}

// =============================================================================
// Multi-file edit (using Cursor as representative)
// =============================================================================

#[test]
fn test_cursor_multi_file_edit_e2e() {
    let repo = TestRepo::new();
    let src_dir = repo.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let file_a = src_dir.join("a.rs");
    let file_b = src_dir.join("b.rs");

    fs::write(&file_a, "fn a() {}\n").unwrap();
    fs::write(&file_b, "fn b() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/a.rs"])
        .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/b.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Pre-hook for file A
    let pre_hook_a = serde_json::json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-multi-001",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_a.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &pre_hook_a])
        .unwrap();

    fs::write(&file_a, "fn a() {}\nfn a_helper() {}\n").unwrap();

    // Post-hook for file A
    let post_hook_a = serde_json::json!({
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-multi-001",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_a.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &post_hook_a])
        .unwrap();

    // Pre-hook for file B
    let pre_hook_b = serde_json::json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-multi-001",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_b.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &pre_hook_b])
        .unwrap();

    fs::write(&file_b, "fn b() {}\nfn b_helper() {}\n").unwrap();

    // Post-hook for file B
    let post_hook_b = serde_json::json!({
        "hook_event_name": "postToolUse",
        "tool_name": "Write",
        "model": "claude-3-5-sonnet",
        "conversation_id": "conv-multi-001",
        "workspace_roots": [repo.canonical_path().to_string_lossy().to_string()],
        "transcript_path": "/tmp/fake-transcript.jsonl",
        "tool_input": {"file_path": file_b.to_string_lossy().to_string()}
    })
    .to_string();
    repo.git_ai(&["checkpoint", "cursor", "--hook-input", &post_hook_b])
        .unwrap();

    repo.stage_all_and_commit("Multi-file cursor edit").unwrap();

    let mut fa = repo.filename("src/a.rs");
    fa.assert_lines_and_blame(crate::lines!["fn a() {}".human(), "fn a_helper() {}".ai(),]);

    let mut fb = repo.filename("src/b.rs");
    fb.assert_lines_and_blame(crate::lines!["fn b() {}".human(), "fn b_helper() {}".ai(),]);
}

// =============================================================================
// Multi-file edit (using Windsurf)
// =============================================================================

#[test]
fn test_windsurf_multi_file_edit_e2e() {
    let repo = TestRepo::new();
    let file_a = repo.path().join("routes.ts");
    let file_b = repo.path().join("middleware.ts");

    fs::write(&file_a, "export const routes = [];\n").unwrap();
    fs::write(&file_b, "export const middleware = [];\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Edit file A and post checkpoint
    fs::write(
        &file_a,
        "export const routes = [];\nexport const apiRoutes = [];\n",
    )
    .unwrap();

    let post_a = serde_json::json!({
        "trajectory_id": "traj-multi-001",
        "agent_action_name": "post_write_code",
        "model_name": "Claude Sonnet 4",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_info": {
            "file_path": file_a.to_string_lossy().to_string(),
            "transcript_path": "/tmp/fake-windsurf-transcript.jsonl"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &post_a])
        .unwrap();

    // Edit file B and post checkpoint
    fs::write(
        &file_b,
        "export const middleware = [];\nexport const authMiddleware = () => {};\n",
    )
    .unwrap();

    let post_b = serde_json::json!({
        "trajectory_id": "traj-multi-001",
        "agent_action_name": "post_write_code",
        "model_name": "Claude Sonnet 4",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "tool_info": {
            "file_path": file_b.to_string_lossy().to_string(),
            "transcript_path": "/tmp/fake-windsurf-transcript.jsonl"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &post_b])
        .unwrap();

    repo.stage_all_and_commit("Multi-file windsurf edit")
        .unwrap();

    let mut fa = repo.filename("routes.ts");
    fa.assert_lines_and_blame(crate::lines![
        "export const routes = [];".human(),
        "export const apiRoutes = [];".ai(),
    ]);

    let mut fb = repo.filename("middleware.ts");
    fb.assert_lines_and_blame(crate::lines![
        "export const middleware = [];".human(),
        "export const authMiddleware = () => {};".ai(),
    ]);
}

// =============================================================================
// Human lines preserved after AI edit (using Claude as representative)
// =============================================================================

#[test]
fn test_claude_human_lines_preserved_after_ai_edit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("mixed.rs");

    // Initial file with multiple human lines
    fs::write(&file_path, "fn first() {}\nfn second() {}\nfn third() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI inserts a line between second and third
    let pre_hook = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-mixed-001",
        "tool_name": "Edit",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "cwd": repo.canonical_path().to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(
        &file_path,
        "fn first() {}\nfn second() {}\nfn ai_inserted() {}\nfn third() {}\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-mixed-001",
        "tool_name": "Edit",
        "tool_input": {"file_path": file_path.to_string_lossy().to_string()},
        "cwd": repo.canonical_path().to_string_lossy().to_string()
    })
    .to_string();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("AI inserts between human lines")
        .unwrap();

    let mut file = repo.filename("mixed.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn first() {}".human(),
        "fn second() {}".human(),
        "fn ai_inserted() {}".ai(),
        "fn third() {}".human(),
    ]);
}

// =============================================================================
// Human lines preserved (using GitHub Copilot)
// =============================================================================

#[test]
fn test_github_copilot_human_lines_preserved() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("config.json");

    fs::write(&file_path, "{\n  \"name\": \"app\"\n}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "config.json"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let file_path_str = file_path.to_string_lossy().to_string();

    let pre_hook = serde_json::json!({
        "hook_event_name": "before_edit",
        "workspaceFolder": repo.canonical_path().to_string_lossy().to_string(),
        "will_edit_filepaths": [file_path_str.clone()],
        "dirty_files": {
            file_path_str.clone(): "{\n  \"name\": \"app\"\n}\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "github-copilot", "--hook-input", &pre_hook])
        .unwrap();

    // AI adds a field but preserves existing human content
    fs::write(
        &file_path,
        "{\n  \"name\": \"app\",\n  \"version\": \"2.0\"\n}\n",
    )
    .unwrap();

    let post_hook = serde_json::json!({
        "hook_event_name": "after_edit",
        "workspaceFolder": repo.canonical_path().to_string_lossy().to_string(),
        "sessionId": "copilot-preserve-001",
        "edited_filepaths": [file_path_str.clone()],
        "dirty_files": {
            file_path_str.clone(): "{\n  \"name\": \"app\",\n  \"version\": \"2.0\"\n}\n"
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "github-copilot", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Copilot adds version field")
        .unwrap();

    let mut file = repo.filename("config.json");
    file.assert_lines_and_blame(crate::lines![
        "{".human(),
        "  \"name\": \"app\",".ai(),
        "  \"version\": \"2.0\"".ai(),
        "}".human(),
    ]);
}

// =============================================================================
// Codex camelCase variant
// =============================================================================

#[test]
fn test_codex_camel_case_hook_event_names_e2e() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("camel.py");

    fs::write(&file_path, "x = 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "camel.py"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Codex supports camelCase hook_event_name variants
    let pre_hook = serde_json::json!({
        "hookEventName": "PreToolUse",
        "sessionId": "codex-camel-001",
        "toolName": "apply_patch",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "toolInput": {"file_path": file_path.to_string_lossy().to_string()},
        "transcriptPath": "/tmp/fake-codex-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "x = 1\ny = 2\n").unwrap();

    let post_hook = serde_json::json!({
        "hookEventName": "PostToolUse",
        "sessionId": "codex-camel-001",
        "toolName": "apply_patch",
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "toolInput": {"file_path": file_path.to_string_lossy().to_string()},
        "transcriptPath": "/tmp/fake-codex-transcript.jsonl"
    })
    .to_string();
    repo.git_ai(&["checkpoint", "codex", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Codex camelCase edit").unwrap();

    let mut file = repo.filename("camel.py");
    file.assert_lines_and_blame(crate::lines!["x = 1".human(), "y = 2".ai(),]);
}

// =============================================================================
// Windsurf run_command (bash) hook variant
// =============================================================================

#[test]
fn test_windsurf_run_command_e2e() {
    let repo = TestRepo::new();
    let repo_root = repo.canonical_path();
    let file_path = repo_root.join("output.txt");

    fs::write(&file_path, "line one\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let pre_hook = serde_json::json!({
        "trajectory_id": "traj-bash-001",
        "execution_id": "exec-bash-001",
        "agent_action_name": "pre_run_command",
        "model_name": "Claude Sonnet 4",
        "tool_info": {
            "command_line": "echo 'appended' >> output.txt",
            "cwd": repo_root.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &pre_hook])
        .unwrap();

    fs::write(&file_path, "line one\nappended\n").unwrap();

    let post_hook = serde_json::json!({
        "trajectory_id": "traj-bash-001",
        "execution_id": "exec-bash-001",
        "agent_action_name": "post_run_command",
        "model_name": "Claude Sonnet 4",
        "tool_info": {
            "command_line": "echo 'appended' >> output.txt",
            "cwd": repo_root.to_string_lossy().to_string()
        }
    })
    .to_string();
    repo.git_ai(&["checkpoint", "windsurf", "--hook-input", &post_hook])
        .unwrap();

    repo.stage_all_and_commit("Windsurf bash edit").unwrap();

    let mut file = repo.filename("output.txt");
    file.assert_lines_and_blame(crate::lines!["line one".human(), "appended".ai(),]);
}
