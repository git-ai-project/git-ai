//! OpenCode agent implementation (SQLite-only).

use crate::metrics::events::AgentTraceValues;
use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{TimestampWatermark, WatermarkStrategy};
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct OpenCodeDbMessageData {
    role: String,
    #[serde(default)]
    time: Option<OpenCodeTime>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenCodeTime {
    created: i64,
    #[allow(dead_code)]
    completed: Option<i64>,
}

#[derive(Debug)]
struct OpenCodeSourceMessage {
    id: String,
    role: String,
    created: i64,
    model_id: Option<String>,
    provider_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenCodeToolState {
    #[allow(dead_code)]
    status: Option<String>,
    input: Option<serde_json::Value>,
    #[allow(dead_code)]
    output: Option<serde_json::Value>,
    #[allow(dead_code)]
    title: Option<String>,
    #[allow(dead_code)]
    metadata: Option<serde_json::Value>,
    #[allow(dead_code)]
    time: Option<OpenCodePartTime>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
enum OpenCodePart {
    Text {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        text: String,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        synthetic: Option<bool>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    Tool {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        tool: String,
        #[serde(rename = "callID")]
        #[allow(dead_code)]
        call_id: String,
        state: Option<OpenCodeToolState>,
        input: Option<serde_json::Value>,
        #[allow(dead_code)]
        output: Option<serde_json::Value>,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepStart {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepFinish {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct OpenCodePartTime {
    start: i64,
    #[allow(dead_code)]
    end: Option<i64>,
}

/// OpenCode agent that reads from an OpenCode SQLite database.
pub struct OpenCodeAgent;

fn open_sqlite_readonly(path: &Path) -> Result<Connection, TranscriptError> {
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
            TranscriptError::Fatal {
                message: format!("Failed to open OpenCode database {}: {}", path.display(), e),
            }
        })?;

    conn.execute_batch("PRAGMA cache_size = -2000;")
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to set PRAGMA cache_size: {}", e),
        })?;

    Ok(conn)
}

fn read_session_messages(
    conn: &Connection,
    session_id: &str,
    after_created: i64,
) -> Result<Vec<OpenCodeSourceMessage>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message \
             WHERE session_id = ? AND time_created > ? \
             ORDER BY time_created ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare message query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id, after_created], |row| {
            let id: String = row.get(0)?;
            let time_created: i64 = row.get(1)?;
            let data: String = row.get(2)?;
            Ok((id, time_created, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query messages: {}", e),
        })?;

    let mut messages = Vec::new();
    for row in rows {
        let (id, time_created, data) = row.map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read message row: {}", e),
        })?;

        let parsed: OpenCodeDbMessageData =
            serde_json::from_str(&data).map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Failed to parse message data for id {}: {}", id, e),
            })?;

        let created = parsed
            .time
            .as_ref()
            .map(|t| t.created)
            .unwrap_or(time_created);

        messages.push(OpenCodeSourceMessage {
            id,
            role: parsed.role,
            created,
            model_id: parsed.model_id,
            provider_id: parsed.provider_id,
        });
    }

    Ok(messages)
}

fn read_all_parts(
    conn: &Connection,
    session_id: &str,
    _after_created: i64,
) -> Result<HashMap<String, Vec<OpenCodePart>>, TranscriptError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, message_id, time_created, data FROM part \
             WHERE session_id = ? \
             ORDER BY message_id ASC, id ASC",
        )
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to prepare part query: {}", e),
        })?;

    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            let _id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            Ok((message_id, time_created, data))
        })
        .map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to query parts: {}", e),
        })?;

    let mut parts_by_message: HashMap<String, Vec<(OpenCodePart, i64)>> = HashMap::new();
    for row in rows {
        let (message_id, time_created, data) = row.map_err(|e| TranscriptError::Fatal {
            message: format!("Failed to read part row: {}", e),
        })?;

        // Skip parts that fail to parse (unknown types handled by #[serde(other)])
        if let Ok(part) = serde_json::from_str::<OpenCodePart>(&data) {
            parts_by_message
                .entry(message_id)
                .or_default()
                .push((part, time_created));
        }
    }

    // Sort parts within each message by their time, then convert to just parts
    let mut result = HashMap::new();
    for (message_id, mut parts_with_time) in parts_by_message {
        parts_with_time.sort_by_key(|(part, fallback)| part_sort_key(part, *fallback));
        let parts = parts_with_time.into_iter().map(|(part, _)| part).collect();
        result.insert(message_id, parts);
    }

    Ok(result)
}

