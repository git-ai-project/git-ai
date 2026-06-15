use crate::streams::sweep::StreamFormat;
use crate::streams::types::StreamError;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

pub fn extract_model(
    path: &Path,
    format: StreamFormat,
    session_id: Option<&str>,
) -> Result<Option<String>, StreamError> {
    match format {
        StreamFormat::ClaudeJsonl
        | StreamFormat::CopilotEventStreamJsonl
        | StreamFormat::GeminiJsonl => extract_model_from_jsonl_tail(path),
        StreamFormat::CopilotSessionJson => extract_model_from_copilot_session_json(path),
        StreamFormat::AmpThreadJson => extract_model_from_amp_thread_json(path),
        StreamFormat::OpenCodeSqlite => extract_model_from_opencode_sqlite(path, session_id),
        // Droid uses extract_model_from_droid_settings() with the settings path instead
        _ => Ok(None),
    }
}

pub fn extract_model_from_droid_settings(
    settings_path: &Path,
) -> Result<Option<String>, StreamError> {
    let content = match std::fs::read_to_string(settings_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(_) => return Ok(None),
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    Ok(json.get("model").and_then(|v| v.as_str()).map(String::from))
}

fn extract_model_from_jsonl_tail(path: &Path) -> Result<Option<String>, StreamError> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(_) => return Ok(None),
    };

    let file_size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Ok(None),
    };

    if file_size == 0 {
        return Ok(None);
    }

    let read_size = std::cmp::min(51200, file_size);
    let seek_pos = file_size - read_size;

    if file.seek(SeekFrom::Start(seek_pos)).is_err() {
        return Ok(None);
    }

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    for line in lines.iter().rev() {
        if let Some(model) = extract_model_from_jsonl_line(line) {
            return Ok(Some(model));
        }
    }

    // Tail didn't contain the model — check the head (Copilot CLI emits
    // session.model_change only at session start, which may fall outside the tail window).
    if seek_pos > 0
        && let Some(model) = extract_model_from_jsonl_head(path)
    {
        return Ok(Some(model));
    }

    Ok(None)
}

fn extract_model_from_jsonl_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    if json.get("type").and_then(|v| v.as_str()) == Some("session.model_change")
        && let Some(model) = json
            .get("data")
            .and_then(|d| d.get("newModel"))
            .and_then(|v| v.as_str())
    {
        return Some(model.to_string());
    }

    let candidate = json
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(|v| v.as_str())
        .or_else(|| json.get("model").and_then(|v| v.as_str()));

    if let Some(model) = candidate
        && model != "<synthetic>"
    {
        return Some(model.to_string());
    }

    None
}

fn extract_model_from_jsonl_head(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok).take(20) {
        if let Some(model) = extract_model_from_jsonl_line(&line) {
            return Some(model);
        }
    }
    None
}

/// Extracts the model from VS Code Copilot's `models.json` debug log.
/// Given a transcript path like `.../transcripts/{session_id}.jsonl`,
/// derives `.../debug-logs/{session_id}/models.json` and reads the default model.
pub fn extract_model_from_copilot_models_json(
    stream_path: &Path,
) -> Result<Option<String>, StreamError> {
    let session_id = stream_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Ok(None);
    }

    // transcript: .../transcripts/{session_id}.jsonl
    // models:     .../debug-logs/{session_id}/models.json
    let transcripts_dir = match stream_path.parent() {
        Some(p) => p,
        None => return Ok(None),
    };
    let copilot_chat_dir = match transcripts_dir.parent() {
        Some(p) => p,
        None => return Ok(None),
    };
    let models_path = copilot_chat_dir
        .join("debug-logs")
        .join(session_id)
        .join("models.json");

    let content = match std::fs::read_to_string(&models_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let models: Vec<serde_json::Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let model = models.iter().find_map(|m| {
        if m.get("is_chat_default").and_then(|v| v.as_bool()) == Some(true) {
            m.get("id").and_then(|v| v.as_str()).map(String::from)
        } else {
            None
        }
    });

    Ok(model)
}

/// Resolves the path to VS Code Copilot's OTEL traces SQLite DB (`agent-traces.db`)
/// from a transcript path.
///
/// Honors the `GIT_AI_COPILOT_OTEL_DB_PATH` override, otherwise navigates:
/// `.../workspaceStorage/{hash}/GitHub.copilot-chat/transcripts/{id}.jsonl`
/// → `.../User/globalStorage/github.copilot-chat/agent-traces.db`.
fn resolve_copilot_otel_db_path(stream_path: &Path) -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("GIT_AI_COPILOT_OTEL_DB_PATH") {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // transcript: .../workspaceStorage/{hash}/GitHub.copilot-chat/transcripts/{id}.jsonl
    let workspace_storage_root = stream_path
        .parent()? // transcripts/
        .parent()? // GitHub.copilot-chat/
        .parent()? // {hash}/
        .parent()?; // workspaceStorage/

    let user_dir = workspace_storage_root.parent()?; // User/
    let otel_db = user_dir
        .join("globalStorage")
        .join("github.copilot-chat")
        .join("agent-traces.db");

    if otel_db.exists() {
        Some(otel_db)
    } else {
        None
    }
}

