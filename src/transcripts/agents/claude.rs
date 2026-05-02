//! Claude Code agent implementation with sweep discovery.

use crate::transcripts::agent::Agent;
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy, TranscriptFormat};
use crate::transcripts::types::{TranscriptBatch, TranscriptError};
use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy, WatermarkType};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Claude Code agent that discovers conversations from Claude Code storage.
pub struct ClaudeAgent;

impl ClaudeAgent {
    /// Scan for Claude conversation files in standard locations.
    fn scan_conversation_files() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Check CLAUDE_CONFIG_DIR override first
        let base_dir = if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
            Some(PathBuf::from(config_dir))
        } else {
            dirs::home_dir().map(|p| p.join(".claude"))
        };

        // Search paths:
        // 1. ~/.claude/projects/**/*.jsonl (or $CLAUDE_CONFIG_DIR/projects/**/*.jsonl)
        // 2. ~/.config/claude/projects/**/*.jsonl
        let search_dirs = vec![
            base_dir.as_ref().map(|p| p.join("projects")),
            dirs::config_dir().map(|p| p.join("claude/projects")),
        ];

        for dir_opt in search_dirs {
            if let Some(dir) = dir_opt
                && dir.exists()
            {
                // Recursively scan for *.jsonl files
                Self::scan_jsonl_recursive(&dir, &mut paths);
            }
        }

        paths
    }

    /// Recursively scan directory for *.jsonl files.
    fn scan_jsonl_recursive(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::scan_jsonl_recursive(&path, paths);
            } else if path.is_file() && path.extension().map(|ext| ext == "jsonl").unwrap_or(false)
            {
                paths.push(path);
            }
        }
    }

    /// Extract session ID from a Claude conversation file path.
    ///
    /// Claude files are typically named like: `<uuid>.jsonl` under `projects/<project-dir>/`
    fn extract_session_id(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("claude:{}", s))
    }
}

impl Agent for ClaudeAgent {
    fn sweep_strategy(&self) -> SweepStrategy {
        // Poll every 30 minutes for new Claude conversations
        SweepStrategy::Periodic(Duration::from_secs(30 * 60))
    }

    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError> {
        let paths = Self::scan_conversation_files();
        let mut sessions = Vec::new();

        for path in paths {
            let Some(session_id) = Self::extract_session_id(&path) else {
                continue;
            };

            // Don't parse file content here - just filesystem scanning.
            // Model will be extracted later during first read_incremental() if needed.
            let session = DiscoveredSession {
                session_id,
                agent_type: "claude".to_string(),
                transcript_path: path,
                transcript_format: TranscriptFormat::ClaudeJsonl,
                watermark_type: WatermarkType::ByteOffset,
                initial_watermark: Box::new(ByteOffsetWatermark::new(0)),
                model: None,
                tool: Some("Claude Code".to_string()),
                external_thread_id: None,
            };

            sessions.push(session);
        }

        Ok(sessions)
    }

    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError> {
        use std::fs::File;
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let byte_watermark = watermark
            .as_any()
            .downcast_ref::<ByteOffsetWatermark>()
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!(
                    "Claude reader requires ByteOffsetWatermark, got incompatible type for session {}",
                    session_id
                ),
            })?;

        let start_offset = byte_watermark.0;

        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TranscriptError::Fatal {
                    message: format!("Transcript file not found: {}", path.display()),
                }
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                TranscriptError::Fatal {
                    message: format!("Permission denied reading transcript: {}", path.display()),
                }
            } else {
                TranscriptError::Transient {
                    message: format!("Failed to open transcript file: {}", e),
                    retry_after: std::time::Duration::from_secs(5),
                }
            }
        })?;

        let mut reader = BufReader::new(file);

        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| TranscriptError::Transient {
                message: format!("Failed to seek to offset {}: {}", start_offset, e),
                retry_after: std::time::Duration::from_secs(5),
            })?;

        let mut events = Vec::new();
        let mut current_offset = start_offset;
        let mut line_number = 0;

        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read =
                reader
                    .read_line(&mut line)
                    .map_err(|e| TranscriptError::Transient {
                        message: format!("I/O error reading line: {}", e),
                        retry_after: std::time::Duration::from_secs(5),
                    })?;

            if bytes_read == 0 {
                break;
            }

            line_number += 1;
            current_offset += bytes_read as u64;

            if line.trim().is_empty() {
                continue;
            }

            let entry: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| TranscriptError::Parse {
                    line: line_number,
                    message: format!("Invalid JSON in {}: {}", path.display(), e),
                })?;

            events.push(entry);
        }

        let new_watermark = Box::new(ByteOffsetWatermark::new(current_offset));

        Ok(TranscriptBatch {
            events,
            new_watermark,
        })
    }
}

/// Check if a file path refers to a Claude plan file.
///
/// Claude plans are written under `~/.claude/plans/`. A path is a plan file
/// when it ends with `.md` and contains the path segment pair `.claude/plans`.
pub fn is_plan_file_path(file_path: &str) -> bool {
    let path = std::path::Path::new(file_path);
    let is_markdown = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !is_markdown {
        return false;
    }
    let components: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => {
                Some(segment.to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        })
        .collect();

    components
        .windows(2)
        .any(|window| window[0] == ".claude" && window[1] == "plans")
}

