use crate::error::GitAiError;
use crate::synopsis::types::{ConversationExchange, ConversationLog, Speaker};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

/// Derive the Claude Code project hash from a repository path.
///
/// Claude Code encodes the project directory as a path with `/` replaced by `-`
/// and the leading `-` stripped.
///
/// Example: `/Users/foo/myrepo` -> `Users-foo-myrepo`
fn claude_project_hash(repo_path: &Path) -> String {
    let path_str = repo_path.to_string_lossy();
    // Replace path separators with `-`
    #[cfg(windows)]
    let hash = path_str.replace('\\', "-").replace('/', "-");
    #[cfg(not(windows))]
    let hash = path_str.replace('/', "-");

    // Strip leading `-`
    hash.trim_start_matches('-').to_string()
}

/// Find the most recently modified Claude Code conversation JSONL file for the
/// given repository. Returns the path to the file, or `None` if nothing is found.
pub fn find_claude_code_conversation(repo_path: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let hash = claude_project_hash(repo_path);
    let projects_dir = home.join(".claude").join("projects").join(&hash);

    if !projects_dir.exists() {
        return None;
    }

    let read_dir = fs::read_dir(&projects_dir).ok()?;

    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((path, modified))
            } else {
                None
            }
        })
        .collect();

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.into_iter().next().map(|(path, _)| path)
}

/// Parse a Claude Code JSONL file into a `ConversationLog`.
///
/// Lines that cannot be parsed are silently skipped so that partial/corrupted
/// files do not abort the entire operation.
pub fn parse_claude_code_jsonl(path: &Path) -> Result<ConversationLog, GitAiError> {
    let content = fs::read_to_string(path).map_err(GitAiError::IoError)?;
    let mut exchanges: Vec<ConversationExchange> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(entry): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };

        let timestamp: Option<DateTime<Utc>> = entry["timestamp"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        let entry_type = entry["type"].as_str().unwrap_or("");

        match entry_type {
            "user" => {
                let text = extract_text_from_content(&entry["message"]["content"]);
                if !text.is_empty() {
                    exchanges.push(ConversationExchange {
                        speaker: Speaker::User,
                        text,
                        timestamp,
                    });
                }
            }
            "assistant" => {
                // Collect text blocks and tool_use blocks separately
                let content_val = &entry["message"]["content"];
                if let Some(blocks) = content_val.as_array() {
                    for block in blocks {
                        let block_type = block["type"].as_str().unwrap_or("");
                        match block_type {
                            "text" => {
                                let text = block["text"].as_str().unwrap_or("").to_string();
                                if !text.is_empty() {
                                    exchanges.push(ConversationExchange {
                                        speaker: Speaker::Assistant,
                                        text,
                                        timestamp,
                                    });
                                }
                            }
                            "tool_use" => {
                                let tool_name =
                                    block["name"].as_str().unwrap_or("unknown_tool").to_string();
                                // Represent tool use as a compact summary
                                let input_summary =
                                    summarise_tool_input(&tool_name, &block["input"]);
                                exchanges.push(ConversationExchange {
                                    speaker: Speaker::ToolUse(tool_name),
                                    text: input_summary,
                                    timestamp,
                                });
                            }
                            _ => {}
                        }
                    }
                } else {
                    // Scalar string content
                    let text = extract_text_from_content(content_val);
                    if !text.is_empty() {
                        exchanges.push(ConversationExchange {
                            speaker: Speaker::Assistant,
                            text,
                            timestamp,
                        });
                    }
                }
            }
            _ => {
                // Skip summary, tool_result, system, etc.
            }
        }
    }

    Ok(ConversationLog {
        source_kind: "claude-code".to_string(),
        exchanges,
        source_path: path.to_string_lossy().to_string(),
    })
}

