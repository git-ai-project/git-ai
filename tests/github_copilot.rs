mod test_utils;

use git_ai::authorship::transcript::Message;
use git_ai::commands::checkpoint_agent::agent_presets::GithubCopilotPreset;
use serde_json::json;
use std::io::Write;
use test_utils::fixture_path;

#[test]
fn copilot_session_parsing_stub() {
    // Minimal valid shape with empty requests
    let sample = r#"{"requests": []}"#;

    // Write to temporary file
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(temp_path);
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);
}

#[test]
fn copilot_session_parsing_simple() {
    // Load the test fixture path
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(fixture_str);
    assert!(result.is_ok());
    let (tx, model, _edited_filepaths) = result.unwrap();

    // Build expected transcript messages exactly
    let expected_messages = vec![
        Message::User {
            text: "What can you help me with?".to_string(),
            timestamp: Some("2025-10-07T13:51:13.835+00:00".to_string()),
        },
        Message::Assistant {
            text: "I'm GitHub Copilot, and I can help you with a wide range of programming and development tasks! Based on your current workspace, I can see you have a TypeScript project with an @/Users/svarlamov/projects/testing-git/index.ts file that prints \"Bonjour\" and numbers 1-10.\n\nHere are some ways I can assist you:\n\n## Code Development\n- **Write, edit, and refactor code** in various languages (TypeScript, JavaScript, Python, Java, C#, Go, Rust, and many more)\n- **Debug and fix issues** in your existing code\n- **Add new features** or modify existing functionality\n- **Optimize performance** and improve code quality\n\n## Project Management\n- **Set up new projects** and workspaces from scratch\n- **Configure build systems** and development environments\n- **Manage dependencies** and package installations\n- **Create and modify configuration files**\n\n## Git and Version Control\n- **Commit changes** and manage your Git workflow\n- **Create branches** and handle merges\n- **View git history** and resolve conflicts\n- **Work with pull requests** and issues\n\n## Development Tools\n- **Run terminal commands** and scripts\n- **Install and configure VS Code extensions**\n- **Set up debugging configurations**\n- **Work with databases** (PostgreSQL support available)\n\n## Documentation and Testing\n- **Write documentation** and README files\n- **Create and run tests**\n- **Generate code comments** and API documentation\n\nFor your current TypeScript project, I could help you:\n- Add error handling and input validation\n- Convert it to a proper CLI tool with argument parsing\n- Add TypeScript configurations and build scripts\n- Set up testing frameworks\n- Add more sophisticated functionality\n\nWhat specific task would you like help with today?".to_string(),
            timestamp: Some("2025-10-07T13:51:26.196+00:00".to_string()),
        },
        Message::User {
            text: "Change Bonjour World to hello world".to_string(),
            timestamp: Some("2025-10-07T13:51:41.282+00:00".to_string()),
        },
        Message::ToolUse {
            name: "prepareToolInvocation".to_string(),
            input: json!({
                "kind": "prepareToolInvocation",
                "toolName": "copilot_replaceString"
            }),
            timestamp: None,
        },
        Message::ToolUse {
            name: "copilot_replaceString".to_string(),
            input: serde_json::Value::String("Using \"Replace String in File\"".to_string()),
            timestamp: None,
        },
        Message::ToolUse {
            name: "textEditGroup".to_string(),
            input: json!({
                "done": true,
                "edits": [[{
                    "range": {
                        "endColumn": 30,
                        "endLineNumber": 10,
                        "startColumn": 1,
                        "startLineNumber": 5
                    },
                    "text": "// Print numbers 1 through 10\nfor (let i = 1; i <= 10; i++) {\n\tconsole.log(i);\n}\n\nconsole.log('hello world');"
                }], []],
                "kind": "textEditGroup",
                "uri": {
                    "$mid": 1,
                    "external": "file:///Users/svarlamov/projects/testing-git/index.ts",
                    "fsPath": "/Users/svarlamov/projects/testing-git/index.ts",
                    "path": "/Users/svarlamov/projects/testing-git/index.ts",
                    "scheme": "file"
                }
            }),
            timestamp: None,
        },
        Message::Assistant {
            text: "I'll change \"Bonjour World\" to \"hello world\" in your selected text. ``` @/Users/svarlamov/projects/testing-git/index.ts ``` Done! I've changed \"Bonjour World\" to \"hello world\" in line 10 of your @/Users/svarlamov/projects/testing-git/index.ts file.".to_string(),
            timestamp: Some("2025-10-07T13:51:50.791+00:00".to_string()),
        },
        Message::User {
            text: "Search for any other mentions of Bonjour".to_string(),
            timestamp: Some("2025-10-07T15:15:50.757+00:00".to_string()),
        },
        Message::ToolUse {
            name: "prepareToolInvocation".to_string(),
            input: json!({
                "kind": "prepareToolInvocation",
                "toolName": "copilot_findTextInFiles"
            }),
            timestamp: None,
        },
        Message::ToolUse {
            name: "copilot_findTextInFiles".to_string(),
            input: serde_json::Value::String("Searching text for `Bonjour`".to_string()),
            timestamp: None,
        },
        Message::Assistant {
            text: "I'll search for any other mentions of \"Bonjour\" in your workspace. I found one more mention of \"Bonjour\" in your code! It's on line 1 of @/Users/svarlamov/projects/testing-git/index.ts :\n\n```typescript\nconsole.log('Bonjour');\n```\n\nWould you like me to change this one to \"hello\" as well to make it consistent?".to_string(),
            timestamp: Some("2025-10-07T15:15:59.544+00:00".to_string()),
        },
    ];

    // Validate transcript messages exactly
    assert_eq!(tx.messages, expected_messages);

    // Validate model exactly
    assert_eq!(model, Some("copilot/claude-sonnet-4".to_string()));
}