/// Extract plan content from a Write or Edit tool_use input if it targets a plan file.
///
/// Maintains a running `plan_states` map keyed by file path so that Edit operations
/// can reconstruct the full plan text.
pub fn extract_plan_from_tool_use(
    tool_name: &str,
    input: &serde_json::Value,
    plan_states: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    match tool_name {
        "Write" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let content = input.get("content")?.as_str()?;
            if content.trim().is_empty() {
                return None;
            }
            plan_states.insert(file_path.to_string(), content.to_string());
            Some(content.to_string())
        }
        "Edit" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let old_string = input.get("old_string").and_then(|v| v.as_str());
            let new_string = input.get("new_string").and_then(|v| v.as_str());

            match (old_string, new_string) {
                (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                    if let Some(current) = plan_states.get(file_path) {
                        let updated = current.replacen(old, new, 1);
                        plan_states.insert(file_path.to_string(), updated.clone());
                        Some(updated)
                    } else {
                        plan_states.insert(file_path.to_string(), new.to_string());
                        Some(new.to_string())
                    }
                }
                (None, Some(new)) if !new.is_empty() => {
                    plan_states.insert(file_path.to_string(), new.to_string());
                    Some(new.to_string())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_session_id() {
        let path =
            PathBuf::from("/home/user/.config/Claude/conversations/conversation_abc-123.jsonl");
        let session_id = ClaudeAgent::extract_session_id(&path);
        assert_eq!(session_id, Some("claude:conversation_abc-123".to_string()));
    }

    #[test]
    fn test_sweep_strategy() {
        let agent = ClaudeAgent;
        assert_eq!(
            agent.sweep_strategy(),
            SweepStrategy::Periodic(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn test_read_incremental_basic() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"content":"Hello"}},"timestamp":"2025-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Hi there"}}],"model":"claude-sonnet-4"}},"timestamp":"2025-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = ClaudeAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0]["type"].as_str(), Some("user"));
        assert_eq!(
            result.events[1]["message"]["model"].as_str(),
            Some("claude-sonnet-4")
        );
    }

    #[test]
    fn test_scan_discovers_real_claude_files() {
        let paths = ClaudeAgent::scan_conversation_files();
        // On this machine we have files in ~/.claude/projects/
        if dirs::home_dir()
            .map(|h| h.join(".claude/projects").exists())
            .unwrap_or(false)
        {
            assert!(
                !paths.is_empty(),
                "Should discover files in ~/.claude/projects/"
            );
            for path in &paths {
                assert!(path.extension().and_then(|s| s.to_str()) == Some("jsonl"));
            }
        }
    }

    #[test]
    fn test_read_incremental_with_token_usage() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Response"}}],"model":"claude-sonnet-4","usage":{{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":200,"cache_creation_input_tokens":300}}}},"timestamp":"2025-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let agent = ClaudeAgent;
        let watermark = Box::new(ByteOffsetWatermark::new(0));
        let result = agent
            .read_incremental(file.path(), watermark, "test-session")
            .unwrap();

        assert_eq!(result.events.len(), 1);
        let event = &result.events[0];
        let usage = &event["message"]["usage"];
        assert_eq!(usage["input_tokens"].as_u64(), Some(100));
        assert_eq!(usage["output_tokens"].as_u64(), Some(50));
        assert_eq!(usage["cache_read_input_tokens"].as_u64(), Some(200));
        assert_eq!(usage["cache_creation_input_tokens"].as_u64(), Some(300));
    }

    #[test]
    fn test_is_plan_file_path_valid() {
        assert!(is_plan_file_path("/home/user/.claude/plans/my-plan.md"));
        assert!(is_plan_file_path("/Users/test/.claude/plans/plan.MD"));
        assert!(is_plan_file_path(".claude/plans/plan.md"));
    }

    #[test]
    fn test_is_plan_file_path_invalid() {
        assert!(!is_plan_file_path("/home/user/plans/not-a-plan.md"));
        assert!(!is_plan_file_path("/home/user/.claude/plans/not-md.txt"));
        assert!(!is_plan_file_path("/home/user/.claude/config.md"));
    }

    #[test]
    fn test_extract_plan_from_write() {
        let mut states = std::collections::HashMap::new();
        let input = serde_json::json!({
            "file_path": "/home/user/.claude/plans/plan.md",
            "content": "# My Plan\n\nStep 1: do stuff"
        });
        let result = extract_plan_from_tool_use("Write", &input, &mut states);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "# My Plan\n\nStep 1: do stuff");
    }

    #[test]
    fn test_extract_plan_from_edit() {
        let mut states = std::collections::HashMap::new();
        states.insert(
            "/home/user/.claude/plans/plan.md".to_string(),
            "Step 1: old".to_string(),
        );
        let input = serde_json::json!({
            "file_path": "/home/user/.claude/plans/plan.md",
            "old_string": "old",
            "new_string": "new"
        });
        let result = extract_plan_from_tool_use("Edit", &input, &mut states);
        assert_eq!(result, Some("Step 1: new".to_string()));
    }

    #[test]
    fn test_extract_plan_non_plan_file() {
        let mut states = std::collections::HashMap::new();
        let input = serde_json::json!({
            "file_path": "/home/user/src/main.rs",
            "content": "fn main() {}"
        });
        let result = extract_plan_from_tool_use("Write", &input, &mut states);
        assert!(result.is_none());
    }
}