fn part_sort_key(part: &OpenCodePart, fallback: i64) -> i64 {
    match part {
        OpenCodePart::Text { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::Tool { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::StepStart { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::StepFinish { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::Unknown => fallback,
    }
}

impl Agent for OpenCodeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        // Discovery comes from presets, not sweep.
        Ok(Vec::new())
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        // Downcast to TimestampWatermark
        let ts_watermark = watermark
            .as_any()
            .downcast_ref::<TimestampWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "OpenCode reader requires TimestampWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let watermark_millis = ts_watermark.0.timestamp_millis();

        // Open SQLite read-only
        let conn = open_sqlite_readonly(path)?;

        // Read messages with time_created > watermark_millis
        let messages = read_session_messages(&conn, session_id, watermark_millis)?;

        if messages.is_empty() {
            return Ok(TranscriptBatch {
                events: Vec::new(),
                model: None,
                new_watermark: Box::new(TimestampWatermark::new(ts_watermark.0)),
            });
        }

        // Read all parts for the session, build HashMap by message_id
        let parts_by_message = read_all_parts(&conn, session_id, watermark_millis)?;

        // Track model: first assistant with provider_id/model_id
        let mut model: Option<String> = None;
        let mut max_created: i64 = watermark_millis;
        let mut events = Vec::new();

        for msg in &messages {
            // Update max_created
            if msg.created > max_created {
                max_created = msg.created;
            }

            // Track model from first assistant message that has it
            if model.is_none()
                && msg.role == "assistant"
                && let Some(ref mid) = msg.model_id
            {
                model = Some(match &msg.provider_id {
                    Some(pid) => format!("{}/{}", pid, mid),
                    None => mid.clone(),
                });
            }

            // Convert created (milliseconds) to RFC3339 timestamp for event_ts (seconds)
            let event_ts = (msg.created / 1000) as u64;

            // Get parts for this message
            let parts = parts_by_message.get(&msg.id);

            if let Some(parts) = parts {
                for part in parts {
                    match part {
                        OpenCodePart::Text { text, .. } => {
                            if text.trim().is_empty() {
                                continue;
                            }
                            if msg.role == "user" {
                                events.push(
                                    AgentTraceValues::new()
                                        .event_type("user_message")
                                        .prompt_text(text.as_str())
                                        .event_ts(event_ts),
                                );
                            } else if msg.role == "assistant" {
                                events.push(
                                    AgentTraceValues::new()
                                        .event_type("assistant_message")
                                        .response_text(text.as_str())
                                        .event_ts(event_ts),
                                );
                            }
                        }
                        OpenCodePart::Tool {
                            tool, input, state, ..
                        } => {
                            if msg.role == "assistant" {
                                let tool_input = input
                                    .as_ref()
                                    .or_else(|| state.as_ref().and_then(|s| s.input.as_ref()));

                                let mut event = AgentTraceValues::new()
                                    .event_type("tool_use")
                                    .tool_name(tool.as_str())
                                    .event_ts(event_ts);

                                // Include tool input as response_text if available
                                if let Some(input_val) = tool_input
                                    && let Ok(input_str) = serde_json::to_string(input_val)
                                {
                                    event = event.response_text(input_str);
                                }

                                events.push(event);
                            }
                        }
                        OpenCodePart::StepStart { .. }
                        | OpenCodePart::StepFinish { .. }
                        | OpenCodePart::Unknown => {
                            // Skip
                        }
                    }
                }
            }
        }

        // New watermark from max_created
        let new_watermark_ts =
            DateTime::from_timestamp_millis(max_created).unwrap_or(ts_watermark.0);
        let new_watermark = Box::new(TimestampWatermark::new(new_watermark_ts));

        Ok(TranscriptBatch {
            events,
            model,
            new_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sweep_strategy() {
        let agent = OpenCodeAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    // SQLite tests require fixtures, so keep unit tests minimal.
    // Integration tests in tests/integration/opencode.rs cover the full flow.
}
