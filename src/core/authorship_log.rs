use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const AUTHORSHIP_LOG_VERSION: &str = "authorship/3.0.0";

#[cfg(all(debug_assertions, test))]
pub const GIT_AI_VERSION: &str = "development";

#[cfg(all(debug_assertions, not(test)))]
pub const GIT_AI_VERSION: &str = concat!("development:", env!("CARGO_PKG_VERSION"));

#[cfg(not(debug_assertions))]
pub const GIT_AI_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The complete authorship log stored as a git note on each commit.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorshipLog {
    pub attestations: Vec<FileAttestation>,
    pub metadata: Metadata,
}

/// Per-file attestation data mapping hashes to line ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttestation {
    pub file_path: String,
    pub entries: Vec<AttestationEntry>,
}

/// A single attestation entry: a short hash and the line ranges it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationEntry {
    pub hash: String,
    pub line_ranges: Vec<LineRange>,
}

/// A line range: either a single line or an inclusive start-end range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LineRange {
    Single(u32),
    Range(u32, u32),
}

/// Metadata section serialized as JSON below the `---` divider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ai_version: Option<String>,
    pub base_commit_sha: String,
    pub prompts: BTreeMap<String, PromptRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, SessionRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub humans: BTreeMap<String, HumanRecord>,
}

/// Agent identity: tool name, session id within that tool, and model used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentId {
    pub tool: String,
    pub id: String,
    pub model: String,
}

/// Record for an AI prompt session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptRecord {
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_url: Option<String>,
    #[serde(default)]
    pub total_additions: u32,
    #[serde(default)]
    pub total_deletions: u32,
    #[serde(default)]
    pub accepted_lines: u32,
    #[serde(default)]
    pub overriden_lines: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<serde_json::Value>,
}

/// Record for a lightweight session (no per-prompt stats).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_attributes: Option<serde_json::Value>,
}

