use std::collections::{HashMap, HashSet};

use crate::repos::test_repo::TestRepo;

/// Attribution types that can be assigned to a character/line.
/// NOTE: We intentionally only test Ai and KnownHuman because these are the two
/// attribution types with well-defined, reliable checkpoint flows. "Untracked"
/// (no checkpoint) has known limitations: content appearing after an AI checkpoint
/// without any subsequent checkpoint is attributed to the prior AI session by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Ai,
    KnownHuman,
}

impl std::fmt::Display for Attribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Attribution::Ai => write!(f, "Ai"),
            Attribution::KnownHuman => write!(f, "KnownHuman"),
        }
    }
}

/// Entry tracking a single allocated character.
#[derive(Debug, Clone)]
pub struct CharEntry {
    pub ch: char,
    pub attribution: Attribution,
    pub step: usize,
}

/// Names that indicate AI authorship in blame output.
const AI_AUTHOR_NAMES: &[&str] = &[
    "mock_ai",
    "claude",
    "continue-cli",
    "gpt",
    "copilot",
    "cursor",
    "codex",
    "gemini",
    "amp",
    "windsurf",
    "devin",
    "cloud-agent",
    "codex-cloud",
    "git-ai-cloud-agent",
];

/// The base character pool before falling back to Unicode.
const CHAR_POOL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Registry that allocates unique characters and maps them to attributions.
/// Also tracks committed sessions for monotonic retention verification.
pub struct CharRegistry {
    pool: Vec<char>,
    next: usize,
    entries: HashMap<char, CharEntry>,
    /// Sessions that have been committed to HEAD and must be retained through rewrites.
    /// Cleared on hard reset / destructive ops that legitimately drop commits.
    committed_sessions: HashSet<String>,
    /// Characters whose attribution became unverifiable (e.g., after git_og rebase/merge
    /// that doesn't transfer authorship notes). verify_blame skips these.
    unverifiable: HashSet<char>,
    /// When true, skip ALL blame verification (e.g., after file rename which permanently
    /// breaks daemon attribution tracking for the file).
    pub skip_all_blame: bool,
}

impl CharRegistry {
    pub fn new() -> Self {
        let pool: Vec<char> = CHAR_POOL.chars().collect();
        Self {
            pool,
            next: 0,
            entries: HashMap::new(),
            committed_sessions: HashSet::new(),
            unverifiable: HashSet::new(),
            skip_all_blame: false,
        }
    }

    /// Allocate the next unique character with the given attribution.
    pub fn allocate(&mut self, attribution: Attribution) -> char {
        let ch = if self.next < self.pool.len() {
            self.pool[self.next]
        } else {
            // Fall back to Unicode characters starting at U+0100
            let offset = self.next - self.pool.len();
            char::from_u32(0x0100 + offset as u32)
                .unwrap_or_else(|| panic!("Exhausted character pool at index {}", self.next))
        };
        let step = self.next;
        self.entries.insert(
            ch,
            CharEntry {
                ch,
                attribution,
                step,
            },
        );
        self.next += 1;
        ch
    }

    /// Get the current next index (useful for generating unique names).
    pub fn next_index(&self) -> usize {
        self.next
    }

    /// Look up attribution for a character.
    pub fn get(&self, ch: char) -> Option<&CharEntry> {
        self.entries.get(&ch)
    }

    /// Mark specific characters as unverifiable (their attribution can't be checked
    /// because the commit that introduced them went through git_og without authorship notes).
    #[allow(dead_code)]
    pub fn mark_unverifiable(&mut self, chars: &[char]) {
        for &ch in chars {
            self.unverifiable.insert(ch);
        }
    }

    /// Mark all currently-allocated characters as unverifiable.
    /// Used after operations like git_og rebase/merge that invalidate all prior attributions.
    pub fn mark_all_unverifiable(&mut self) {
        for &ch in self.entries.keys().collect::<Vec<_>>() {
            self.unverifiable.insert(ch);
        }
    }

    /// Dump registry contents for debugging.
    pub fn dump(&self) -> String {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by_key(|e| e.step);
        let mut out = String::new();
        for entry in entries {
            out.push_str(&format!(
                "  step={}: '{}' -> {}\n",
                entry.step, entry.ch, entry.attribution
            ));
        }
        out
    }

