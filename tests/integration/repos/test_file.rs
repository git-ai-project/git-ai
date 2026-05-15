#![allow(dead_code)]

use std::{fs, path::PathBuf};

/// AI author names that indicate AI-generated content
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
    "ai_tab",
    "droid",
    "firebender",
    "opencode",
    "pi",
];

#[derive(Debug, Clone, PartialEq)]
pub enum AuthorType {
    Human,
    UnattributedHuman,
    Ai,
}

#[derive(Debug, Clone)]
pub struct ExpectedLine {
    pub contents: String,
    pub author_type: AuthorType,
}

impl ExpectedLine {
    fn new(contents: String, author_type: AuthorType) -> Self {
        if contents.contains('\n') {
            panic!(
                "fluent test file API does not support strings with new lines (must be a single line): {:?}",
                contents
            );
        }
        Self {
            contents,
            author_type,
        }
    }
}

/// Trait to add .ai(), .human(), and .unattributed_human() methods to string types
pub trait ExpectedLineExt {
    fn ai(self) -> ExpectedLine;
    fn human(self) -> ExpectedLine;
    fn unattributed_human(self) -> ExpectedLine;
}

impl ExpectedLineExt for &str {
    fn ai(self) -> ExpectedLine {
        ExpectedLine::new(self.to_string(), AuthorType::Ai)
    }

    fn human(self) -> ExpectedLine {
        ExpectedLine::new(self.to_string(), AuthorType::Human)
    }

    fn unattributed_human(self) -> ExpectedLine {
        ExpectedLine::new(self.to_string(), AuthorType::UnattributedHuman)
    }
}

impl ExpectedLineExt for String {
    fn ai(self) -> ExpectedLine {
        ExpectedLine::new(self, AuthorType::Ai)
    }

    fn human(self) -> ExpectedLine {
        ExpectedLine::new(self, AuthorType::Human)
    }

    fn unattributed_human(self) -> ExpectedLine {
        ExpectedLine::new(self, AuthorType::UnattributedHuman)
    }
}

impl ExpectedLineExt for ExpectedLine {
    fn ai(self) -> ExpectedLine {
        ExpectedLine::new(self.contents, AuthorType::Ai)
    }

    fn human(self) -> ExpectedLine {
        ExpectedLine::new(self.contents, AuthorType::Human)
    }

    fn unattributed_human(self) -> ExpectedLine {
        ExpectedLine::new(self.contents, AuthorType::UnattributedHuman)
    }
}

/// Default conversion from &str to ExpectedLine (defaults to Human authorship)
impl From<&str> for ExpectedLine {
    fn from(s: &str) -> Self {
        ExpectedLine::new(s.to_string(), AuthorType::Human)
    }
}

/// Default conversion from String to ExpectedLine (defaults to Human authorship)
impl From<String> for ExpectedLine {
    fn from(s: String) -> Self {
        ExpectedLine::new(s, AuthorType::Human)
    }
}

// ---------------------------------------------------------------------------
// TestFile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TestFile<'a> {
    pub lines: Vec<ExpectedLine>,
    pub file_path: PathBuf,
    pub repo: &'a super::test_repo::TestRepo,
}

impl<'a> TestFile<'a> {
    pub fn new_with_filename(
        file_path: PathBuf,
        lines: Vec<ExpectedLine>,
        repo: &'a super::test_repo::TestRepo,
    ) -> Self {
        Self {
            lines,
            file_path,
            repo,
        }
    }

    /// Populate TestFile from an existing file by reading its contents and blame
    pub fn from_existing_file(file_path: PathBuf, repo: &'a super::test_repo::TestRepo) -> Self {
        if !file_path.exists() {
            return Self {
                lines: vec![],
                file_path,
                repo,
            };
        }

        let contents = fs::read_to_string(&file_path).unwrap_or_default();
        if contents.is_empty() {
            return Self {
                lines: vec![],
                file_path,
                repo,
            };
        }

        let filename = file_path
            .strip_prefix(repo.path())
            .unwrap_or(&file_path)
            .to_string_lossy()
            .replace('\\', "/");
        let blame_result = repo.git_ai(&["blame", &filename]);

        let lines = if let Ok(blame_output) = blame_result {
            let content_lines: Vec<&str> = contents.lines().collect();
            let blame_lines: Vec<&str> = blame_output
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect();

            content_lines
                .iter()
                .zip(blame_lines.iter())
                .map(|(content, blame_line)| {
                    let (author, _) = parse_blame_line(blame_line);
                    let author_type = if is_ai_author(&author) {
                        AuthorType::Ai
                    } else {
                        AuthorType::Human
                    };
                    ExpectedLine::new(content.to_string(), author_type)
                })
                .collect()
        } else {
            contents
                .lines()
                .map(|line| ExpectedLine::new(line.to_string(), AuthorType::Human))
                .collect()
        };

        Self {
            lines,
            file_path,
            repo,
        }
    }

