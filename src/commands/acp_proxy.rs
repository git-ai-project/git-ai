use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// A tracked in-flight edit tool call
#[derive(Debug, Clone)]
struct TrackedToolCall {
    file_paths: Vec<String>,
}

/// Shared proxy state
struct ProxyState {
    tracked_edits: HashMap<String, TrackedToolCall>,
    session_id: Option<String>,
    cwd: String,
    git_ai_binary: String,
}

/// Read a Content-Length framed JSON-RPC message from a reader.
/// Returns None on EOF.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;

    // Read headers
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
        // Content-Length header is case-insensitive per LSP/JSON-RPC spec
        let lower = trimmed.to_ascii_lowercase();
        if let Some(len_str) = lower.strip_prefix("content-length:")
            && let Ok(len) = len_str.trim().parse::<usize>()
        {
            content_length = Some(len);
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

/// Write a Content-Length framed message to a writer.
fn write_message(writer: &mut impl Write, body: &str) -> io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

/// Tool call kinds that represent file edits
const EDIT_KINDS: &[&str] = &["edit", "delete", "move", "write", "create"];

/// Check if a tool call kind represents a file-editing operation
fn is_edit_kind(kind: &str) -> bool {
    EDIT_KINDS.iter().any(|k| k.eq_ignore_ascii_case(kind))
}

/// Extract file paths from a tool_call or tool_call_update notification.
/// Paths can appear in `locations[].path` and `content[].path` (for Diff entries).
fn extract_file_paths(params: &serde_json::Value) -> Vec<String> {
    let mut paths = HashSet::new();

    // Extract from locations[].path
    if let Some(locations) = params.get("locations").and_then(|v| v.as_array()) {
        for loc in locations {
            if let Some(path) = loc.get("path").and_then(|v| v.as_str()) {
                paths.insert(path.to_string());
            }
        }
    }

    // Extract from content[].path (Diff entries)
    if let Some(content) = params.get("content").and_then(|v| v.as_array()) {
        for entry in content {
            if let Some(path) = entry.get("path").and_then(|v| v.as_str()) {
                paths.insert(path.to_string());
            }
        }
    }

    let mut result: Vec<String> = paths.into_iter().collect();
    result.sort();
    result
}

/// Spawn a background checkpoint process.
/// This never blocks the proxy — errors are logged to stderr.
fn spawn_checkpoint(
    git_ai_binary: &str,
    hook_event_name: &str,
    session_id: &str,
    cwd: &str,
    file_paths: &[String],
) {
    let binary = git_ai_binary.to_string();
    let event = hook_event_name.to_string();
    let sid = session_id.to_string();
    let wd = cwd.to_string();
    let fps = file_paths.to_vec();

    thread::spawn(move || {
        let payload = serde_json::json!({
            "hook_event_name": event,
            "session_id": sid,
            "cwd": wd,
            "file_paths": fps,
        });

        let payload_str = payload.to_string();

        let result = Command::new(&binary)
            .args(["checkpoint", "zed", "--hook-input", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .and_then(|mut child| {
                // Take stdin and drop it after writing so the child sees EOF
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(payload_str.as_bytes())?;
                }
                // stdin is now dropped, child will see EOF and can proceed
                child.wait()
            });

        if let Err(e) = result {
            eprintln!("[git-ai acp-proxy] checkpoint error: {}", e);
        }
    });
}

/// Inspect an agent→Zed message for tool_call / tool_call_update notifications.
/// Returns true if the message was inspected (doesn't affect forwarding).
fn inspect_agent_message(body: &str, state: &Arc<Mutex<ProxyState>>) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return, // Not valid JSON, skip
    };

    // JSON-RPC notification: {"jsonrpc":"2.0","method":"...","params":{...}}
    let method = match parsed.get("method").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return,
    };

    let params = match parsed.get("params") {
        Some(p) => p,
        None => return,
    };

    match method {
        "session/update" => {
            // session/update can carry tool_call or tool_call_update in its params
            // Extract session_id if present
            if let Some(sid) = params.get("sessionId").and_then(|v| v.as_str()) {
                let mut st = state.lock().unwrap();
                if st.session_id.is_none() {
                    st.session_id = Some(sid.to_string());
                }
            }

            if let Some(updates) = params.get("updates").and_then(|v| v.as_array()) {
                for update in updates {
                    let update_type = update.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match update_type {
                        "tool_call" => handle_tool_call(update, state),
                        "tool_call_update" => handle_tool_call_update(update, state),
                        _ => {}
                    }
                }
            }

            // Also check top-level tool_call / tool_call_update in params directly
            if params.get("toolCallId").is_some() {
                if params.get("status").is_some() {
                    handle_tool_call_update(params, state);
                } else if params.get("kind").is_some() {
                    handle_tool_call(params, state);
                }
            }
        }
        "tool_call" => handle_tool_call(params, state),
        "tool_call_update" => handle_tool_call_update(params, state),
        _ => {}
    }
}