/// Extracts the model from VS Code Copilot's OTEL traces DB (`agent-traces.db`).
///
/// VS Code does not record the model in its transcript JSONL, so the only
/// reliable per-session source of the user-selected model is the OTEL spans DB.
/// Given a transcript path like `.../transcripts/{session_id}.jsonl`, this
/// derives the OTEL DB path and reads the most recent span for that session,
/// preferring the requested model (the user-facing identifier) and falling back
/// to the resolved response model.
pub fn extract_model_from_copilot_otel_db(
    stream_path: &Path,
) -> Result<Option<String>, StreamError> {
    let session_id = stream_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Ok(None);
    }

    let db_path = match resolve_copilot_otel_db_path(stream_path) {
        Some(p) => p,
        None => return Ok(None),
    };

    let conn = match crate::streams::agents::opencode::open_sqlite_readonly(&db_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let model = conn
        .query_row(
            "SELECT request_model, response_model FROM spans \
             WHERE chat_session_id = ?1 \
             AND (request_model IS NOT NULL OR response_model IS NOT NULL) \
             ORDER BY end_time_ms DESC LIMIT 1",
            [session_id],
            |row| {
                let request: Option<String> = row.get(0)?;
                let response: Option<String> = row.get(1)?;
                Ok(request.filter(|s| !s.is_empty()).or(response))
            },
        )
        .ok()
        .flatten();

    Ok(model)
}

fn extract_model_from_copilot_session_json(path: &Path) -> Result<Option<String>, StreamError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let model = json
        .get("requests")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|req| {
                req.get("modelId")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
        });

    Ok(model)
}

fn extract_model_from_amp_thread_json(path: &Path) -> Result<Option<String>, StreamError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let model = json
        .get("messages")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|msg| {
                msg.get("usage")
                    .and_then(|u| u.get("model"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
        });

    Ok(model)
}

