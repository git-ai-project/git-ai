use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Minimal MCP server over stdio for Zed integration.
///
/// Implements just enough of the MCP protocol (JSON-RPC 2.0 over stdio) to expose
/// a `git_ai_checkpoint` tool that Zed's agent can call before/after file edits.
///
/// Message framing uses Content-Length headers (same as LSP):
///   Content-Length: <byte-count>\r\n
///   \r\n
///   <JSON payload>

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

/// Read one MCP message from stdin using Content-Length framing.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    // Read headers until empty line
    let mut content_length: Option<usize> = None;
    loop {
        let mut header_line = String::new();
        let bytes_read = reader.read_line(&mut header_line)?;
        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }

        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            if let Ok(len) = value.trim().parse::<usize>() {
                content_length = Some(len);
            }
        }
    }

    let length = match content_length {
        Some(l) => l,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing Content-Length header",
            ));
        }
    };

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body).map(Some).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid UTF-8 in message body: {}", e),
        )
    })
}

/// Write one MCP message to stdout with Content-Length framing.
fn write_message(writer: &mut impl Write, json: &str) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n{}", json.len(), json)?;
    writer.flush()
}

pub fn run_mcp_server() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();

    // Generate a stable session ID for this MCP server instance
    let session_id = format!(
        "zed-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    loop {
        let body = match read_message(&mut reader) {
            Ok(Some(b)) => b,
            Ok(None) => break, // EOF
            Err(e) => {
                eprintln!("[git-ai mcp] Failed to read message: {}", e);
                continue;
            }
        };

        let request: JsonRpcRequest = match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[git-ai mcp] Failed to parse JSON-RPC: {}", e);
                continue;
            }
        };

        let response = handle_request(&request, &session_id);

        if let Some(resp) = response {
            match serde_json::to_string(&resp) {
                Ok(json) => {
                    let mut out = stdout.lock();
                    if let Err(e) = write_message(&mut out, &json) {
                        eprintln!("[git-ai mcp] Failed to write response: {}", e);
                    }
                }
                Err(e) => {
                    eprintln!("[git-ai mcp] Failed to serialize response: {}", e);
                }
            }
        }
    }
}

fn handle_request(request: &JsonRpcRequest, session_id: &str) -> Option<JsonRpcResponse> {
    let id = request.id.clone()?;

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(id, request.params.as_ref(), session_id),
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
                                "description": "File paths being edited (recommended but optional)"
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

fn handle_tools_call(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    session_id: &str,
) -> JsonRpcResponse {
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

    let result = run_checkpoint(&input, session_id);

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

fn run_checkpoint(input: &CheckpointToolInput, session_id: &str) -> Result<String, String> {
    // Validate event field
    if input.event != "PreToolUse" && input.event != "PostToolUse" {
        return Err(format!(
            "Invalid event '{}': must be 'PreToolUse' or 'PostToolUse'",
            input.event
        ));
    }

    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to get current directory: {}", e))?
        .to_string_lossy()
        .to_string();

    let hook_input = serde_json::json!({
        "hook_event_name": input.event,
        "session_id": session_id,
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

    let mut child = Command::new(&binary_path)
        .args(["checkpoint", "zed", "--hook-input", "stdin"])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn checkpoint: {}", e))?;

    // Follow pipe deadlock prevention: start stdout/stderr readers BEFORE writing stdin
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_string(&mut buf);
        }
        buf
    });

    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut err) = stderr {
            let _ = err.read_to_string(&mut buf);
        }
        buf
    });

    // Write stdin asynchronously after readers are started
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(hook_input_str.as_bytes());
        // stdin dropped here, signaling EOF
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for checkpoint: {}", e))?;

    let stderr_output = stderr_handle.join().unwrap_or_default();

    // stdout_handle must be joined to avoid leak
    let _ = stdout_handle.join();

    if !status.success() {
        return Err(format!("Checkpoint failed: {}", stderr_output.trim()));
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