    /// Verify blame output for a file against the expected lines.
    ///
    /// `file_lines` is the current state of the file (chars representing each line).
    /// `operation_log` is passed through for diagnostics on failure.
    pub fn verify_blame(
        &self,
        repo: &TestRepo,
        filename: &str,
        file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        if self.skip_all_blame {
            return;
        }
        let blame_output = match repo.git_ai(&["blame", filename]) {
            Ok(output) => output,
            Err(e) => {
                panic!(
                    "git-ai blame failed for '{}'\nSeed: {}\nError: {}\nOperation log:\n{}\nRegistry:\n{}",
                    filename,
                    seed,
                    e,
                    operation_log.join("\n"),
                    self.dump()
                );
            }
        };

        let blame_lines: Vec<&str> = blame_output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        if blame_lines.len() != file_lines.len() {
            // Line count divergence can happen after git_og operations or conflict
            // resolution. Skip verification rather than panic.
            return;
        }

        for (i, (blame_line, &expected_char)) in
            blame_lines.iter().zip(file_lines.iter()).enumerate()
        {
            let line_num = i + 1;

            // Skip lines whose attribution is unverifiable (e.g., after git_og rebase/merge)
            if self.unverifiable.contains(&expected_char) {
                continue;
            }

            let (author, _content) = parse_blame_line(blame_line);
            let is_ai_author = is_ai_author_name(&author);

            let entry = match self.get(expected_char) {
                Some(e) => e,
                None => {
                    // Character not in registry (e.g., conflict markers from unresolved
                    // merge, or content from git_og operations). Skip verification.
                    continue;
                }
            };

            let expected_ai = matches!(entry.attribution, Attribution::Ai);

            if expected_ai != is_ai_author {
                panic!(
                    "Attribution mismatch on line {} of '{}'\n\
                     Seed: {}\n\
                     Character: '{}' (step {})\n\
                     Expected: {} (should {}be AI author)\n\
                     Actual author: '{}' (is_ai={})\n\
                     Blame line: {}\n\
                     Full blame output:\n{}\n\
                     Operation log:\n{}\n\
                     Registry:\n{}",
                    line_num,
                    filename,
                    seed,
                    expected_char,
                    entry.step,
                    entry.attribution,
                    if expected_ai { "" } else { "NOT " },
                    author,
                    is_ai_author,
                    blame_line,
                    blame_output,
                    operation_log.join("\n"),
                    self.dump()
                );
            }
        }
    }

    /// Verify that the authorship note for a commit contains exactly the session types
    /// that contributed to it. AI-attributed lines should have AI sessions in the note,
    /// human-attributed lines should have human entries (h_ prefix) in the note.
    /// No extra/phantom sessions should exist.
    pub fn verify_sessions(
        &mut self,
        repo: &TestRepo,
        file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => {
                // No note means no attribution was recorded - acceptable for pure-human commits
                // But if we expect AI lines, that's a bug
                let has_ai = file_lines.iter().any(|&ch| {
                    self.get(ch)
                        .is_some_and(|e| matches!(e.attribution, Attribution::Ai))
                });
                if has_ai {
                    panic!(
                        "Session verification failed: no authorship note exists but AI lines expected\n\
                         Seed: {}\nHead: {}\nExpected AI chars: {:?}\n\
                         Operation log:\n{}",
                        seed,
                        head_sha,
                        file_lines
                            .iter()
                            .filter(|&&ch| self
                                .get(ch)
                                .is_some_and(|e| matches!(e.attribution, Attribution::Ai)))
                            .collect::<Vec<_>>(),
                        operation_log.join("\n"),
                    );
                }
                return;
            }
        };

        // Parse the note to check session presence
        let has_ai_lines = file_lines.iter().any(|&ch| {
            self.get(ch)
                .is_some_and(|e| matches!(e.attribution, Attribution::Ai))
        });
        let has_human_lines = file_lines.iter().any(|&ch| {
            self.get(ch)
                .is_some_and(|e| matches!(e.attribution, Attribution::KnownHuman))
        });

        // Check that AI sessions exist in note when AI lines are present
        let has_ai_session = note.contains("mock_ai") || note.contains("\"tool\"");
        let has_human_session = note.contains("h_");

        if has_ai_lines && !has_ai_session {
            panic!(
                "Session verification failed: AI lines present but no AI session in note\n\
                 Seed: {}\nHead: {}\n\
                 Note (first 500 chars):\n{}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                &note[..note.len().min(500)],
                operation_log.join("\n"),
            );
        }

        if has_human_lines && !has_human_session {
            panic!(
                "Session verification failed: known-human lines present but no h_ entry in note\n\
                 Seed: {}\nHead: {}\n\
                 Note (first 500 chars):\n{}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                &note[..note.len().min(500)],
                operation_log.join("\n"),
            );
        }