/// Extract a plain-text string from a Claude content field, which may be:
/// - a plain JSON string, or
/// - an array of content blocks with `{"type": "text", "text": "..."}`.
fn extract_text_from_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .filter_map(|b| {
                if b["type"].as_str() == Some("text") {
                    b["text"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Produce a brief human-readable description of a tool invocation.
fn summarise_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Write" | "create_file" => {
            let path = input["path"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .unwrap_or("(unknown path)");
            format!("[Write {}]", path)
        }
        "Edit" | "str_replace_editor" => {
            let path = input["path"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .unwrap_or("(unknown path)");
            format!("[Edit {}]", path)
        }
        "Read" | "view" => {
            let path = input["path"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .unwrap_or("(unknown path)");
            format!("[Read {}]", path)
        }
        "Bash" | "execute_bash" => {
            let cmd = input["command"]
                .as_str()
                .unwrap_or("(command)")
                .chars()
                .take(120)
                .collect::<String>();
            format!("[Bash: {}]", cmd)
        }
        "Glob" | "list_files" => {
            let pattern = input["pattern"].as_str().unwrap_or("(pattern)");
            format!("[Glob: {}]", pattern)
        }
        "Grep" | "search_files" => {
            let pattern = input["pattern"].as_str().unwrap_or("(pattern)");
            format!("[Grep: {}]", pattern)
        }
        _ => {
            // Generic fallback: show tool name and first string field if any
            if let Some(obj) = input.as_object() {
                if let Some(first_val) = obj.values().find_map(|v| v.as_str()) {
                    let preview: String = first_val.chars().take(80).collect();
                    return format!("[{}: {}]", tool_name, preview);
                }
            }
            format!("[{}]", tool_name)
        }
    }
}

/// Filter a `ConversationLog` to exchanges that occurred within `window_minutes`
/// minutes before the most recent timestamp in the log.
///
/// If no timestamps are present the full log is returned unchanged.
pub fn filter_by_time_window(log: &ConversationLog, window_minutes: u64) -> ConversationLog {
    let timestamps: Vec<DateTime<Utc>> = log.exchanges.iter().filter_map(|e| e.timestamp).collect();

    let Some(end_time) = timestamps.iter().copied().max() else {
        // No timestamps — return everything
        return log.clone();
    };

    let window = chrono::Duration::minutes(window_minutes as i64);
    let start_time = end_time - window;

    let filtered = log
        .exchanges
        .iter()
        .filter(|e| {
            // Keep exchanges that either have no timestamp or fall within the window
            e.timestamp
                .map_or(true, |ts| ts >= start_time && ts <= end_time)
        })
        .cloned()
        .collect();

    ConversationLog {
        source_kind: log.source_kind.clone(),
        exchanges: filtered,
        source_path: log.source_path.clone(),
    }
}

/// Render a `ConversationLog` to a human-readable string for inclusion in a
/// synopsis prompt, truncating to `max_chars` to avoid exceeding context limits.
pub fn render_conversation(log: &ConversationLog, max_chars: usize) -> String {
    let mut out = String::new();

    for exchange in &log.exchanges {
        let prefix = match &exchange.speaker {
            Speaker::User => "**User**: ".to_string(),
            Speaker::Assistant => "**Assistant**: ".to_string(),
            Speaker::ToolUse(name) => format!("**Tool ({})**: ", name),
        };

        let line = format!("{}{}\n\n", prefix, exchange.text.trim());

        if out.len() + line.len() > max_chars {
            // Truncate to fit within budget
            let remaining = max_chars.saturating_sub(out.len());
            if remaining > 64 {
                out.push_str(&line[..remaining]);
                out.push_str("\n\n[... conversation truncated for length ...]\n");
            }
            break;
        }

        out.push_str(&line);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_project_hash_unix() {
        let path = Path::new("/Users/foo/myrepo");
        assert_eq!(claude_project_hash(path), "Users-foo-myrepo");
    }

    #[test]
    fn test_claude_project_hash_nested() {
        let path = Path::new("/home/user/projects/git-ai");
        assert_eq!(claude_project_hash(path), "home-user-projects-git-ai");
    }

    #[test]
    fn test_extract_text_string() {
        let val = serde_json::json!("hello world");
        assert_eq!(extract_text_from_content(&val), "hello world");
    }

    #[test]
    fn test_extract_text_blocks() {
        let val = serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "image", "source": {}},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(extract_text_from_content(&val), "first\nsecond");
    }

    #[test]
    fn test_filter_by_time_window_no_timestamps() {
        let log = ConversationLog {
            source_kind: "claude-code".to_string(),
            exchanges: vec![ConversationExchange {
                speaker: Speaker::User,
                text: "hello".to_string(),
                timestamp: None,
            }],
            source_path: "/tmp/test.jsonl".to_string(),
        };
        let filtered = filter_by_time_window(&log, 60);
        assert_eq!(filtered.exchanges.len(), 1);
    }

    #[test]
    fn test_render_conversation_truncates() {
        let log = ConversationLog {
            source_kind: "claude-code".to_string(),
            exchanges: vec![
                ConversationExchange {
                    speaker: Speaker::User,
                    text: "x".repeat(200),
                    timestamp: None,
                },
                ConversationExchange {
                    speaker: Speaker::Assistant,
                    text: "y".repeat(200),
                    timestamp: None,
                },
            ],
            source_path: "/tmp/test.jsonl".to_string(),
        };
        let rendered = render_conversation(&log, 100);
        assert!(rendered.len() <= 200); // Should be truncated
    }
}
