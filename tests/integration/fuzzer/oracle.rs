use std::collections::HashMap;

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
pub struct CharRegistry {
    pool: Vec<char>,
    next: usize,
    entries: HashMap<char, CharEntry>,
}

impl CharRegistry {
    pub fn new() -> Self {
        let pool: Vec<char> = CHAR_POOL.chars().collect();
        Self {
            pool,
            next: 0,
            entries: HashMap::new(),
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
            panic!(
                "Blame line count mismatch for '{}'\n\
                 Seed: {}\n\
                 Expected {} lines, got {} lines\n\
                 Expected chars: {:?}\n\
                 Blame output:\n{}\n\
                 Operation log:\n{}\n\
                 Registry:\n{}",
                filename,
                seed,
                file_lines.len(),
                blame_lines.len(),
                file_lines,
                blame_output,
                operation_log.join("\n"),
                self.dump()
            );
        }

        for (i, (blame_line, &expected_char)) in
            blame_lines.iter().zip(file_lines.iter()).enumerate()
        {
            let line_num = i + 1;
            let (author, _content) = parse_blame_line(blame_line);
            let is_ai_author = is_ai_author_name(&author);

            let entry = self.get(expected_char).unwrap_or_else(|| {
                panic!(
                    "Character '{}' on line {} not found in registry\n\
                     Seed: {}\nFilename: {}\nBlame line: {}\n\
                     Operation log:\n{}\nRegistry:\n{}",
                    expected_char,
                    line_num,
                    seed,
                    filename,
                    blame_line,
                    operation_log.join("\n"),
                    self.dump()
                );
            });

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
