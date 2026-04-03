use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// A tracked in-flight edit tool call
#[derive(Debug, Clone)]
struct TrackedToolCall {
    file_paths: Vec<String>,
    session_id: String,
}

/// Shared proxy state
struct ProxyState {
    tracked_edits: HashMap<String, TrackedToolCall>,
    cwd: String,
    git_ai_binary: String,
}

/// Tracks in-flight checkpoint threads so we can wait for them before exiting.
struct CheckpointTracker {
    count: Mutex<usize>,
    done: Condvar,
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
    tracker: &Arc<CheckpointTracker>,
) {
    let binary = git_ai_binary.to_string();
    let event = hook_event_name.to_string();
    let sid = session_id.to_string();
    let wd = cwd.to_string();
    let fps = file_paths.to_vec();
    let tracker = Arc::clone(tracker);

    // Increment before spawning so the count is visible immediately
    {
        let mut count = tracker.count.lock().unwrap();
        *count += 1;
    }

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

        // Decrement and notify regardless of success/failure
        let mut count = tracker.count.lock().unwrap();
        *count -= 1;
        if *count == 0 {
            tracker.done.notify_all();
        }
    });
}

/// Inspect an agent→Zed message for tool_call / tool_call_update notifications.
/// Returns true if the message was inspected (doesn't affect forwarding).
fn inspect_agent_message(
    body: &str,
    state: &Arc<Mutex<ProxyState>>,
    tracker: &Arc<CheckpointTracker>,
) {
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
            // Extract session_id from this notification — passed to handlers so
            // each tool call is attributed to the correct session.
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // ACP protocol: params.update is a single SessionUpdate object
            // discriminated by the "sessionUpdate" field
            if let Some(update) = params.get("update") {
                let update_type = update
                    .get("sessionUpdate")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match update_type {
                    "tool_call" => handle_tool_call(update, state, tracker, session_id),
                    "tool_call_update" => handle_tool_call_update(update, state, tracker),
                    _ => {}
                }
            }
        }
        "tool_call" => handle_tool_call(params, state, tracker, "unknown"),
        "tool_call_update" => handle_tool_call_update(params, state, tracker),
        _ => {}
    }
}

/// Handle a tool_call notification — if kind is edit-like, track it and spawn a human checkpoint.
fn handle_tool_call(
    params: &serde_json::Value,
    state: &Arc<Mutex<ProxyState>>,
    tracker: &Arc<CheckpointTracker>,
    session_id: &str,
) {
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

    let (git_ai_binary, cwd) = {
        let mut st = state.lock().unwrap();

        st.tracked_edits.insert(
            tool_call_id.clone(),
            TrackedToolCall {
                file_paths: file_paths.clone(),
                session_id: session_id.to_string(),
            },
        );

        (st.git_ai_binary.clone(), st.cwd.clone())
    };

    if !file_paths.is_empty() {
        spawn_checkpoint(
            &git_ai_binary,
            "PreToolUse",
            session_id,
            &cwd,
            &file_paths,
            tracker,
        );
    }
}

