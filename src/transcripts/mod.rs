//! Transcript reading — unified incremental reader for all AI agent session files.
//!
//! v1 had 7K lines across 11 per-agent readers that were 90% identical copy-paste.
//! v2 collapses this into one generic reader parameterized by agent config tables.
//!
//! The two reading strategies:
//! - **ByteOffset**: seek to byte position, read lines (JSONL files — most agents)
//! - **RecordIndex**: parse whole file, skip N records (JSON array files — amp, continue, copilot sessions)

mod reader;

pub use reader::{
    read_jsonl_incremental, read_json_array_incremental, TranscriptBatch, TranscriptError,
};

use std::path::{Path, PathBuf};

/// Per-agent configuration for transcript discovery.
pub struct AgentTranscriptConfig {
    pub tool: &'static str,
    pub discovery: DiscoveryStrategy,
    pub format: TranscriptFormat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TranscriptFormat {
    Jsonl,
    JsonArray,
}

#[derive(Debug, Clone)]
pub enum DiscoveryStrategy {
    /// Scan these directories (relative to home) for files matching the glob.
    ScanDirs {
        dirs: &'static [&'static str],
        extension: &'static str,
        recursive: bool,
    },
    /// Transcript path is provided in the checkpoint hook payload (no discovery needed).
    FromHookPayload,
}

static AGENT_TRANSCRIPT_CONFIGS: &[AgentTranscriptConfig] = &[
    AgentTranscriptConfig {
        tool: "cursor",
        discovery: DiscoveryStrategy::ScanDirs {
            dirs: &[".config/Cursor/User/globalStorage/conversations"],
            extension: "jsonl",
            recursive: false,
        },
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "claude",
        discovery: DiscoveryStrategy::ScanDirs {
            dirs: &[".claude/projects"],
            extension: "jsonl",
            recursive: true,
        },
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "codex",
        discovery: DiscoveryStrategy::ScanDirs {
            dirs: &[".codex/sessions"],
            extension: "jsonl",
            recursive: false,
        },
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "gemini",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "windsurf",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "droid",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "pi",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::Jsonl,
    },
    AgentTranscriptConfig {
        tool: "amp",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::JsonArray,
    },
    AgentTranscriptConfig {
        tool: "continue-cli",
        discovery: DiscoveryStrategy::FromHookPayload,
        format: TranscriptFormat::JsonArray,
    },
    AgentTranscriptConfig {
        tool: "github-copilot",
        discovery: DiscoveryStrategy::ScanDirs {
            dirs: &[
                ".config/github-copilot/sessions",
                ".config/github-copilot/events",
            ],
            extension: "jsonl",
            recursive: false,
        },
        format: TranscriptFormat::Jsonl,
    },
];

pub fn get_config(tool: &str) -> Option<&'static AgentTranscriptConfig> {
    AGENT_TRANSCRIPT_CONFIGS.iter().find(|c| c.tool == tool)
}

/// Discover transcript files for an agent by scanning known directories.
pub fn discover_sessions(tool: &str) -> Vec<PathBuf> {
    let config = match get_config(tool) {
        Some(c) => c,
        None => return vec![],
    };

    let dirs = match &config.discovery {
        DiscoveryStrategy::ScanDirs { dirs, extension, recursive } => {
            let home = match std::env::var("HOME") {
                Ok(h) => PathBuf::from(h),
                Err(_) => return vec![],
            };
            let mut results = Vec::new();
            for dir in *dirs {
                let full = home.join(dir);
                if full.is_dir() {
                    scan_dir(&full, extension, *recursive, &mut results);
                }
            }
            results
        }
        DiscoveryStrategy::FromHookPayload => vec![],
    };

    dirs
}

fn scan_dir(dir: &Path, extension: &str, recursive: bool, results: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some(extension) {
            results.push(path);
        } else if recursive && path.is_dir() {
            scan_dir(&path, extension, true, results);
        }
    }
}