/// Handle a tool_call notification — if kind is edit-like, track it and spawn a human checkpoint.
fn handle_tool_call(params: &serde_json::Value, state: &Arc<Mutex<ProxyState>>) {
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return,
    };

    if !is_edit_kind(kind) {
        return;
    }

    let tool_call_id = match params.get("toolCallId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };

    let file_paths = extract_file_paths(params);

    let (git_ai_binary, session_id, cwd) = {
        let mut st = state.lock().unwrap();

        st.tracked_edits.insert(
            tool_call_id.clone(),
            TrackedToolCall {
                file_paths: file_paths.clone(),
            },
        );

        (
            st.git_ai_binary.clone(),
            st.session_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            st.cwd.clone(),
        )
    };

    if !file_paths.is_empty() {
        spawn_checkpoint(&git_ai_binary, "PreToolUse", &session_id, &cwd, &file_paths);
    }
}

/// Handle a tool_call_update notification — if status is completed for a tracked edit,
/// spawn an AI checkpoint.
fn handle_tool_call_update(params: &serde_json::Value, state: &Arc<Mutex<ProxyState>>) {
    let tool_call_id = match params.get("toolCallId").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return,
    };

    let status = match params.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return,
    };

    // Merge any additional file paths from the update
    let update_paths = extract_file_paths(params);

    let (git_ai_binary, session_id, cwd, file_paths) = {
        let mut st = state.lock().unwrap();

        match status {
            "completed" => {
                let tracked = st.tracked_edits.remove(tool_call_id);
                let mut paths = tracked.map(|t| t.file_paths).unwrap_or_default();

                // Merge update paths
                for p in update_paths {
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }

                (
                    st.git_ai_binary.clone(),
                    st.session_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    st.cwd.clone(),
                    paths,
                )
            }
            "failed" | "cancelled" => {
                st.tracked_edits.remove(tool_call_id);
                return;
            }
            _ => return,
        }
    };

    if !file_paths.is_empty() {
        spawn_checkpoint(
            &git_ai_binary,
            "PostToolUse",
            &session_id,
            &cwd,
            &file_paths,
        );
    }
}

/// Parse CLI arguments for the ACP proxy command.
/// Usage: git-ai acp-proxy [--cwd <dir>] -- <agent-command> [agent-args...]
/// Returns (cwd, agent_command, agent_args)
fn parse_args(args: &[String]) -> Result<(String, String, Vec<String>), String> {
    let mut cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let mut agent_cmd: Option<String> = None;
    let mut agent_args: Vec<String> = Vec::new();
    let mut found_separator = false;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            found_separator = true;
            i += 1;
            // Everything after -- is the agent command
            if i < args.len() {
                agent_cmd = Some(args[i].clone());
                i += 1;
                while i < args.len() {
                    agent_args.push(args[i].clone());
                    i += 1;
                }
            }
            break;
        } else if args[i] == "--cwd" {
            if i + 1 < args.len() {
                cwd = args[i + 1].clone();
                i += 2;
            } else {
                return Err("--cwd requires a value".to_string());
            }
        } else {
            // If no -- separator, treat remaining args as agent command
            agent_cmd = Some(args[i].clone());
            i += 1;
            while i < args.len() {
                agent_args.push(args[i].clone());
                i += 1;
            }
            break;
        }
    }

    if !found_separator && agent_cmd.is_none() {
        return Err(
            "Usage: git-ai acp-proxy [--cwd <dir>] -- <agent-command> [agent-args...]".to_string(),
        );
    }

    match agent_cmd {
        Some(cmd) => Ok((cwd, cmd, agent_args)),
        None => Err("No agent command specified after --".to_string()),
    }
}

