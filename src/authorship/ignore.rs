use crate::git::repository::Repository;
use glob::Pattern;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    "*.lock",
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.sum",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
    "Pipfile.lock",
    "shrinkwrap.yaml",
    "*.generated.*",
    "*.min.js",
    "*.min.css",
    "*.map",
    "**/vendor/**",
    "**/node_modules/**",
    "**/__snapshots__/**",
    "**/*.snap",
    "**/*.snap.new",
    "**/drizzle/meta/**",
    // Protobuf generated code
    "*.pbobjc.h",
    "*.pbobjc.m",
    "*.pb.go",
    "*.pb.h",
    "*.pb.cc",
    "*_pb2.py",
    "*_pb2_grpc.py",
    "*.pb.swift",
    "*.pb.dart",
];

#[derive(Clone, Debug)]
enum CompiledPattern {
    Glob(Pattern),
    Exact(String),
}

#[derive(Clone, Debug, Default)]
pub struct IgnoreMatcher {
    patterns: Vec<CompiledPattern>,
}

impl IgnoreMatcher {
    pub fn new(patterns: &[String]) -> Self {
        let patterns = patterns
            .iter()
            .map(|pattern| match Pattern::new(pattern) {
                Ok(glob) => CompiledPattern::Glob(glob),
                Err(_) => CompiledPattern::Exact(pattern.clone()),
            })
            .collect();

        Self { patterns }
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        let filename = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        self.patterns.iter().any(|pattern| match pattern {
            CompiledPattern::Glob(glob_pattern) => {
                glob_pattern.matches(path) || glob_pattern.matches(filename)
            }
            CompiledPattern::Exact(pattern) => filename == pattern || path == pattern,
        })
    }
}

pub fn default_ignore_patterns() -> Vec<String> {
    DEFAULT_IGNORE_PATTERNS
        .iter()
        .map(|pattern| pattern.to_string())
        .collect()
}

pub fn build_ignore_matcher(patterns: &[String]) -> IgnoreMatcher {
    IgnoreMatcher::new(patterns)
}

pub fn should_ignore_file_with_matcher(path: &str, matcher: &IgnoreMatcher) -> bool {
    matcher.is_ignored(path)
}

/// Check if a file path should be ignored based on the provided patterns.
/// Supports both exact matches and glob patterns (e.g., "*.lock", "**/*.generated.js").
#[allow(dead_code)] // Kept for API compatibility; prefer should_ignore_file_with_matcher in hot paths.
pub fn should_ignore_file(path: &str, patterns: &[String]) -> bool {
    should_ignore_file_with_matcher(path, &build_ignore_matcher(patterns))
}

pub fn load_linguist_generated_patterns_from_root_gitattributes(repo: &Repository) -> Vec<String> {
    let Some(contents) = load_root_gitattributes_contents(repo) else {
        return Vec::new();
    };
    parse_linguist_generated_patterns(&contents)
}

fn parse_linguist_generated_patterns(contents: &str) -> Vec<String> {
    let mut patterns = Vec::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let tokens = split_gitattributes_tokens(line);
        if tokens.len() < 2 {
            continue;
        }

        let path_pattern = &tokens[0];
        if path_pattern.starts_with("[attr]") {
            continue;
        }
        let mut state: Option<bool> = None;

        for attr in &tokens[1..] {
            if attr == "linguist-generated" {
                state = Some(true);
                continue;
            }
            if attr == "-linguist-generated" || attr == "!linguist-generated" {
                state = Some(false);
                continue;
            }
            if let Some(value) = attr.strip_prefix("linguist-generated=") {
                if value.eq_ignore_ascii_case("true") || value == "1" {
                    state = Some(true);
                } else if value.eq_ignore_ascii_case("false") || value == "0" {
                    state = Some(false);
                }
            }
        }

        if state == Some(true) {
            patterns.push(path_pattern.to_string());
        }
    }

    dedupe_patterns(patterns)
}

fn load_root_gitattributes_contents(repo: &Repository) -> Option<String> {
    if repo.is_bare_repository().unwrap_or(false) {
        return repo
            .get_file_content(".gitattributes", "HEAD")
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }

    let workdir = repo.workdir().ok()?;
    let gitattributes_path = workdir.join(".gitattributes");
    fs::read_to_string(gitattributes_path).ok()
}

/// Load ignore patterns from a `.git-ai-ignore` file at the repository root.
/// The file follows `.gitignore` syntax: one glob pattern per line, blank lines
/// and lines starting with `#` are skipped.
pub fn load_git_ai_ignore_patterns(repo: &Repository) -> Vec<String> {
    let Some(contents) = load_root_git_ai_ignore_contents(repo) else {
        return Vec::new();
    };

    let mut patterns = Vec::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        patterns.push(line.to_string());
    }

    dedupe_patterns(patterns)
}

fn load_root_git_ai_ignore_contents(repo: &Repository) -> Option<String> {
    if repo.is_bare_repository().unwrap_or(false) {
        return repo
            .get_file_content(".git-ai-ignore", "HEAD")
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
    }

    let workdir = repo.workdir().ok()?;
    let ignore_path = workdir.join(".git-ai-ignore");
    fs::read_to_string(ignore_path).ok()
}

/// Load `.git-ai-ignore` patterns from a repo root path directly (no Repository object needed).
/// Use this when you have a `&Path` but not a `Repository` (e.g. in snapshot capture code).
pub fn load_git_ai_ignore_patterns_from_path(repo_root: &Path) -> Vec<String> {
    let contents = match fs::read_to_string(repo_root.join(".git-ai-ignore")) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut patterns = Vec::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        patterns.push(line.to_string());
    }
    dedupe_patterns(patterns)
}

/// Load linguist-generated patterns from `.gitattributes` at a repo root path directly.
/// Use this when you have a `&Path` but not a `Repository` (e.g. in snapshot capture code).
/// Uses the same parser as `load_linguist_generated_patterns_from_root_gitattributes`.
pub fn load_linguist_generated_patterns_from_path(repo_root: &Path) -> Vec<String> {
    match fs::read_to_string(repo_root.join(".gitattributes")) {
        Ok(contents) => parse_linguist_generated_patterns(&contents),
        Err(_) => Vec::new(),
    }
}

pub fn effective_ignore_patterns(
    repo: &Repository,
    user_patterns: &[String],
    extra_patterns: &[String],
) -> Vec<String> {
    let mut patterns = default_ignore_patterns();
    patterns.extend(load_linguist_generated_patterns_from_root_gitattributes(
        repo,
    ));
    patterns.extend(load_git_ai_ignore_patterns(repo));
    patterns.extend(extra_patterns.iter().cloned());
    patterns.extend(user_patterns.iter().cloned());
    dedupe_patterns(patterns)
}

fn dedupe_patterns(patterns: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for pattern in patterns {
        if seen.insert(pattern.clone()) {
            deduped.push(pattern);
        }
    }

    deduped
}

fn split_gitattributes_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}
