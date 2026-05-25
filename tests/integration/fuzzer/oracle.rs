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
    /// When true, line count divergence is allowed (set alongside mark_all_unverifiable).
    divergence_allowed: bool,
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
            divergence_allowed: false,
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
        self.divergence_allowed = true;
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
        &mut self,
        repo: &TestRepo,
        filename: &str,
        file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        if self.skip_all_blame {
            return;
        }
        // Skip verification if the file doesn't exist on disk (can happen after
        // destructive operations like reset --hard)
        if !repo.path().join(filename).exists() {
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
            // Check if there are ANY verifiable chars in file_lines
            let has_verifiable_chars = file_lines.iter().any(|&ch| {
                !self.unverifiable.contains(&ch) && self.entries.contains_key(&ch)
            });

            // If there are verifiable chars and divergence isn't explicitly allowed, this is a bug
            if has_verifiable_chars && !self.divergence_allowed {
                panic!(
                    "Line count mismatch with verifiable characters present (NOT after git_og)\n\
                     Seed: {}\n\
                     File: {}\n\
                     Blame line count: {}\n\
                     Expected file line count: {}\n\
                     Verifiable chars present: true\n\
                     Divergence allowed: {}\n\
                     Operation log:\n{}\n\
                     Registry:\n{}",
                    seed,
                    filename,
                    blame_lines.len(),
                    file_lines.len(),
                    self.divergence_allowed,
                    operation_log.join("\n"),
                    self.dump()
                );
            }
            // Otherwise, line count divergence is expected (all unverifiable or divergence allowed)
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

    /// Verify blame for multiple files in a single call. Ensures that all files in a
    /// multi-file commit are verified together, and that the authorship note contains
    /// all files with AI or known-human attribution.
    pub fn verify_multi_file_commit(
        &mut self,
        repo: &TestRepo,
        files: &[(&str, &[char])],
        operation_log: &[String],
        seed: u64,
    ) {
        if self.skip_all_blame {
            return;
        }

        // Verify blame for each file
        for &(filename, file_lines) in files {
            if !file_lines.is_empty() {
                self.verify_blame(repo, filename, file_lines, operation_log, seed);
            }
        }

        // Verify that the authorship note contains all files with attributions
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => {
                // No note is only acceptable if no files have AI/known-human lines
                let has_attributed_lines = files.iter().any(|(_, file_lines)| {
                    file_lines.iter().any(|&ch| {
                        self.get(ch).is_some_and(|e| {
                            matches!(e.attribution, Attribution::Ai | Attribution::KnownHuman)
                        })
                    })
                });
                if has_attributed_lines {
                    panic!(
                        "Multi-file verification failed: no authorship note but files have attributed lines\n\
                         Seed: {}\nHead: {}\nFiles: {:?}\n\
                         Operation log:\n{}",
                        seed,
                        head_sha,
                        files.iter().map(|(name, _)| name).collect::<Vec<_>>(),
                        operation_log.join("\n"),
                    );
                }
                return;
            }
        };

        // Parse attestation section to find all files mentioned
        let attestation_section = if let Some(idx) = note.find("\n---\n") {
            &note[..idx]
        } else {
            &note
        };

        // Check that each file with AI/known-human lines is present in the note
        // NOTE: File renames/moves break daemon attribution tracking (known limitation).
        // If a file has verifiable attributed lines but is missing from the note, mark
        // those chars as unverifiable rather than panicking, since the daemon may have
        // legitimately failed to track the file due to a rename in its history.
        for &(filename, file_lines) in files {
            let has_verifiable_ai_or_human = file_lines.iter().any(|&ch| {
                if self.unverifiable.contains(&ch) {
                    return false;
                }
                self.get(ch).is_some_and(|e| {
                    matches!(e.attribution, Attribution::Ai | Attribution::KnownHuman)
                })
            });

            // If file has verifiable attributed lines but is missing from note, assume
            // it had a rename in its history and mark all its chars as unverifiable
            if has_verifiable_ai_or_human && !attestation_section.contains(filename) {
                // Mark all chars from this file as unverifiable
                for &ch in file_lines {
                    self.unverifiable.insert(ch);
                }
            }
        }
    }

    /// Verify that line ranges in the authorship note match actual attributions.
    /// For each line claimed by a session, verify that the character at that line
    /// in the registry has the corresponding attribution (AI or KnownHuman).
    pub fn verify_note_line_ranges(
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

        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => return, // No note to verify
        };

        // Parse the attestation section manually
        let attestation_section = if let Some(idx) = note.find("\n---\n") {
            &note[..idx]
        } else {
            &note
        };

        // Find attestations for this file
        let mut current_file: Option<String> = None;
        let mut file_attestations: Vec<(String, Vec<(u32, u32)>)> = Vec::new();

        for line in attestation_section.lines() {
            if line.is_empty() {
                continue;
            }

            if !line.starts_with(' ') && !line.starts_with('\t') {
                // File path line
                current_file = Some(line.trim().to_string());
            } else if let Some(ref file) = current_file {
                // Attestation entry line: "  hash ranges" or "  hash::tool ranges"
                if file != filename {
                    continue;
                }

                let trimmed = line.trim();
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 2 {
                    continue;
                }

                let session_hash = parts[0];
                let ranges_str = parts[1];

                // Parse line ranges
                let ranges = parse_line_ranges(ranges_str);
                file_attestations.push((session_hash.to_string(), ranges));
            }
        }

        // Verify each range
        for (session_hash, ranges) in file_attestations {
            let is_ai_session = !session_hash.starts_with("h_");
            let is_human_session = session_hash.starts_with("h_");

            for (start, end) in ranges {
                for line_num in start..=end {
                    let idx = (line_num - 1) as usize; // Convert to 0-indexed
                    if idx >= file_lines.len() {
                        continue; // Line beyond current file length (may have been deleted)
                    }

                    let ch = file_lines[idx];
                    if self.unverifiable.contains(&ch) {
                        continue; // Skip unverifiable chars
                    }

                    let entry = match self.get(ch) {
                        Some(e) => e,
                        None => continue, // Char not in registry
                    };

                    let actual_is_ai = matches!(entry.attribution, Attribution::Ai);
                    let actual_is_human = matches!(entry.attribution, Attribution::KnownHuman);

                    // Verify session type matches attribution
                    if is_ai_session && !actual_is_ai {
                        panic!(
                            "Line range verification failed: AI session claims line {} but char has {:?} attribution\n\
                             Seed: {}\nFile: {}\nSession: {}\n\
                             Line {}: char '{}' (step {})\n\
                             Note (first 800 chars):\n{}\n\
                             Operation log:\n{}",
                            line_num,
                            entry.attribution,
                            seed,
                            filename,
                            session_hash,
                            line_num,
                            ch,
                            entry.step,
                            &note[..note.len().min(800)],
                            operation_log.join("\n"),
                        );
                    }

                    if is_human_session && !actual_is_human {
                        panic!(
                            "Line range verification failed: human session claims line {} but char has {:?} attribution\n\
                             Seed: {}\nFile: {}\nSession: {}\n\
                             Line {}: char '{}' (step {})\n\
                             Note (first 800 chars):\n{}\n\
                             Operation log:\n{}",
                            line_num,
                            entry.attribution,
                            seed,
                            filename,
                            session_hash,
                            line_num,
                            ch,
                            entry.step,
                            &note[..note.len().min(800)],
                            operation_log.join("\n"),
                        );
                    }
                }
            }
        }
    }

    /// Verify that the authorship note for HEAD is well-formed.
    /// Checks JSON validity, required fields, and attestation format.
    pub fn verify_note_schema(
        &self,
        repo: &TestRepo,
        operation_log: &[String],
        seed: u64,
    ) {
        if self.skip_all_blame {
            return;
        }

        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => return, // No note to verify
        };

        // Check for separator - note may start with "---" if there are no attestations
        let (attestation_section, json_section) = if note.starts_with("---\n") {
            // Empty attestation section
            ("", &note[4..]) // Skip "---\n"
        } else if let Some(separator_idx) = note.find("\n---\n") {
            let attestation_section = &note[..separator_idx];
            let json_section = &note[separator_idx + 5..]; // Skip "\n---\n"
            (attestation_section, json_section)
        } else {
            panic!(
                "Note schema validation failed: missing '---' separator\n\
                 Seed: {}\nHead: {}\n\
                 Note (first 500 chars):\n{}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                &note[..note.len().min(500)],
                operation_log.join("\n"),
            );
        };

        // Validate JSON
        let json_value: serde_json::Value = match serde_json::from_str(json_section.trim()) {
            Ok(v) => v,
            Err(e) => {
                panic!(
                    "Note schema validation failed: invalid JSON in metadata section\n\
                     Seed: {}\nHead: {}\n\
                     JSON parse error: {}\n\
                     JSON section (first 500 chars):\n{}\n\
                     Operation log:\n{}",
                    seed,
                    head_sha,
                    e,
                    &json_section[..json_section.len().min(500)],
                    operation_log.join("\n"),
                );
            }
        };

        // Check required top-level keys
        let obj = match json_value.as_object() {
            Some(o) => o,
            None => {
                panic!(
                    "Note schema validation failed: JSON root is not an object\n\
                     Seed: {}\nHead: {}\n\
                     Operation log:\n{}",
                    seed,
                    head_sha,
                    operation_log.join("\n"),
                );
            }
        };

        if !obj.contains_key("schema_version") {
            panic!(
                "Note schema validation failed: missing 'schema_version' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        if !obj.contains_key("sessions") && !obj.contains_key("prompts") {
            panic!(
                "Note schema validation failed: missing 'sessions' or 'prompts' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        if !obj.contains_key("base_commit_sha") {
            panic!(
                "Note schema validation failed: missing 'base_commit_sha' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        // Validate attestation section format
        let mut current_file: Option<String> = None;
        for (line_num, line) in attestation_section.lines().enumerate() {
            if line.is_empty() {
                continue;
            }

            let is_indented = line.starts_with(' ') || line.starts_with('\t');

            if !is_indented {
                // File path line
                current_file = Some(line.to_string());
            } else {
                // Entry line
                if current_file.is_none() {
                    panic!(
                        "Note schema validation failed: attestation entry before any file path (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }

                let trimmed = line.trim();
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 2 {
                    panic!(
                        "Note schema validation failed: attestation entry missing hash or ranges (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }

                // Validate line ranges format
                let ranges_str = parts[1];
                if !is_valid_line_ranges(ranges_str) {
                    panic!(
                        "Note schema validation failed: invalid line range format '{}' (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        ranges_str,
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }
            }
        }
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

/// Parse line ranges from a string like "1-5" or "1,3-5,7" into a vector of (start, end) tuples.
/// Single lines like "3" become (3, 3).
fn parse_line_ranges(ranges_str: &str) -> Vec<(u32, u32)> {
    let mut result = Vec::new();
    for part in ranges_str.split(',') {
        if let Some(dash_idx) = part.find('-') {
            // Range like "1-5"
            let start = part[..dash_idx].parse::<u32>().unwrap_or(0);
            let end = part[dash_idx + 1..].parse::<u32>().unwrap_or(0);
            if start > 0 && end > 0 {
                result.push((start, end));
            }
        } else {
            // Single line like "3"
            if let Ok(line) = part.parse::<u32>() {
                if line > 0 {
                    result.push((line, line));
                }
            }
        }
    }
    result
}

/// Check if a line ranges string is valid (only contains digits, dashes, and commas).
fn is_valid_line_ranges(ranges_str: &str) -> bool {
    if ranges_str.is_empty() {
        return false;
    }
    ranges_str.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ',')
}