#[test]
fn test_copilot_extracts_edited_filepaths() {
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(fixture_str);
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    // Verify edited_filepaths is extracted from textEditGroup
    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/Users/svarlamov/projects/testing-git/index.ts");
}

#[test]
fn test_copilot_no_edited_filepaths_when_no_edits() {
    let sample = r##"{
        "requests": [
            {
                "timestamp": 1728308673835,
                "message": {
                    "text": "What can you help me with?"
                },
                "response": [
                    {
                        "kind": "markdown",
                        "value": "I can help with code!"
                    }
                ],
                "modelId": "copilot/claude-sonnet-4"
            }
        ]
    }"##;

    // Write to temporary file
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(temp_path);
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    // Verify edited_filepaths is empty when there are no textEditGroup entries
    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 0);
}

#[test]
fn test_copilot_deduplicates_edited_filepaths() {
    let sample = r##"{
        "requests": [
            {
                "timestamp": 1728308673835,
                "message": {
                    "text": "Edit the file"
                },
                "response": [
                    {
                        "kind": "textEditGroup",
                        "uri": {
                            "fsPath": "/Users/test/file.ts"
                        }
                    },
                    {
                        "kind": "textEditGroup",
                        "uri": {
                            "fsPath": "/Users/test/file.ts"
                        }
                    },
                    {
                        "kind": "textEditGroup",
                        "uri": {
                            "fsPath": "/Users/test/other.ts"
                        }
                    }
                ],
                "modelId": "copilot/claude-sonnet-4"
            }
        ]
    }"##;

    // Write to temporary file
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    temp_file.write_all(sample.as_bytes()).unwrap();
    let temp_path = temp_file.path().to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(temp_path);
    assert!(result.is_ok());
    let (_tx, _model, edited_filepaths) = result.unwrap();

    // Verify duplicate paths are removed
    assert!(edited_filepaths.is_some());
    let paths = edited_filepaths.unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&"/Users/test/file.ts".to_string()));
    assert!(paths.contains(&"/Users/test/other.ts".to_string()));
}

#[test]
#[serial_test::serial] // Run serially to avoid env var conflicts with other tests
fn test_copilot_returns_empty_transcript_in_codespaces() {
    // Save original values if present
    let original_codespaces = std::env::var("CODESPACES").ok();

    // Set CODESPACES to true
    unsafe {
        std::env::set_var("CODESPACES", "true");
    }

    // Load the test fixture path
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(fixture_str);
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();

    // Should return empty transcript when running in Codespaces
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);

    // Restore original value or remove if it wasn't set
    unsafe {
        if let Some(original) = original_codespaces {
            std::env::set_var("CODESPACES", original);
        } else {
            std::env::remove_var("CODESPACES");
        }
    }
}

#[test]
#[serial_test::serial] // Run serially to avoid env var conflicts with other tests
fn test_copilot_returns_empty_transcript_in_remote_containers() {
    // Save original values if present
    let original_remote_containers = std::env::var("REMOTE_CONTAINERS").ok();

    // Set REMOTE_CONTAINERS to true
    unsafe {
        std::env::set_var("REMOTE_CONTAINERS", "true");
    }

    // Load the test fixture path
    let fixture = fixture_path("copilot_session_simple.json");
    let fixture_str = fixture.to_str().unwrap();

    let result = GithubCopilotPreset::transcript_and_model_from_copilot_session_json(fixture_str);
    assert!(result.is_ok());
    let (tx, model, edited_filepaths) = result.unwrap();

    // Should return empty transcript when running in Remote Containers
    assert!(tx.messages.is_empty());
    assert!(model.is_none());
    assert!(edited_filepaths.is_some());
    assert_eq!(edited_filepaths.unwrap().len(), 0);

    // Restore original value or remove if it wasn't set
    unsafe {
        if let Some(original) = original_remote_containers {
            std::env::set_var("REMOTE_CONTAINERS", original);
        } else {
            std::env::remove_var("REMOTE_CONTAINERS");
        }
    }
}