    pub fn stage(&self) {
        self.repo
            .git(&["add", self.file_path.to_str().expect("valid path")])
            .expect("add file should succeed");
    }

    pub fn contents(&self) -> String {
        self.lines
            .iter()
            .map(|s| s.contents.clone())
            .collect::<Vec<String>>()
            .join("\n")
    }

    fn repo_relative_path(&self) -> String {
        self.file_path
            .strip_prefix(self.repo.path())
            .expect("test file path should be inside the test repo")
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn run_checkpoint_for_author_type(&self, author_type: &AuthorType) {
        let relative_path = self.repo_relative_path();
        let result = match author_type {
            AuthorType::Ai => self
                .repo
                .git_ai(&["checkpoint", "mock_ai", relative_path.as_str()]),
            AuthorType::Human => {
                self.repo
                    .git_ai(&["checkpoint", "mock_known_human", relative_path.as_str()])
            }
            AuthorType::UnattributedHuman => {
                self.repo
                    .git_ai(&["checkpoint", "--", relative_path.as_str()])
            }
        };
        result.unwrap();
    }

    fn write_and_checkpoint(&self, author_type: &AuthorType) {
        if let Some(parent) = self.file_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent).expect("failed to create parent directories");
        }
        let contents = self.contents();
        fs::write(&self.file_path, contents).unwrap();
        self.run_checkpoint_for_author_type(author_type);
    }

    fn write_and_checkpoint_with_contents(&self, contents: &str, author_type: &AuthorType) {
        if let Some(parent) = self.file_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent).expect("failed to create parent directories");
        }
        fs::write(&self.file_path, contents).unwrap();

        // Stage the file first
        self.repo.git(&["add", "-A"]).unwrap();

        self.run_checkpoint_for_author_type(author_type);
    }

    // -------------------------------------------------------------------------
    // Mutation methods
    // -------------------------------------------------------------------------

    /// Like `set_contents` but does NOT stage the file (no `git add`).
    /// Useful for testing partial staging scenarios.
    pub fn set_contents_no_stage<T: Into<ExpectedLine>>(&mut self, lines: Vec<T>) -> &mut Self {
        let lines: Vec<ExpectedLine> = lines.into_iter().map(|l| l.into()).collect();

        // Write human lines first (with placeholders for AI lines)
        let line_contents = lines
            .iter()
            .map(|s| {
                if s.author_type == AuthorType::Ai {
                    "||__AI LINE__ PENDING__||".to_string()
                } else {
                    s.contents.clone()
                }
            })
            .collect::<Vec<String>>()
            .join("\n");

        let human_kind = if lines
            .iter()
            .any(|l| l.author_type == AuthorType::UnattributedHuman)
        {
            &AuthorType::UnattributedHuman
        } else {
            &AuthorType::Human
        };
        // Write and checkpoint but don't stage
        if let Some(parent) = self.file_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent).expect("failed to create parent directories");
        }
        fs::write(&self.file_path, &line_contents).unwrap();
        self.run_checkpoint_for_author_type(human_kind);

        // Write full content with AI lines (no stage)
        let line_contents_with_ai = lines
            .iter()
            .map(|s| s.contents.clone())
            .collect::<Vec<String>>()
            .join("\n");

        fs::write(&self.file_path, &line_contents_with_ai).unwrap();
        self.run_checkpoint_for_author_type(&AuthorType::Ai);

        self.lines = lines;
        self
    }

    pub fn set_contents<T: Into<ExpectedLine>>(&mut self, lines: Vec<T>) -> &mut Self {
        let lines: Vec<ExpectedLine> = lines.into_iter().map(|l| l.into()).collect();

        // Write human lines first (with placeholders for AI lines)
        let line_contents = lines
            .iter()
            .map(|s| {
                if s.author_type == AuthorType::Ai {
                    "||__AI LINE__ PENDING__||".to_string()
                } else {
                    s.contents.clone()
                }
            })
            .collect::<Vec<String>>()
            .join("\n");

        let human_kind = if lines
            .iter()
            .any(|l| l.author_type == AuthorType::UnattributedHuman)
        {
            &AuthorType::UnattributedHuman
        } else {
            &AuthorType::Human
        };
        self.write_and_checkpoint_with_contents(&line_contents, human_kind);

        // Write full content with AI lines
        let line_contents_with_ai = lines
            .iter()
            .map(|s| s.contents.clone())
            .collect::<Vec<String>>()
            .join("\n");

        self.write_and_checkpoint_with_contents(&line_contents_with_ai, &AuthorType::Ai);

        self.lines = lines;
        self
    }

    pub fn insert_at<T: Into<ExpectedLine>>(
        &mut self,
        starting_index: usize,
        lines: Vec<T>,
    ) -> &mut Self {
        let lines: Vec<ExpectedLine> = lines.into_iter().map(|l| l.into()).collect();

        if lines.is_empty() {
            panic!("[test internals] must insert > 0 lines")
        }

        // Build splits - indices where author type changes
        let mut splits: Vec<usize> = vec![0];
        let mut last_author_type = &lines[0].author_type;

        for (i, line) in lines.iter().enumerate().skip(1) {
            if &line.author_type != last_author_type {
                splits.push(i);
                last_author_type = &line.author_type;
            }
        }

        let mut cumulative_offset = 0;

        for (chunk_idx, &split_start) in splits.iter().enumerate() {
            let split_end = if chunk_idx + 1 < splits.len() {
                splits[chunk_idx + 1]
            } else {
                lines.len()
            };

            let chunk = &lines[split_start..split_end];
            let author_type = &chunk[0].author_type;

            let insert_position = starting_index + cumulative_offset;
            self.lines
                .splice(insert_position..insert_position, chunk.iter().cloned());

            self.write_and_checkpoint(author_type);

            cumulative_offset += chunk.len();
        }

        self
    }

    pub fn replace_at<T: Into<ExpectedLine>>(&mut self, index: usize, line: T) -> &mut Self {
        let line = line.into();
        if index >= self.lines.len() {
            panic!(
                "Index {} out of bounds for {} lines",
                index,
                self.lines.len()
            );
        }

        if line.author_type == AuthorType::Ai {
            // For AI replacements, take a pre-edit (human) checkpoint first
            // to establish the "before" state, then write the new content
            // and take an AI checkpoint.
            self.run_checkpoint_for_author_type(&AuthorType::UnattributedHuman);
            self.lines[index] = line.clone();
            self.write_and_checkpoint(&AuthorType::Ai);
        } else {
            self.lines[index] = line.clone();
            self.write_and_checkpoint(&line.author_type);
        }

        self
    }

    pub fn delete_at(&mut self, index: usize) -> &mut Self {
        if index < self.lines.len() {
            self.lines.remove(index);
        } else {
            panic!(
                "Index {} out of bounds for {} lines",
                index,
                self.lines.len()
            );
        }

        self.write_and_checkpoint(&AuthorType::Human);
        self
    }

    pub fn delete_range(&mut self, start: usize, end: usize) -> &mut Self {
        if start >= end {
            panic!(
                "[test internals] start index {} must be less than end index {}",
                start, end
            );
        }

        if end > self.lines.len() {
            panic!(
                "End index {} out of bounds for {} lines",
                end,
                self.lines.len()
            );
        }

        self.lines.drain(start..end);

        self.write_and_checkpoint(&AuthorType::Human);
        self
    }

    // -------------------------------------------------------------------------
    // Assertion methods
    // -------------------------------------------------------------------------

    pub fn assert_lines_and_blame<T: Into<ExpectedLine>>(&mut self, lines: Vec<T>) {
        let expected_lines: Vec<ExpectedLine> = lines.into_iter().map(|l| l.into()).collect();

        let filename = self.repo_relative_path();
        let blame_output = self
            .repo
            .git_ai(&["blame", &filename])
            .expect("git-ai blame should succeed");

        let actual_lines: Vec<(String, String)> = blame_output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(parse_blame_line)
            .collect();

        assert_eq!(
            actual_lines.len(),
            expected_lines.len(),
            "Number of lines in blame output ({}) doesn't match expected ({})\nBlame output:\n{}",
            actual_lines.len(),
            expected_lines.len(),
            blame_output
        );

        for (i, ((actual_author, actual_content), expected_line)) in
            actual_lines.iter().zip(&expected_lines).enumerate()
        {
            let line_num = i + 1;

            assert_eq!(
                actual_content.trim(),
                expected_line.contents.trim(),
                "Line {}: Content mismatch\nExpected: {:?}\nActual: {:?}\nFull blame output:\n{}",
                line_num,
                expected_line.contents,
                actual_content,
                blame_output
            );

            match &expected_line.author_type {
                AuthorType::Ai => {
                    assert!(
                        is_ai_author(actual_author),
                        "Line {}: Expected AI author but got '{}'\nExpected: {:?}\nActual content: {:?}\nFull blame output:\n{}",
                        line_num,
                        actual_author,
                        expected_line,
                        actual_content,
                        blame_output
                    );
                }
                AuthorType::Human | AuthorType::UnattributedHuman => {
                    assert!(
                        !is_ai_author(actual_author),
                        "Line {}: Expected Human author but got AI author '{}'\nExpected: {:?}\nActual content: {:?}\nFull blame output:\n{}",
                        line_num,
                        actual_author,
                        expected_line,
                        actual_content,
                        blame_output
                    );
                }
            }
        }
    }

    /// Return lines attributed to a specific author type.
    /// Runs blame and filters by author type.
    pub fn lines_by_author(&self, author_type: AuthorType) -> Vec<String> {
        let filename = self.repo_relative_path();
        let blame_output = match self.repo.git_ai(&["blame", &filename]) {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };

        blame_output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                let (author, content) = parse_blame_line(line);
                let is_ai = is_ai_author(&author);
                let matches = match &author_type {
                    AuthorType::Ai => is_ai,
                    AuthorType::Human | AuthorType::UnattributedHuman => !is_ai,
                };
                if matches { Some(content) } else { None }
            })
            .collect()
    }

    /// Assert only committed lines (filters out uncommitted lines).
    pub fn assert_committed_lines<T: Into<ExpectedLine>>(&mut self, lines: Vec<T>) {
        let expected_lines: Vec<ExpectedLine> = lines.into_iter().map(|l| l.into()).collect();

        let filename = self.repo_relative_path();
        let blame_output = self
            .repo
            .git_ai(&["blame", &filename])
            .expect("git-ai blame should succeed");

        let committed_lines: Vec<(String, String)> = blame_output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(parse_blame_line)
            .filter(|(author, _)| author != "Not Committed Yet")
            .collect();

        assert_eq!(
            committed_lines.len(),
            expected_lines.len(),
            "Number of committed lines ({}) doesn't match expected ({})\nBlame output:\n{}",
            committed_lines.len(),
            expected_lines.len(),
            blame_output
        );

        for (i, ((actual_author, actual_content), expected_line)) in
            committed_lines.iter().zip(&expected_lines).enumerate()
        {
            let line_num = i + 1;

            assert_eq!(
                actual_content.trim(),
                expected_line.contents.trim(),
                "Line {}: Content mismatch\nExpected: {:?}\nActual: {:?}\nFull blame output:\n{}",
                line_num,
                expected_line.contents,
                actual_content,
                blame_output
            );

            match &expected_line.author_type {
                AuthorType::Ai => {
                    assert!(
                        is_ai_author(actual_author),
                        "Line {}: Expected AI author but got '{}'\nExpected: {:?}\nActual content: {:?}\nFull blame output:\n{}",
                        line_num,
                        actual_author,
                        expected_line,
                        actual_content,
                        blame_output
                    );
                }
                AuthorType::Human | AuthorType::UnattributedHuman => {
                    assert!(
                        !is_ai_author(actual_author),
                        "Line {}: Expected Human author but got AI author '{}'\nExpected: {:?}\nActual content: {:?}\nFull blame output:\n{}",
                        line_num,
                        actual_author,
                        expected_line,
                        actual_content,
                        blame_output
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Blame parsing helpers
// ---------------------------------------------------------------------------

/// Parse a blame line: `sha (author date line_num) content`
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

/// Check if an author string indicates AI authorship.
fn is_ai_author(author: &str) -> bool {
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

// ---------------------------------------------------------------------------
// lines! macro
// ---------------------------------------------------------------------------

/// Macro to create a Vec<ExpectedLine> from mixed types.
/// Plain strings default to Human authorship.
#[macro_export]
macro_rules! lines {
    ($($line:expr),* $(,)?) => {{
        {
            use $crate::repos::test_file::ExpectedLine;
            let v: Vec<ExpectedLine> = vec![$(Into::into($line)),*];
            v
        }
    }};
}