/// Record for a known human author attested by an IDE extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanRecord {
    pub author: String,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Error {
    MissingDivider,
    InvalidAttestationEntry(String),
    OrphanedEntry,
    ParseInt(std::num::ParseIntError),
    Json(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MissingDivider => write!(f, "missing '---' divider in authorship log"),
            Error::InvalidAttestationEntry(line) => {
                write!(f, "invalid attestation entry: {}", line)
            }
            Error::OrphanedEntry => write!(f, "attestation entry without a preceding file path"),
            Error::ParseInt(e) => write!(f, "line range parse error: {}", e),
            Error::Json(e) => write!(f, "metadata JSON error: {}", e),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Error::ParseInt(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

// ---------------------------------------------------------------------------
// LineRange implementation
// ---------------------------------------------------------------------------

impl LineRange {
    /// Compress a sorted slice of line numbers into minimal ranges.
    pub fn compress_lines(lines: &[u32]) -> Vec<LineRange> {
        if lines.is_empty() {
            return vec![];
        }

        let mut ranges = Vec::new();
        let mut start = lines[0];
        let mut end = lines[0];

        for &line in &lines[1..] {
            if line == end + 1 {
                end = line;
            } else {
                if start == end {
                    ranges.push(LineRange::Single(start));
                } else {
                    ranges.push(LineRange::Range(start, end));
                }
                start = line;
                end = line;
            }
        }

        if start == end {
            ranges.push(LineRange::Single(start));
        } else {
            ranges.push(LineRange::Range(start, end));
        }

        ranges
    }

    /// Whether this range contains the given line number.
    pub fn contains(&self, line: u32) -> bool {
        match self {
            LineRange::Single(l) => *l == line,
            LineRange::Range(s, e) => line >= *s && line <= *e,
        }
    }

    /// Expand this range into individual line numbers.
    pub fn expand(&self) -> Vec<u32> {
        match self {
            LineRange::Single(l) => vec![*l],
            LineRange::Range(s, e) => (*s..=*e).collect(),
        }
    }

    /// Count the number of lines in this range.
    pub fn line_count(&self) -> u32 {
        match self {
            LineRange::Single(_) => 1,
            LineRange::Range(s, e) => e - s + 1,
        }
    }
}

impl fmt::Display for LineRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineRange::Single(l) => write!(f, "{}", l),
            LineRange::Range(s, e) => write!(f, "{}-{}", s, e),
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata constructors
// ---------------------------------------------------------------------------

impl Metadata {
    pub fn new(base_commit_sha: String) -> Self {
        Self {
            schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
            git_ai_version: Some(GIT_AI_VERSION.to_string()),
            base_commit_sha,
            prompts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            humans: BTreeMap::new(),
        }
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self::new(String::new())
    }
}

// ---------------------------------------------------------------------------
// AuthorshipLog serialization / deserialization
// ---------------------------------------------------------------------------

impl AuthorshipLog {
    pub fn new(metadata: Metadata) -> Self {
        Self {
            attestations: Vec::new(),
            metadata,
        }
    }

    /// Serialize to the text+JSON format stored in git notes.
    pub fn serialize_to_string(&self) -> String {
        let mut out = String::new();

        for file in &self.attestations {
            // Quote paths containing whitespace
            if file.file_path.contains(' ')
                || file.file_path.contains('\t')
                || file.file_path.contains('\n')
            {
                out.push('"');
                out.push_str(&file.file_path);
                out.push('"');
            } else {
                out.push_str(&file.file_path);
            }
            out.push('\n');

            for entry in &file.entries {
                out.push_str("  ");
                out.push_str(&entry.hash);
                out.push(' ');
                out.push_str(&format_line_ranges(&entry.line_ranges));
                out.push('\n');
            }
        }

        out.push_str("---\n");

        // Unwrap is safe: Metadata derives Serialize and contains no non-string map keys.
        let json = serde_json::to_string_pretty(&self.metadata).unwrap();
        out.push_str(&json);

        out
    }

    /// Parse the text+JSON format back into an AuthorshipLog.
    pub fn deserialize_from_string(s: &str) -> Result<Self, Error> {
        let lines: Vec<&str> = s.lines().collect();

        let divider = lines
            .iter()
            .position(|&l| l == "---")
            .ok_or(Error::MissingDivider)?;

        let attestations = parse_attestations(&lines[..divider])?;

        let json_text: String = lines[divider + 1..].join("\n");
        let metadata: Metadata = serde_json::from_str(&json_text)?;

        Ok(Self {
            attestations,
            metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// Hash generation utilities
// ---------------------------------------------------------------------------

/// Generate a 16-char hex hash: SHA256("tool:id")[0..16].
/// Used as the key in `metadata.prompts`.
pub fn generate_short_hash(tool: &str, id: &str) -> String {
    let input = format!("{}:{}", tool, id);
    let digest = Sha256::digest(input.as_bytes());
    format!("{:x}", digest)[..16].to_string()
}

/// Generate a session ID: "s_" + SHA256("tool:id")[0..14] = 16 chars total.
/// Used as the key in `metadata.sessions`.
pub fn generate_session_id(tool: &str, id: &str) -> String {
    let input = format!("{}:{}", tool, id);
    let digest = Sha256::digest(input.as_bytes());
    format!("s_{}", &format!("{:x}", digest)[..14])
}

/// Generate a human hash: "h_" + SHA256(author)[0..14] = 16 chars total.
/// Used as the key in `metadata.humans`.
pub fn generate_human_hash(author: &str) -> String {
    let digest = Sha256::digest(author.as_bytes());
    format!("h_{}", &format!("{:x}", digest)[..14])
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Format line ranges as comma-separated "1,2,5-10,20-25".
fn format_line_ranges(ranges: &[LineRange]) -> String {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|r| match r {
        LineRange::Single(l) => *l,
        LineRange::Range(s, _) => *s,
    });

    sorted
        .iter()
        .map(|r| match r {
            LineRange::Single(l) => l.to_string(),
            LineRange::Range(s, e) => format!("{}-{}", s, e),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a line-range string like "1,2,19-222".
fn parse_line_ranges(s: &str) -> Result<Vec<LineRange>, Error> {
    let mut ranges = Vec::new();
    for part in s.split(',') {
        if part.is_empty() {
            continue;
        }
        if let Some(dash) = part.find('-') {
            let start: u32 = part[..dash].parse()?;
            let end: u32 = part[dash + 1..].parse()?;
            ranges.push(LineRange::Range(start, end));
        } else {
            let line: u32 = part.parse()?;
            ranges.push(LineRange::Single(line));
        }
    }
    Ok(ranges)
}

/// Parse the attestation section (lines before the `---` divider).
fn parse_attestations(lines: &[&str]) -> Result<Vec<FileAttestation>, Error> {
    let mut result: Vec<FileAttestation> = Vec::new();
    let mut current: Option<FileAttestation> = None;

    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }

        if let Some(entry_line) = line.strip_prefix("  ") {
            // Indented: attestation entry
            let space = entry_line
                .find(' ')
                .ok_or_else(|| Error::InvalidAttestationEntry(entry_line.to_string()))?;
            let hash = entry_line[..space].to_string();
            let ranges = parse_line_ranges(&entry_line[space + 1..])?;

            let file = current.as_mut().ok_or(Error::OrphanedEntry)?;
            file.entries.push(AttestationEntry {
                hash,
                line_ranges: ranges,
            });
        } else {
            // Not indented: file path
            if let Some(file) = current.take() {
                if !file.entries.is_empty() {
                    result.push(file);
                }
            }

            let path = if line.starts_with('"') && line.ends_with('"') {
                line[1..line.len() - 1].to_string()
            } else {
                line.to_string()
            };

            current = Some(FileAttestation {
                file_path: path,
                entries: Vec::new(),
            });
        }
    }

    if let Some(file) = current {
        if !file.entries.is_empty() {
            result.push(file);
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_lines_empty() {
        assert_eq!(LineRange::compress_lines(&[]), Vec::<LineRange>::new());
    }

    #[test]
    fn test_compress_lines_single() {
        assert_eq!(LineRange::compress_lines(&[5]), vec![LineRange::Single(5)]);
    }

    #[test]
    fn test_compress_lines_consecutive() {
        assert_eq!(
            LineRange::compress_lines(&[1, 2, 3, 4, 5]),
            vec![LineRange::Range(1, 5)]
        );
    }

    #[test]
    fn test_compress_lines_mixed() {
        assert_eq!(
            LineRange::compress_lines(&[1, 2, 5, 10, 11, 12, 20]),
            vec![
                LineRange::Range(1, 2),
                LineRange::Single(5),
                LineRange::Range(10, 12),
                LineRange::Single(20),
            ]
        );
    }

    #[test]
    fn test_generate_short_hash_length_and_determinism() {
        let h = generate_short_hash("cursor", "session_123");
        assert_eq!(h.len(), 16);
        assert_eq!(h, generate_short_hash("cursor", "session_123"));
        assert_ne!(h, generate_short_hash("cursor", "session_456"));
    }

    #[test]
    fn test_generate_session_id_format() {
        let id = generate_session_id("cursor", "session_123");
        assert!(id.starts_with("s_"));
        assert_eq!(id.len(), 16);
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_human_hash_format() {
        let h = generate_human_hash("Alice Smith <alice@example.com>");
        assert!(h.starts_with("h_"));
        assert_eq!(h.len(), 16);
        assert!(h[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_session_id_shares_hash_base_with_short_hash() {
        let session = generate_session_id("cursor", "session_123");
        let prompt = generate_short_hash("cursor", "session_123");
        // The hex portion after "s_" is a prefix of the full 16-char prompt hash
        assert_eq!(&session[2..], &prompt[..14]);
    }

    #[test]
    fn test_roundtrip_serialize_deserialize() {
        let mut log = AuthorshipLog::new(Metadata::new("abc123def".to_string()));

        log.attestations.push(FileAttestation {
            file_path: "src/main.rs".to_string(),
            entries: vec![
                AttestationEntry {
                    hash: "abcdef1234567890".to_string(),
                    line_ranges: vec![
                        LineRange::Single(1),
                        LineRange::Range(3, 10),
                        LineRange::Single(15),
                    ],
                },
                AttestationEntry {
                    hash: "h_12345678901234".to_string(),
                    line_ranges: vec![LineRange::Range(20, 30)],
                },
            ],
        });

        log.attestations.push(FileAttestation {
            file_path: "path with spaces/file.rs".to_string(),
            entries: vec![AttestationEntry {
                hash: "fedcba0987654321".to_string(),
                line_ranges: vec![LineRange::Range(1, 5)],
            }],
        });

        log.metadata.prompts.insert(
            "abcdef1234567890".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess_1".to_string(),
                    model: "claude-3-sonnet".to_string(),
                },
                human_author: Some("dev@example.com".to_string()),
                messages_url: None,
                total_additions: 10,
                total_deletions: 2,
                accepted_lines: 8,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );

        log.metadata.humans.insert(
            "h_12345678901234".to_string(),
            HumanRecord {
                author: "Alice <alice@example.com>".to_string(),
            },
        );

        let serialized = log.serialize_to_string();
        let deserialized = AuthorshipLog::deserialize_from_string(&serialized).unwrap();

        assert_eq!(log.attestations, deserialized.attestations);
        assert_eq!(log.metadata, deserialized.metadata);
    }

    #[test]
    fn test_no_attestations_roundtrip() {
        let log = AuthorshipLog::new(Metadata::new("deadbeef".to_string()));
        let serialized = log.serialize_to_string();
        let deserialized = AuthorshipLog::deserialize_from_string(&serialized).unwrap();
        assert!(deserialized.attestations.is_empty());
        assert_eq!(deserialized.metadata.base_commit_sha, "deadbeef");
    }

    #[test]
    fn test_line_range_contains() {
        assert!(LineRange::Single(5).contains(5));
        assert!(!LineRange::Single(5).contains(6));
        assert!(LineRange::Range(3, 7).contains(3));
        assert!(LineRange::Range(3, 7).contains(5));
        assert!(LineRange::Range(3, 7).contains(7));
        assert!(!LineRange::Range(3, 7).contains(2));
        assert!(!LineRange::Range(3, 7).contains(8));
    }

    #[test]
    fn test_format_and_parse_line_ranges() {
        let ranges = vec![
            LineRange::Range(19, 222),
            LineRange::Single(1),
            LineRange::Single(2),
        ];
        let formatted = format_line_ranges(&ranges);
        assert_eq!(formatted, "1,2,19-222");

        let parsed = parse_line_ranges(&formatted).unwrap();
        assert_eq!(
            parsed,
            vec![
                LineRange::Single(1),
                LineRange::Single(2),
                LineRange::Range(19, 222),
            ]
        );
    }
}