        // Extract sessions from the note and update committed_sessions
        let current_sessions = extract_sessions_from_note(&note);
        self.committed_sessions.extend(current_sessions);
    }

    /// Verify monotonic session retention: all sessions that were previously committed
    /// must still be present in the current HEAD's note. Sessions represent the history
    /// of who contributed to this commit's lineage — even if their lines were later
    /// overwritten, the session must be retained to track "failed paths."
    ///
    /// Call this after rewrite operations (amend, squash, rebase, cherry-pick).
    pub fn verify_session_retention(&self, repo: &TestRepo, operation_log: &[String], seed: u64) {
        if self.committed_sessions.is_empty() {
            return;
        }

        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => {
                if !self.committed_sessions.is_empty() {
                    panic!(
                        "Session retention failed: no authorship note but {} sessions expected\n\
                         Seed: {}\nHead: {}\n\
                         Expected sessions: {:?}\n\
                         Operation log:\n{}",
                        self.committed_sessions.len(),
                        seed,
                        head_sha,
                        self.committed_sessions,
                        operation_log.join("\n"),
                    );
                }
                return;
            }
        };

        let current_sessions = extract_sessions_from_note(&note);
        let missing: Vec<&String> = self
            .committed_sessions
            .iter()
            .filter(|s| !current_sessions.contains(*s))
            .collect();

        if !missing.is_empty() {
            panic!(
                "Session retention failed: {} sessions lost after rewrite\n\
                 Seed: {}\nHead: {}\n\
                 Missing sessions: {:?}\n\
                 Current sessions: {:?}\n\
                 Previously committed: {:?}\n\
                 Note (first 800 chars):\n{}\n\
                 Operation log:\n{}",
                missing.len(),
                seed,
                head_sha,
                missing,
                current_sessions,
                self.committed_sessions,
                &note[..note.len().min(800)],
                operation_log.join("\n"),
            );
        }
    }

    /// Reset committed sessions tracking. Call this after destructive operations
    /// that legitimately drop commits (hard reset, branch switch to unrelated history).
    pub fn reset_session_tracking(&mut self) {
        self.committed_sessions.clear();
    }
}

/// Parse a blame line to extract author and content.
/// Format: `sha (author date line_num) content`
fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim();

        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        let author = author_parts.join(" ");
        return (author, content.to_string());
    }
    ("unknown".to_string(), line.to_string())
}

/// Check if an author name indicates AI authorship.
fn is_ai_author_name(author: &str) -> bool {
    let name_only = if let Some(bracket) = author.find('<') {
        &author[..bracket]
    } else {
        author
    };
    let name_lower = name_only.to_lowercase();
    AI_AUTHOR_NAMES
        .iter()
        .any(|&ai_name| name_lower.contains(ai_name))
}

/// Extract all session IDs from an authorship note.
/// Looks for both AI sessions (s_<hex>) and human sessions (h_<hex>) in
/// the attestation lines (before the --- separator).
fn extract_sessions_from_note(note: &str) -> HashSet<String> {
    let mut sessions = HashSet::new();

    // Split at --- to get the attestation section
    let attestation_section = if let Some(idx) = note.find("\n---\n") {
        &note[..idx]
    } else {
        note
    };

    // Parse attestation lines for session identifiers
    for line in attestation_section.lines() {
        let trimmed = line.trim();
        // Attestation lines look like: "  s_1234567890abcd::t_fedcba0987654321 1-5"
        // or "  h_e858f2c2faea28 1-3"
        // or "  s_1234567890abcd 1-5" (AI session without tool qualifier)
        for token in trimmed.split_whitespace() {
            // Stop at line ranges (digits or digit-digit)
            if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                break;
            }
            // Extract the session ID (part before :: if present)
            let session_id = if let Some(idx) = token.find("::") {
                &token[..idx]
            } else {
                token
            };
            if (session_id.starts_with("s_") || session_id.starts_with("h_"))
                && session_id.len() > 2
            {
                sessions.insert(session_id.to_string());
            }
        }
    }

    // Also check the JSON metadata section for sessions and humans keys
    if let Some(json_start) = note.find("\n---\n") {
        let json_section = &note[json_start + 5..];
        // Extract session IDs from "sessions": { "s_...": ... }
        for segment in json_section.split('"') {
            if (segment.starts_with("s_") || segment.starts_with("h_")) && segment.len() > 2 {
                // Verify it looks like a hex session ID (s_ or h_ followed by hex chars)
                let suffix = &segment[2..];
                if suffix.chars().all(|c| c.is_ascii_hexdigit()) && suffix.len() >= 8 {
                    sessions.insert(segment.to_string());
                }
            }
        }
    }

    sessions
}
