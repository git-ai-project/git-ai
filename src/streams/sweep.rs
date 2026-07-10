// src/streams/sweep.rs

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A single agent may retain at most this many transcript paths during discovery.
pub(crate) const MAX_DISCOVERED_SESSIONS_PER_AGENT: usize = 4_096;

/// Caps recursive nesting and simultaneously open directory iterators.
const MAX_DISCOVERY_DIRECTORY_DEPTH: usize = 128;

#[derive(Debug, Eq, PartialEq)]
struct RecentPath {
    modified: SystemTime,
    path: PathBuf,
}

impl Ord for RecentPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.modified
            .cmp(&other.modified)
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for RecentPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Retains only the newest `limit` paths while a directory iterator is consumed.
pub(crate) struct BoundedPathCollector {
    limit: usize,
    paths: BTreeSet<RecentPath>,
}

impl BoundedPathCollector {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            paths: BTreeSet::new(),
        }
    }

    pub(crate) fn push(&mut self, path: PathBuf) {
        let Ok(metadata) = std::fs::metadata(&path) else {
            return;
        };
        self.push_with_modified(path, metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    }

    fn push_with_modified(&mut self, path: PathBuf, modified: SystemTime) {
        if self.limit == 0 {
            return;
        }
        self.paths.insert(RecentPath { modified, path });
        if self.paths.len() > self.limit {
            self.paths.pop_first();
        }
    }

    pub(crate) fn into_paths_newest_first(self) -> Vec<PathBuf> {
        self.paths
            .into_iter()
            .rev()
            .map(|candidate| candidate.path)
            .collect()
    }
}

/// Iteratively scans roots with lazy directory iterators and bounded nesting depth.
pub(crate) fn discover_recent_files(
    roots: impl IntoIterator<Item = PathBuf>,
    limit: usize,
    matches: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut files = BoundedPathCollector::new(limit);
    let mut depth_limit_reached = false;
    for root in roots {
        let Ok(root_entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut directory_stack = vec![root_entries];
        while let Some(entries) = directory_stack.last_mut() {
            let Some(entry) = entries.next() else {
                directory_stack.pop();
                continue;
            };
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if directory_stack.len() >= MAX_DISCOVERY_DIRECTORY_DEPTH {
                    depth_limit_reached = true;
                } else if let Ok(entries) = std::fs::read_dir(path) {
                    directory_stack.push(entries);
                }
            } else if file_type.is_file() && matches(&path) {
                files.push(path);
            }
        }
    }

    if depth_limit_reached {
        tracing::warn!(
            depth_limit = MAX_DISCOVERY_DIRECTORY_DEPTH,
            "transcript discovery depth limit reached; deeply nested paths were skipped"
        );
    }

    files.into_paths_newest_first()
}

/// Strategy for discovering new/updated sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepStrategy {
    /// Periodic polling at the given interval
    Periodic(Duration),
    /// File system watcher (not implemented yet)
    FsWatcher,
    /// HTTP API polling (not implemented yet)
    HttpApi,
    /// No sweep support for this agent
    None,
}

/// A session discovered during a sweep.
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    pub session_id: String,
    pub tool: String,
    pub stream_path: PathBuf,
    pub external_session_id: String,
    pub external_parent_session_id: Option<String>,
}

/// Transcript file format enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFormat {
    ClaudeJsonl,
    CursorJsonl,
    DroidJsonl,
    CopilotSessionJson,
    CopilotEventStreamJsonl,
    GeminiJsonl,
    ContinueJson,
    WindsurfJsonl,
    CodexJsonl,
    AmpThreadJson,
    OpenCodeSqlite,
    PiJsonl,
    CopilotOtelSqlite,
}

impl std::fmt::Display for StreamFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeJsonl => write!(f, "ClaudeJsonl"),
            Self::CursorJsonl => write!(f, "CursorJsonl"),
            Self::DroidJsonl => write!(f, "DroidJsonl"),
            Self::CopilotSessionJson => write!(f, "CopilotSessionJson"),
            Self::CopilotEventStreamJsonl => write!(f, "CopilotEventStreamJsonl"),
            Self::GeminiJsonl => write!(f, "GeminiJsonl"),
            Self::ContinueJson => write!(f, "ContinueJson"),
            Self::WindsurfJsonl => write!(f, "WindsurfJsonl"),
            Self::CodexJsonl => write!(f, "CodexJsonl"),
            Self::AmpThreadJson => write!(f, "AmpThreadJson"),
            Self::OpenCodeSqlite => write!(f, "OpenCodeSqlite"),
            Self::PiJsonl => write!(f, "PiJsonl"),
            Self::CopilotOtelSqlite => write!(f, "CopilotOtelSqlite"),
        }
    }
}

impl StreamFormat {
    pub fn watermark_type(self) -> super::watermark::WatermarkType {
        use super::watermark::WatermarkType;
        match self {
            Self::ClaudeJsonl
            | Self::CursorJsonl
            | Self::GeminiJsonl
            | Self::WindsurfJsonl
            | Self::CodexJsonl
            | Self::PiJsonl
            | Self::CopilotEventStreamJsonl => WatermarkType::ByteOffset,
            Self::DroidJsonl => WatermarkType::Hybrid,
            Self::CopilotSessionJson | Self::ContinueJson | Self::AmpThreadJson => {
                WatermarkType::RecordIndex
            }
            Self::OpenCodeSqlite => WatermarkType::Timestamp,
            Self::CopilotOtelSqlite => WatermarkType::TimestampCursor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_path_collector_keeps_newest_paths() {
        let mut paths = BoundedPathCollector::new(2);
        for seconds in 1..=4 {
            paths.push_with_modified(
                PathBuf::from(format!("session-{seconds}.jsonl")),
                SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
            );
        }

        assert_eq!(
            paths.into_paths_newest_first(),
            [
                PathBuf::from("session-4.jsonl"),
                PathBuf::from("session-3.jsonl")
            ]
        );
    }

    #[test]
    fn recursive_discovery_bounds_results_and_prefers_newest_files() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let now = filetime::FileTime::now();
        for seconds_old in 1..=4 {
            let path = nested.join(format!("session-{seconds_old}.jsonl"));
            std::fs::write(&path, "{}\n").unwrap();
            filetime::set_file_mtime(
                &path,
                filetime::FileTime::from_unix_time(now.unix_seconds() - seconds_old, 0),
            )
            .unwrap();
        }

        let paths = discover_recent_files([temp.path().to_path_buf()], 2, |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });

        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("session-1.jsonl"));
        assert!(paths[1].ends_with("session-2.jsonl"));
    }
}
