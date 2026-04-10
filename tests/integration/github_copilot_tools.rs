use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use serde_json::json;

// Helper to create a realistic Copilot transcript path matching actual VS Code format
fn fake_copilot_transcript_path(repo: &TestRepo) -> String {
    repo.path()
        .join("Library/Application Support/Code/User/workspaceStorage/3a1e37d25f1dc63984c2bcc9a52a6bdd/GitHub.copilot-chat/transcripts/session-test-uuid.jsonl")
        .to_str()
        .unwrap()
        .to_string()
}

/// Test replace_string_in_file with realistic hook data
/// This is a normal file edit tool, not a bash tool
#[test]
fn test_replace_string_in_file_basic() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O (not helpers that trigger checkpoints)
    let file_path = repo.path().join("foo.py");
    std::fs::write(&file_path, "# Human comment\n").unwrap();

    // Commit with direct git commands
    repo.git(&["add", "foo.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let session_id = "0ae773c0-f1c2-4904-bd18-fb1046ff61cd";

    // PreToolUse hook
    let pre_hook_input = json!({
        "timestamp": "2026-04-07T18:10:41.626Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "replace_string_in_file",
        "tool_input": {
            "filePath": file_path.to_str().unwrap(),
            "oldString": "# Human comment",
            "newString": "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()"
        },
        "tool_use_id": "toolu_bdrk_013o2nzaLHN3dzQimNj9PaNg__vscode-1775585312869",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // AI makes the edit with raw I/O
    std::fs::write(&file_path, "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()\n").unwrap();

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-07T18:10:41.816Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "replace_string_in_file",
        "tool_input": {
            "filePath": file_path.to_str().unwrap(),
            "oldString": "# Human comment",
            "newString": "# Human comment\nimport argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Hello World CLI\")\n    parser.parse_args()\n    print(\"Hello, World!\")\n\nif __name__ == \"__main__\":\n    main()"
        },
        "tool_response": "",
        "tool_use_id": "toolu_bdrk_013o2nzaLHN3dzQimNj9PaNg__vscode-1775585312869",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // Sync daemon before assertions
    repo.sync_daemon();

    // Commit with direct git commands
    repo.git(&["add", "foo.py"]).unwrap();
    repo.git(&["commit", "-m", "Add CLI functionality"])
        .unwrap();

    // Sync daemon again after commit to ensure notes are written
    repo.sync_daemon();

    // AI-added lines should be attributed to AI
    let mut file = repo.filename("foo.py");
    file.assert_lines_and_blame(crate::lines![
        "# Human comment".human(),
        "import argparse".ai(),
        "".ai(),
        "def main():".ai(),
        "    parser = argparse.ArgumentParser(description=\"Hello World CLI\")".ai(),
        "    parser.parse_args()".ai(),
        "    print(\"Hello, World!\")".ai(),
        "".ai(),
        "if __name__ == \"__main__\":".ai(),
        "    main()".ai()
    ]);
}

/// Test run_in_terminal with realistic hook data
/// This tool should use bash checkpoint flow (snapshot diff)
#[test]
fn test_run_in_terminal_bash_checkpoint() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O — do NOT use set_contents/filename helpers
    // as they fire real checkpoints that corrupt the bash snapshot state.
    std::fs::write(
        repo.path().join("example.py"),
        "import argparse\n\ndef main():\n    parser = argparse.ArgumentParser(description=\"Test CLI\")\n    parser.add_argument(\"--name\", default=\"World\")\n    args = parser.parse_args()\n    print(f\"Hello, {args.name}!\")\n\nif __name__ == \"__main__\":\n    main()\n",
    )
    .unwrap();
    repo.git(&["add", "example.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial script"]).unwrap();

    // Wait for the daemon's watermark grace window (2s) to expire so the
    // pre-snapshot is not filtered to empty.
    std::thread::sleep(std::time::Duration::from_secs(3));

    let session_id = "b4a517c6-b9f0-4787-af3a-7c002539b448";

    // PreToolUse hook for run_in_terminal
    let pre_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.227Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 example.py",
            "explanation": "Run the CLI script to validate behavior.",
            "goal": "Validate behavior",
            "isBackground": false
        },
        "tool_use_id": "call_k6q1U6W9xW4fWjmJwsSI1IJP__vscode-1775710200829",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // Simulate the bash command writing a file directly to disk — raw I/O only,
    // no set_contents/filename helpers between Pre and PostToolUse.
    std::fs::write(repo.path().join("output.txt"), "Hello, World!").unwrap();

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-09T04:50:44.542Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 example.py",
            "explanation": "Run the CLI script to validate behavior.",
            "goal": "Validate behavior",
            "isBackground": false
        },
        "tool_response": "Hello, World!\n",
        "tool_use_id": "call_k6q1U6W9xW4fWjmJwsSI1IJP__vscode-1775710200829",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ])
    .unwrap();

    // Sync daemon before assertions
    repo.sync_daemon();

    repo.git(&["add", "output.txt"]).unwrap();
    repo.git(&["commit", "-m", "Add output file from command"])
        .unwrap();

    repo.sync_daemon();

    // File created by bash command should be attributed to AI
    let mut output = repo.filename("output.txt");
    output.assert_lines_and_blame(crate::lines!["Hello, World!".ai()]);
}

/// Test run_in_terminal with no file changes (no checkpoint created)
#[test]
fn test_run_in_terminal_no_changes() {
    let repo = TestRepo::new();

    // Create initial file with raw I/O
    std::fs::write(repo.path().join("test.py"), "print('test')\n").unwrap();
    repo.git(&["add", "test.py"]).unwrap();
    repo.git(&["commit", "-m", "Initial commit"]).unwrap();

    let session_id = "c3f5a7b8-9d0e-1f2a-3b4c-5d6e7f8a9b0c";

    // PreToolUse hook
    let pre_hook_input = json!({
        "timestamp": "2026-04-09T05:00:00.000Z",
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 test.py",
            "explanation": "Run test",
            "goal": "Validate",
            "isBackground": false
        },
        "tool_use_id": "call_testNoChanges__vscode-1775710200900",
        "cwd": repo.path().to_str().unwrap()
    });

    repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &pre_hook_input.to_string(),
    ])
    .unwrap();

    // Command runs but doesn't modify any files

    // PostToolUse hook
    let post_hook_input = json!({
        "timestamp": "2026-04-09T05:00:00.200Z",
        "hook_event_name": "PostToolUse",
        "session_id": session_id,
        "transcript_path": fake_copilot_transcript_path(&repo),
        "tool_name": "run_in_terminal",
        "tool_input": {
            "command": "python3 test.py",
            "explanation": "Run test",
            "goal": "Validate",
            "isBackground": false
        },
        "tool_response": "test\n",
        "tool_use_id": "call_testNoChanges__vscode-1775710200900",
        "cwd": repo.path().to_str().unwrap()
    });

    // This should succeed but not create a checkpoint (no file changes)
    let result = repo.git_ai(&[
        "checkpoint",
        "github-copilot",
        "--hook-input",
        &post_hook_input.to_string(),
    ]);

    // Should either succeed with no checkpoint or fail with "No editable file paths" error
    match result {
        Ok(_) => {
            // No checkpoint created, which is fine
        }
        Err(msg) => {
            assert!(
                msg.contains("No editable file paths") || msg.contains("Skipping checkpoint"),
                "Unexpected error: {}",
                msg
            );
        }
    }
}
