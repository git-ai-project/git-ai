use std::collections::HashMap;
use std::fmt;
use std::fs;

use crate::repos::test_repo::TestRepo;

use super::helpers::{
    is_ai_author_name, note_covers_line_as_ai, note_covers_line_as_human, parse_blame_line,
    parse_porcelain_line_info,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineAttribution {
    Ai,
    KnownHuman,
    Untracked,
}

impl fmt::Display for LineAttribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineAttribution::Ai => write!(f, "Ai"),
            LineAttribution::KnownHuman => write!(f, "KnownHuman"),
            LineAttribution::Untracked => write!(f, "Untracked"),
        }
    }
}

/// Global registry: maps each unique char to its CHECKPOINT-TIME attribution.
/// This never forgets — once a char is registered, its original attribution is preserved.
/// Reconciliation can downgrade it to Untracked in the FileModel, but the registry
/// always remembers what was checkpointed.
#[derive(Debug, Clone)]
pub struct AttrRegistry {
    map: HashMap<char, LineAttribution>,
}

impl AttrRegistry {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn register(&mut self, ch: char, attr: LineAttribution) {
        self.map.insert(ch, attr);
    }

    pub fn get(&self, ch: char) -> LineAttribution {
        self.map
            .get(&ch)
            .copied()
            .unwrap_or(LineAttribution::Untracked)
    }
}

/// The current state of a file as the fuzzer understands it.
/// `lines` contains one char per line — the char identifies the line uniquely.
/// Attribution is looked up from the AttrRegistry + reconciliation state.
#[derive(Debug, Clone)]
pub struct FileModel {
    pub filename: String,
    pub lines: Vec<char>,
    /// Per-line attribution AFTER reconciliation. This is what we assert against.
    /// Updated by reconcile() — can downgrade from registry's value to Untracked.
    pub resolved_attrs: Vec<LineAttribution>,
}

impl FileModel {
    pub fn new(filename: &str) -> Self {
        Self {
            filename: filename.to_string(),
            lines: Vec::new(),
            resolved_attrs: Vec::new(),
        }
    }

    pub fn write_to_disk(&self, repo: &TestRepo) {
        let content: String = self.lines.iter().map(|ch| format!("{}\n", ch)).collect();
        fs::write(repo.path().join(&self.filename), content).unwrap();
    }

    /// Re-read file content from disk. Updates `lines` to match what's on disk.
    /// Then rebuilds `resolved_attrs` from the registry (before reconciliation).
    pub fn sync_from_disk(&mut self, repo: &TestRepo, registry: &AttrRegistry) {
        let path = repo.path().join(&self.filename);
        if !path.exists() {
            self.lines.clear();
            self.resolved_attrs.clear();
            return;
        }
        let content = fs::read_to_string(&path).unwrap();
        self.lines = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.chars().next().unwrap_or('?'))
            .collect();
        self.resolved_attrs = self.lines.iter().map(|&ch| registry.get(ch)).collect();
    }

    /// Reconcile the in-memory model against git blame state.
    ///
    /// After any commit-producing operation, git blame may assign lines to different
    /// commits. If a line's blamed commit doesn't have an authorship note covering
    /// that line, its resolved attribution becomes Untracked.
    ///
    /// This IS our prediction of what git-ai blame will return.
    pub fn reconcile(&mut self, repo: &TestRepo) {
        let path = repo.path().join(&self.filename);
        if !path.exists() || self.lines.is_empty() {
            return;
        }

        let porcelain_output = repo
            .git(&["blame", "--line-porcelain", "--", &self.filename])
            .unwrap_or_else(|e| {
                panic!(
                    "git blame --line-porcelain failed for '{}': {}",
                    self.filename, e
                )
            });
        let line_info = parse_porcelain_line_info(&porcelain_output);

        if line_info.len() != self.lines.len() {
            panic!(
                "Porcelain line count ({}) != model line count ({}) for '{}'",
                line_info.len(),
                self.lines.len(),
                self.filename
            );
        }

        let mut note_cache: HashMap<String, Option<String>> = HashMap::new();

        for (i, attr) in self.resolved_attrs.iter_mut().enumerate() {
            if matches!(attr, LineAttribution::Untracked) {
                continue;
            }

            let info = &line_info[i];
            let note = note_cache
                .entry(info.commit_sha.clone())
                .or_insert_with(|| repo.read_authorship_note(&info.commit_sha));

            let still_covered = match note.as_deref() {
                None => false,
                Some(n) => match attr {
                    LineAttribution::Ai => {
                        note_covers_line_as_ai(n, &self.filename, info.orig_line)
                    }
                    LineAttribution::KnownHuman => {
                        note_covers_line_as_human(n, &self.filename, info.orig_line)
                    }
                    LineAttribution::Untracked => unreachable!(),
                },
            };

            if !still_covered {
                *attr = LineAttribution::Untracked;
            }
        }
    }

    /// Assert that git-ai blame output matches our model EXACTLY.
    /// Every line. Every time. No exceptions.
    pub fn assert_blame(&self, repo: &TestRepo, op_log: &[String], seed: u64) {
        let path = repo.path().join(&self.filename);
        if !path.exists() || self.lines.is_empty() {
            return;
        }

        let blame_output = match repo.git_ai(&["blame", &self.filename]) {
            Ok(output) => output,
            Err(e) => {
                panic!(
                    "git-ai blame failed for '{}'\nSeed: {}\nError: {}\nOp log:\n{}\nModel:\n{}",
                    self.filename,
                    seed,
                    e,
                    op_log.join("\n"),
                    self.dump()
                );
            }
        };

        let blame_lines: Vec<&str> = blame_output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        if blame_lines.len() != self.lines.len() {
            panic!(
                "Line count mismatch for '{}'\nSeed: {}\n\
                 Blame lines: {}\nModel lines: {}\n\
                 Op log:\n{}\nModel:\n{}",
                self.filename,
                seed,
                blame_lines.len(),
                self.lines.len(),
                op_log.join("\n"),
                self.dump()
            );
        }

        for (i, (blame_line, &expected_attr)) in
            blame_lines.iter().zip(&self.resolved_attrs).enumerate()
        {
            let line_num = i + 1;
            let (author, _content) = parse_blame_line(blame_line);
            let actual_is_ai = is_ai_author_name(&author);
            let expected_is_ai = matches!(expected_attr, LineAttribution::Ai);

            if expected_is_ai != actual_is_ai {
                panic!(
                    "Attribution mismatch on line {} of '{}'\n\
                     Seed: {}\n\
                     Char: '{}'\n\
                     Model says: {:?} (expected_is_ai={})\n\
                     Blame shows: author='{}' (actual_is_ai={})\n\
                     Blame line: {}\n\
                     Full blame:\n{}\n\
                     Op log:\n{}\n\
                     Model:\n{}",
                    line_num,
                    self.filename,
                    seed,
                    self.lines[i],
                    expected_attr,
                    expected_is_ai,
                    author,
                    actual_is_ai,
                    blame_line,
                    blame_output,
                    op_log.join("\n"),
                    self.dump()
                );
            }
        }
    }

    pub fn dump(&self) -> String {
        let mut out = format!("File: {} ({} lines)\n", self.filename, self.lines.len());
        for (i, (&ch, &attr)) in self.lines.iter().zip(&self.resolved_attrs).enumerate() {
            out.push_str(&format!("  L{}: '{}' -> {}\n", i + 1, ch, attr));
        }
        out
    }
}