fn extract_model_from_opencode_sqlite(
    path: &Path,
    session_id: Option<&str>,
) -> Result<Option<String>, StreamError> {
    let conn = match crate::streams::agents::opencode::open_sqlite_readonly(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // OpenCode stores model info in two places depending on message role:
    //   User messages:     data.model.modelID  (nested object)
    //   Assistant messages: data.modelID        (top-level string)
    let (query, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match session_id {
        Some(sid) => (
            "SELECT data FROM message WHERE session_id = ? AND (data LIKE '%\"modelID\"%' OR data LIKE '%\"model\"%') LIMIT 1",
            vec![Box::new(sid.to_string())],
        ),
        None => (
            "SELECT data FROM message WHERE (data LIKE '%\"modelID\"%' OR data LIKE '%\"model\"%') LIMIT 1",
            vec![],
        ),
    };

    let result: Option<String> = conn
        .query_row(query, rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|data| {
            let json: serde_json::Value = serde_json::from_str(&data).ok()?;
            // Try user message format: data.model.modelID
            if let Some(model) = json
                .get("model")
                .and_then(|m| m.get("modelID"))
                .and_then(|v| v.as_str())
            {
                return Some(model.to_string());
            }
            // Try assistant message format: data.modelID
            json.get("modelID")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn test_extract_model_claude() {
        let path = fixture_path("example-claude-code.jsonl");
        let result = extract_model(&path, StreamFormat::ClaudeJsonl, None).unwrap();
        assert_eq!(result, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_extract_model_droid_settings() {
        let path = fixture_path("droid-session.settings.json");
        let result = extract_model_from_droid_settings(&path).unwrap();
        assert_eq!(result, Some("custom:BYOK-GPT-5-MINI-0".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_session() {
        let path = fixture_path("copilot_session_simple.json");
        let result = extract_model(&path, StreamFormat::CopilotSessionJson, None).unwrap();
        assert_eq!(result, Some("copilot/claude-sonnet-4".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_event_stream() {
        let path = fixture_path("copilot_session_event_stream.jsonl");
        let result = extract_model(&path, StreamFormat::CopilotEventStreamJsonl, None).unwrap();
        // No model field in this fixture
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_gemini() {
        let path = fixture_path("gemini-session-simple.jsonl");
        let result = extract_model(&path, StreamFormat::GeminiJsonl, None).unwrap();
        assert_eq!(result, Some("gemini-2.5-flash".to_string()));
    }

    #[test]
    fn test_extract_model_amp() {
        let path = fixture_path("amp-threads/T-019ca1ce-3ae2-7686-a41e-ccc078837f8a.json");
        let result = extract_model(&path, StreamFormat::AmpThreadJson, None).unwrap();
        assert_eq!(result, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn test_extract_model_opencode() {
        let path = fixture_path("opencode-sqlite/opencode.db");
        let result = extract_model(
            &path,
            StreamFormat::OpenCodeSqlite,
            Some("test-session-123"),
        )
        .unwrap();
        assert_eq!(result, Some("gpt-5".to_string()));
    }

    #[test]
    fn test_extract_model_opencode_assistant_message_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO message VALUES ('msg-1', 'sess-1', 1000, 1000, '{\"role\":\"assistant\",\"modelID\":\"claude-opus-4-6\",\"providerID\":\"anthropic\"}');",
        ).unwrap();
        drop(conn);

        let result = extract_model(&db_path, StreamFormat::OpenCodeSqlite, Some("sess-1")).unwrap();
        assert_eq!(result, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_cli() {
        let path = fixture_path("copilot_cli_session_events.jsonl");
        let result = extract_model(&path, StreamFormat::CopilotEventStreamJsonl, None).unwrap();
        assert_eq!(result, Some("gpt-4.1".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_cli_no_model() {
        let path = fixture_path("copilot_cli_session_no_model.jsonl");
        let result = extract_model(&path, StreamFormat::CopilotEventStreamJsonl, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_missing_file() {
        let path = PathBuf::from("/nonexistent/path/to/file.jsonl");
        let result = extract_model(&path, StreamFormat::ClaudeJsonl, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_empty_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let result = extract_model(file.path(), StreamFormat::ClaudeJsonl, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_droid_settings_missing_file() {
        let path = PathBuf::from("/nonexistent/settings.json");
        let result = extract_model_from_droid_settings(&path).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_unsupported_format_returns_none() {
        let path = fixture_path("example-claude-code.jsonl");
        let result = extract_model(&path, StreamFormat::DroidJsonl, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_claude_model_not_on_last_line() {
        let path = fixture_path("claude-model-not-last.jsonl");
        let result = extract_model(&path, StreamFormat::ClaudeJsonl, None).unwrap();
        assert_eq!(result, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn test_extract_model_skips_synthetic_model() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":"hello"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","content":[{{"type":"text","text":"hi"}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"bye"}}]}}}}"#).unwrap();
        file.flush().unwrap();

        let result = extract_model(file.path(), StreamFormat::ClaudeJsonl, None).unwrap();
        assert_eq!(result, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_vscode_models_json() {
        let path = fixture_path(
            "copilot_vscode_workspace/GitHub.copilot-chat/transcripts/test-session-abc.jsonl",
        );
        let result = extract_model_from_copilot_models_json(&path).unwrap();
        assert_eq!(result, Some("gpt-4.1".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_vscode_models_json_missing() {
        let path = PathBuf::from("/nonexistent/transcripts/fake-session.jsonl");
        let result = extract_model_from_copilot_models_json(&path).unwrap();
        assert_eq!(result, None);
    }

    /// Build a Copilot VS Code workspace layout in `root` and an `agent-traces.db`
    /// OTEL DB at the expected globalStorage location. Returns the transcript path
    /// (which need not exist on disk) for the given session id.
    fn setup_copilot_otel_workspace(root: &Path, session_id: &str) -> (PathBuf, PathBuf) {
        let transcripts_dir = root
            .join("User")
            .join("workspaceStorage")
            .join("hash123")
            .join("GitHub.copilot-chat")
            .join("transcripts");
        std::fs::create_dir_all(&transcripts_dir).unwrap();
        let transcript_path = transcripts_dir.join(format!("{session_id}.jsonl"));

        let global_storage_dir = root
            .join("User")
            .join("globalStorage")
            .join("github.copilot-chat");
        std::fs::create_dir_all(&global_storage_dir).unwrap();
        let db_path = global_storage_dir.join("agent-traces.db");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE spans (
                span_id TEXT PRIMARY KEY, trace_id TEXT NOT NULL, parent_span_id TEXT,
                name TEXT NOT NULL, start_time_ms INTEGER NOT NULL, end_time_ms INTEGER NOT NULL,
                status_code INTEGER NOT NULL DEFAULT 0, status_message TEXT,
                operation_name TEXT, provider_name TEXT, agent_name TEXT, conversation_id TEXT,
                request_model TEXT, response_model TEXT,
                input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER, reasoning_tokens INTEGER,
                tool_name TEXT, tool_call_id TEXT, tool_type TEXT,
                chat_session_id TEXT, turn_index INTEGER, ttft_ms REAL
            );",
        )
        .unwrap();
        drop(conn);

        (transcript_path, db_path)
    }

    fn insert_otel_span(
        db_path: &Path,
        span_id: &str,
        end_time_ms: i64,
        chat_session_id: &str,
        request_model: Option<&str>,
        response_model: Option<&str>,
    ) {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "INSERT INTO spans (span_id, trace_id, name, start_time_ms, end_time_ms, \
             request_model, response_model, chat_session_id) \
             VALUES (?1, 'trace1', 'chat', ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                span_id,
                end_time_ms - 1000,
                end_time_ms,
                request_model,
                response_model,
                chat_session_id,
            ],
        )
        .unwrap();
    }

    #[test]
    fn test_extract_model_copilot_otel_prefers_request_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let (transcript_path, db_path) = setup_copilot_otel_workspace(dir.path(), "sess-otel-1");
        insert_otel_span(
            &db_path,
            "span1",
            1000,
            "sess-otel-1",
            Some("claude-sonnet-4"),
            Some("claude-sonnet-4-20250514"),
        );

        let result = extract_model_from_copilot_otel_db(&transcript_path).unwrap();
        assert_eq!(result, Some("claude-sonnet-4".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_otel_falls_back_to_response_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let (transcript_path, db_path) = setup_copilot_otel_workspace(dir.path(), "sess-otel-2");
        insert_otel_span(
            &db_path,
            "span1",
            1000,
            "sess-otel-2",
            None,
            Some("gpt-4.1"),
        );

        let result = extract_model_from_copilot_otel_db(&transcript_path).unwrap();
        assert_eq!(result, Some("gpt-4.1".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_otel_uses_most_recent_span() {
        let dir = tempfile::TempDir::new().unwrap();
        let (transcript_path, db_path) = setup_copilot_otel_workspace(dir.path(), "sess-otel-3");
        insert_otel_span(
            &db_path,
            "span1",
            1000,
            "sess-otel-3",
            Some("gpt-4.1"),
            None,
        );
        insert_otel_span(
            &db_path,
            "span2",
            5000,
            "sess-otel-3",
            Some("claude-sonnet-4"),
            None,
        );

        let result = extract_model_from_copilot_otel_db(&transcript_path).unwrap();
        assert_eq!(result, Some("claude-sonnet-4".to_string()));
    }

    #[test]
    fn test_extract_model_copilot_otel_ignores_other_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let (transcript_path, db_path) = setup_copilot_otel_workspace(dir.path(), "sess-otel-4");
        insert_otel_span(
            &db_path,
            "span1",
            1000,
            "other-session",
            Some("gpt-4.1"),
            None,
        );

        let result = extract_model_from_copilot_otel_db(&transcript_path).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_copilot_otel_missing_db() {
        let path = PathBuf::from("/nonexistent/transcripts/fake-session.jsonl");
        let result = extract_model_from_copilot_otel_db(&path).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_head_fallback_for_large_file() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::with_suffix(".jsonl").unwrap();
        // model_change at the start
        writeln!(file, r#"{{"type":"session.start","data":{{"sessionId":"s1"}},"id":"e1","timestamp":"2026-01-01T00:00:00Z","parentId":null}}"#).unwrap();
        writeln!(file, r#"{{"type":"session.model_change","data":{{"newModel":"gpt-4.1"}},"id":"e2","timestamp":"2026-01-01T00:00:01Z","parentId":"e1"}}"#).unwrap();
        // Pad with >50KB of filler events so the model_change falls outside the tail window
        for i in 0..600 {
            writeln!(file, r#"{{"type":"user.message","data":{{"content":"padding message number {} with extra text to make the line longer and push past the fifty kilobyte tail read window boundary"}},"id":"pad-{}","timestamp":"2026-01-01T00:01:{:02}Z","parentId":null}}"#, i, i, i % 60).unwrap();
        }
        file.flush().unwrap();

        let size = std::fs::metadata(file.path()).unwrap().len();
        assert!(
            size > 51200,
            "file must exceed 50KB tail window, got {}",
            size
        );

        let result =
            extract_model(file.path(), StreamFormat::CopilotEventStreamJsonl, None).unwrap();
        assert_eq!(result, Some("gpt-4.1".to_string()));
    }
}