/// Handle a tool_call_update notification — if status is completed for a tracked edit,
/// spawn an AI checkpoint.
fn handle_tool_call_update(
    params: &serde_json::Value,
    state: &Arc<Mutex<ProxyState>>,
    tracker: &Arc<CheckpointTracker>,
) {
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
                let session_id = tracked
                    .as_ref()
                    .map(|t| t.session_id.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                let mut paths = tracked.map(|t| t.file_paths).unwrap_or_default();

                // Merge update paths
                for p in update_paths {
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }

                (st.git_ai_binary.clone(), session_id, st.cwd.clone(), paths)
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
            tracker,
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
        .env("GIT_AI_ACP_PROXY", "1")
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
        cwd,
        git_ai_binary,
    }));

    let tracker = Arc::new(CheckpointTracker {
        count: Mutex::new(0),
        done: Condvar::new(),
    });

    // Thread 1: Zed (our stdin) → Agent (child stdin)
    // Forward messages without inspection
    let _zed_to_agent = thread::spawn(move || {
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
    let tracker_clone = Arc::clone(&tracker);
    let agent_to_zed = thread::spawn(move || {
        let mut reader = BufReader::new(child_stdout);
        let mut writer = io::stdout().lock();

        loop {
            match read_message(&mut reader) {
                Ok(Some(body)) => {
                    // Inspect for tool call notifications (never fails the forward)
                    inspect_agent_message(&body, &state_clone, &tracker_clone);

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

    // Wait for the agent_to_zed forwarding thread (returns quickly since child
    // stdout is already closed) and then drain any in-flight checkpoint threads.
    let code = exit_status.code().unwrap_or(1);
    let _ = agent_to_zed.join();

    // Wait for in-flight checkpoint threads (max 30s) so the final PostToolUse
    // checkpoint is not lost when the agent exits immediately after its last tool call.
    {
        let mut count = tracker.count.lock().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while *count > 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                eprintln!(
                    "[git-ai acp-proxy] timed out waiting for {} checkpoint(s)",
                    *count
                );
                break;
            }
            let (guard, _) = tracker.done.wait_timeout(count, remaining).unwrap();
            count = guard;
        }
    }

    std::process::exit(code);
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
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(), // won't actually run
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call",
            "params": {
                "toolCallId": "tc-1",
                "kind": "edit",
                "locations": [{"path": "src/main.rs"}]
            }
        });

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        assert!(st.tracked_edits.contains_key("tc-1"));
        assert_eq!(st.tracked_edits["tc-1"].file_paths, vec!["src/main.rs"]);
    }

    #[test]
    fn test_inspect_agent_message_non_edit_kind_not_tracked() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool_call",
            "params": {
                "toolCallId": "tc-2",
                "kind": "read",
                "locations": [{"path": "src/main.rs"}]
            }
        });

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        assert!(!st.tracked_edits.contains_key("tc-2"));
    }

    #[test]
    fn test_inspect_agent_message_tool_call_update_completed() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        // First, track a tool call
        {
            let mut st = state.lock().unwrap();
            st.tracked_edits.insert(
                "tc-3".to_string(),
                TrackedToolCall {
                    file_paths: vec!["src/main.rs".to_string()],
                    session_id: "test-session".to_string(),
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

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        // Should be removed from tracked after completion
        assert!(!st.tracked_edits.contains_key("tc-3"));
    }

    #[test]
    fn test_inspect_agent_message_tool_call_update_failed() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        {
            let mut st = state.lock().unwrap();
            st.tracked_edits.insert(
                "tc-4".to_string(),
                TrackedToolCall {
                    file_paths: vec!["src/main.rs".to_string()],
                    session_id: "test-session".to_string(),
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

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        assert!(!st.tracked_edits.contains_key("tc-4"));
    }

    #[test]
    fn test_inspect_session_update_captures_session_id_per_tool_call() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        // Tool call from session "acp-A"
        let msg_a = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-A",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-from-a",
                    "kind": "edit",
                    "title": "Edit",
                    "locations": [{"path": "a.rs"}]
                }
            }
        });

        // Tool call from session "acp-B"
        let msg_b = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-B",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-from-b",
                    "kind": "edit",
                    "title": "Edit",
                    "locations": [{"path": "b.rs"}]
                }
            }
        });

        inspect_agent_message(&msg_a.to_string(), &state, &tracker);
        inspect_agent_message(&msg_b.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        // Each tool call should carry its own session's ID
        assert_eq!(st.tracked_edits["tc-from-a"].session_id, "acp-A");
        assert_eq!(st.tracked_edits["tc-from-b"].session_id, "acp-B");
    }

    #[test]
    fn test_inspect_session_update_tool_call() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "test-session",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-acp-1",
                    "kind": "edit",
                    "title": "Edit file",
                    "locations": [{"path": "src/lib.rs"}]
                }
            }
        });

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        assert!(st.tracked_edits.contains_key("tc-acp-1"));
        assert_eq!(st.tracked_edits["tc-acp-1"].file_paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn test_inspect_session_update_tool_call_update() {
        let state = Arc::new(Mutex::new(ProxyState {
            tracked_edits: HashMap::new(),
            cwd: "/tmp".to_string(),
            git_ai_binary: "/usr/bin/false".to_string(),
        }));
        let tracker = Arc::new(CheckpointTracker {
            count: Mutex::new(0),
            done: Condvar::new(),
        });

        // Pre-track a tool call
        {
            let mut st = state.lock().unwrap();
            st.tracked_edits.insert(
                "tc-acp-2".to_string(),
                TrackedToolCall {
                    file_paths: vec!["src/lib.rs".to_string()],
                    session_id: "test-session".to_string(),
                },
            );
        }

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "test-session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-acp-2",
                    "status": "completed"
                }
            }
        });

        inspect_agent_message(&msg.to_string(), &state, &tracker);

        let st = state.lock().unwrap();
        assert!(!st.tracked_edits.contains_key("tc-acp-2"));
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
