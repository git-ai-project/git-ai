use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::process::Command;

/// Minimal MCP server over stdio for Zed integration.
///
/// Implements just enough of the MCP protocol (JSON-RPC 2.0 over stdio) to expose
/// a `git_ai_checkpoint` tool that Zed's agent can call before/after file edits.
///
/// Message format: each JSON-RPC message is a single line on stdin/stdout.

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

/// Tool input for git_ai_checkpoint
#[derive(Debug, Deserialize)]
struct CheckpointToolInput {
    /// "PreToolUse" or "PostToolUse"
    event: String,
    /// File paths being edited
    #[serde(default)]
    file_paths: Vec<String>,
}

pub fn run_mcp_server() {
    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[git-ai mcp] Failed to parse JSON-RPC: {}", e);
                continue;
            }
        };

        let response = handle_request(&request);

        if let Some(resp) = response {
            let mut out = stdout.lock();
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = writeln!(out, "{}", json);
                let _ = out.flush();
            }
        }
    }
}

fn handle_request(request: &JsonRpcRequest) -> Option<JsonRpcResponse> {
    let id = request.id.clone()?;

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(id, request.params.as_ref()),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::json!({})),
            error: None,
        },
        _ => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
            }),
        },
    };

    Some(response)
}

fn handle_initialize(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "git-ai",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        error: None,
    }
}

fn handle_tools_list(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(serde_json::json!({
            "tools": [
                {
                    "name": "git_ai_checkpoint",
                    "description": "Track AI code authorship. Call with event='PreToolUse' BEFORE editing files and event='PostToolUse' AFTER editing files. This lets git-ai attribute code changes to AI vs human authors.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "event": {
                                "type": "string",
                                "enum": ["PreToolUse", "PostToolUse"],
                                "description": "PreToolUse = about to edit files (human checkpoint), PostToolUse = just edited files (AI checkpoint)"
                            },
                            "file_paths": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "File paths being edited"
                            }
                        },
                        "required": ["event"]
                    }
                }
            ]
        })),
        error: None,
    }
}

fn handle_tools_call(id: serde_json::Value, params: Option<&serde_json::Value>) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Missing params".to_string(),
                }),
            };
        }
    };

    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if tool_name != "git_ai_checkpoint" {
        return JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Unknown tool: {}", tool_name)
                }],
                "isError": true
            })),
            error: None,
        };
    }

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let input: CheckpointToolInput = match serde_json::from_value(arguments) {
        Ok(i) => i,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Invalid arguments: {}", e)
                    }],
                    "isError": true
                })),
                error: None,
            };
        }
    };

    let result = run_checkpoint(&input);

    let (text, is_error) = match result {
        Ok(msg) => (msg, false),
        Err(e) => (format!("Checkpoint failed: {}", e), true),
    };

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(serde_json::json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": is_error
        })),
        error: None,
    }
}

fn run_checkpoint(input: &CheckpointToolInput) -> Result<String, String> {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let hook_input = serde_json::json!({
        "hook_event_name": input.event,
        "cwd": cwd,
        "tool_input": {
            "file_paths": input.file_paths,
        },
        "edited_filepaths": input.file_paths,
    });

    let hook_input_str =
        serde_json::to_string(&hook_input).map_err(|e| format!("JSON error: {}", e))?;

    // Shell out to `git-ai checkpoint zed --hook-input stdin`
    // This mirrors how OpenCode/Amp plugins work
    let binary_path =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?;

    let result = Command::new(&binary_path)
        .args(["checkpoint", "zed", "--hook-input", "stdin"])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write as _;
                let _ = stdin.write_all(hook_input_str.as_bytes());
            }
            // Drop stdin to signal EOF
            child.stdin.take();
            child.wait_with_output()
        })
        .map_err(|e| format!("Failed to run checkpoint: {}", e))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("Checkpoint failed: {}", stderr.trim()));
    }

    let event_type = if input.event == "PreToolUse" {
        "human"
    } else {
        "AI"
    };

    Ok(format!(
        "Created {} checkpoint for {} file(s)",
        event_type,
        input.file_paths.len()
    ))
}