/// Entry point for the ACP proxy command.
pub fn handle_acp_proxy(args: &[String]) {
    let git_ai_binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "git-ai".to_string());

    let (cwd, agent_cmd, agent_args) = match parse_args(args) {
        Ok((cwd, cmd, aargs)) => (cwd, cmd, aargs),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Spawn the agent child process
    let mut child = match Command::new(&agent_cmd)
        .args(&agent_args)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to spawn agent process '{}': {}", agent_cmd, e);
            std::process::exit(1);
        }
    };

    let child_stdin = child.stdin.take().expect("Failed to capture child stdin");
    let child_stdout = child.stdout.take().expect("Failed to capture child stdout");

    let state = Arc::new(Mutex::new(ProxyState {
        tracked_edits: HashMap::new(),
        session_id: None,
        cwd,
        git_ai_binary,
    }));

    // Thread 1: Zed (our stdin) → Agent (child stdin)
    // Forward messages without inspection
    let zed_to_agent = thread::spawn(move || {
        let mut reader = BufReader::new(io::stdin().lock());
        let mut writer = child_stdin;

        loop {
            match read_message(&mut reader) {
                Ok(Some(body)) => {
                    if let Err(e) = write_message(&mut writer, &body) {
                        eprintln!("[git-ai acp-proxy] write to agent failed: {}", e);
                        break;
                    }
                }
                Ok(None) => break, // Zed closed stdin
                Err(e) => {
                    eprintln!("[git-ai acp-proxy] read from Zed failed: {}", e);
                    break;
                }
            }
        }
    });

    // Thread 2: Agent (child stdout) → Zed (our stdout)
    // Forward messages with inspection for tool calls
    let state_clone = Arc::clone(&state);
    let agent_to_zed = thread::spawn(move || {
        let mut reader = BufReader::new(child_stdout);
        let mut writer = io::stdout().lock();

        loop {
            match read_message(&mut reader) {
                Ok(Some(body)) => {
                    // Inspect for tool call notifications (never fails the forward)
                    inspect_agent_message(&body, &state_clone);

                    if let Err(e) = write_message(&mut writer, &body) {
                        eprintln!("[git-ai acp-proxy] write to Zed failed: {}", e);
                        break;
                    }
                }
                Ok(None) => break, // Agent closed stdout
                Err(e) => {
                    eprintln!("[git-ai acp-proxy] read from agent failed: {}", e);
                    break;
                }
            }
        }
    });

    // Wait for the child process to exit
    let exit_status = child.wait().unwrap_or_else(|e| {
        eprintln!("[git-ai acp-proxy] wait on child failed: {}", e);
        std::process::exit(1);
    });

    // Wait for forwarding threads
    let _ = zed_to_agent.join();
    let _ = agent_to_zed.join();

    // Exit with the same code as the agent
    std::process::exit(exit_status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_paths_from_locations() {
        let params = serde_json::json!({
            "locations": [
                {"path": "src/main.rs", "line": 10},
                {"path": "src/lib.rs", "line": 20}
            ]
        });
        let mut paths = extract_file_paths(&params);
        paths.sort();
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn test_extract_file_paths_from_content() {
        let params = serde_json::json!({
            "content": [
                {"type": "Diff", "path": "src/foo.rs", "diff": "..."},
                {"type": "Diff", "path": "src/bar.rs", "diff": "..."}
            ]
        });
        let mut paths = extract_file_paths(&params);
        paths.sort();
        assert_eq!(paths, vec!["src/bar.rs", "src/foo.rs"]);
    }

    #[test]
    fn test_extract_file_paths_deduplicates() {
        let params = serde_json::json!({
            "locations": [
                {"path": "src/main.rs"},
                {"path": "src/main.rs"}
            ],
            "content": [
                {"path": "src/main.rs"}
            ]
        });
        let paths = extract_file_paths(&params);
        assert_eq!(paths, vec!["src/main.rs"]);
    }

    #[test]
    fn test_extract_file_paths_empty() {
        let params = serde_json::json!({});
        let paths = extract_file_paths(&params);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_is_edit_kind() {
        assert!(is_edit_kind("edit"));
        assert!(is_edit_kind("Edit"));
        assert!(is_edit_kind("delete"));
        assert!(is_edit_kind("move"));
        assert!(is_edit_kind("write"));
        assert!(is_edit_kind("create"));
        assert!(!is_edit_kind("read"));
        assert!(!is_edit_kind("search"));
    }

    #[test]
    fn test_parse_args_with_separator() {
        let args: Vec<String> = vec!["--".into(), "claude".into()];
        let (_cwd, cmd, rest) = parse_args(&args).unwrap();
        assert_eq!(cmd, "claude");
        assert!(rest.is_empty());
    }

    #[test]
    fn test_parse_args_with_cwd_and_separator() {
        let args: Vec<String> = vec![
            "--cwd".into(),
            "/tmp/project".into(),
            "--".into(),
            "claude".into(),
            "--verbose".into(),
        ];
        let (cwd, cmd, rest) = parse_args(&args).unwrap();
        assert_eq!(cmd, "claude");
        assert_eq!(rest, vec!["--verbose"]);
        assert_eq!(cwd, "/tmp/project");
    }

    #[test]
    fn test_parse_args_without_separator() {
        let args: Vec<String> = vec!["claude".into(), "--verbose".into()];
        let (_cwd, cmd, rest) = parse_args(&args).unwrap();
        assert_eq!(cmd, "claude");
        assert_eq!(rest, vec!["--verbose"]);
    }

    #[test]
    fn test_parse_args_empty() {
        let args: Vec<String> = vec![];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn test_inspect_agent_message_tool_call() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            session_id: Some("test-session".to_string()),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(), // won't actually run
        }));

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call",
            "params": {
                "toolCallId": "tc-1",
                "kind": "edit",
                "locations": [{"path": "src/main.rs"}]
            }
        });

        inspect_agent_message(&msg.to_string(), &state);

        let st = state.lock().unwrap();
        assert!(st.tracked_edits.contains_key("tc-1"));
        assert_eq!(st.tracked_edits["tc-1"].file_paths, vec!["src/main.rs"]);
    }

    #[test]
    fn test_inspect_agent_message_non_edit_kind_not_tracked() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            session_id: Some("test-session".to_string()),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call",
            "params": {
                "toolCallId": "tc-2",
                "kind": "read",
                "locations": [{"path": "src/main.rs"}]
            }
        });

        inspect_agent_message(&msg.to_string(), &state);

        let st = state.lock().unwrap();
        assert!(!st.tracked_edits.contains_key("tc-2"));
    }

    #[test]
    fn test_inspect_agent_message_tool_call_update_completed() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            session_id: Some("test-session".to_string()),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));

        // First, track a tool call
        {
            let mut st = state.lock().unwrap();
            st.tracked_edits.insert(
                "tc-3".to_string(),
                TrackedToolCall {
                    file_paths: vec!["src/main.rs".to_string()],
                },
            );
        }

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call_update",
            "params": {
                "toolCallId": "tc-3",
                "status": "completed"
            }
        });

        inspect_agent_message(&msg.to_string(), &state);

        let st = state.lock().unwrap();
        // Should be removed from tracked after completion
        assert!(!st.tracked_edits.contains_key("tc-3"));
    }

    #[test]
    fn test_inspect_agent_message_tool_call_update_failed() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            session_id: Some("test-session".to_string()),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));

        {
            let mut st = state.lock().unwrap();
            st.tracked_edits.insert(
                "tc-4".to_string(),
                TrackedToolCall {
                    file_paths: vec!["src/main.rs".to_string()],
                },
            );
        }

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call_update",
            "params": {
                "toolCallId": "tc-4",
                "status": "failed"
            }
        });

        inspect_agent_message(&msg.to_string(), &state);

        let st = state.lock().unwrap();
        assert!(!st.tracked_edits.contains_key("tc-4"));
    }

    #[test]
    fn test_inspect_agent_message_session_update_with_session_id() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            session_id: None,
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-123",
                "updates": []
            }
        });

        inspect_agent_message(&msg.to_string(), &state);

        let st = state.lock().unwrap();
        assert_eq!(st.session_id, Some("acp-123".to_string()));
    }

    #[test]
    fn test_read_write_message_roundtrip() {
        let body = r#"{"jsonrpc":"2.0","method":"test","params":{}}"#;

        let mut buffer = Vec::new();
        write_message(&mut buffer, body).unwrap();

        let mut reader = BufReader::new(&buffer[..]);
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, Some(body.to_string()));
    }

    #[test]
    fn test_read_message_eof() {
        let mut reader = BufReader::new(&b""[..]);
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, None);
    }
}
