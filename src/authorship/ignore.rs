use crate::git::repository::Repository;
use glob::Pattern;
use std::collections::HashSet;
use std::fs;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repository::from_bare_repository;
    use crate::git::test_utils::TmpRepo;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn make_bare_repo(
        root_gitattributes: Option<&str>,
        parent_gitattributes: Option<&str>,
    ) -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let bare = temp.path().join("bare.git");
        fs::create_dir_all(&source).expect("create source");

        run_git(&source, &["init"]);
        run_git(&source, &["config", "user.name", "Test User"]);
        run_git(&source, &["config", "user.email", "test@example.com"]);

        fs::write(source.join("README.md"), "# repo\n").expect("write readme");
        if let Some(attrs) = root_gitattributes {
            fs::write(source.join(".gitattributes"), attrs).expect("write attrs");
        }

        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "initial"]);
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        if let Some(parent_attrs) = parent_gitattributes {
            fs::write(temp.path().join(".gitattributes"), parent_attrs)
                .expect("write parent attrs");
        }

        (
            temp,
            from_bare_repository(&bare).expect("bare repository should load"),
        )
    }

    #[test]
    fn defaults_include_snapshot_and_lock_patterns() {
        let defaults = default_ignore_patterns();
        assert!(defaults.contains(&"**/*.snap".to_string()));
        assert!(defaults.contains(&"Cargo.lock".to_string()));
        assert!(defaults.contains(&"*.generated.*".to_string()));
    }

    #[test]
    fn defaults_do_not_ignore_generic_snapshots_directories() {
        let defaults = default_ignore_patterns();
        let matcher = build_ignore_matcher(&defaults);

        assert!(!should_ignore_file_with_matcher(
            "backups/snapshots/state.json",
            &matcher
        ));
        assert!(should_ignore_file_with_matcher(
            "tests/__snapshots__/feature.snap",
            &matcher
        ));
        assert!(should_ignore_file_with_matcher(
            "tests/snapshots/feature.snap",
            &matcher
        ));
    }

    #[test]
    fn defaults_ignore_nested_named_lockfiles() {
        let defaults = default_ignore_patterns();
        let matcher = build_ignore_matcher(&defaults);

        assert!(should_ignore_file_with_matcher(
            "apps/web/Gemfile.lock",
            &matcher
        ));
        assert!(should_ignore_file_with_matcher(
            "services/api/package-lock.json",
            &matcher
        ));
        assert!(should_ignore_file_with_matcher(
            "libs/core/Cargo.lock",
            &matcher
        ));
    }

    #[test]
    fn should_ignore_file_matches_path_and_filename() {
        let patterns = vec!["*.lock".to_string(), "**/node_modules/**".to_string()];
        let matcher = build_ignore_matcher(&patterns);
        assert!(should_ignore_file("Cargo.lock", &patterns));
        assert!(should_ignore_file("backend/Cargo.lock", &patterns));
        assert!(should_ignore_file_with_matcher("Cargo.lock", &matcher));
        assert!(should_ignore_file_with_matcher(
            "backend/Cargo.lock",
            &matcher
        ));
        assert!(should_ignore_file(
            "web/node_modules/lodash/index.js",
            &patterns
        ));
        assert!(should_ignore_file_with_matcher(
            "web/node_modules/lodash/index.js",
            &matcher
        ));
        assert!(!should_ignore_file("src/main.rs", &patterns));
        assert!(!should_ignore_file_with_matcher("src/main.rs", &matcher));
    }

    #[test]
    fn loads_positive_linguist_generated_only() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".gitattributes",
                "\
*.generated.ts linguist-generated=true
dist/** linguist-generated
vendor/** -linguist-generated
manual/** linguist-generated=false
flags/** linguist-generated=1
other/** linguist-generated=0
generated\\ files/** linguist-generated=true
",
                true,
            )
            .expect("write attrs");
        tmp_repo
            .commit_with_message("add gitattributes")
            .expect("commit");

        let patterns =
            load_linguist_generated_patterns_from_root_gitattributes(tmp_repo.gitai_repo());
        assert!(patterns.contains(&"*.generated.ts".to_string()));
        assert!(patterns.contains(&"dist/**".to_string()));
        assert!(patterns.contains(&"flags/**".to_string()));
        assert!(patterns.contains(&"generated files/**".to_string()));
        assert!(!patterns.contains(&"vendor/**".to_string()));
        assert!(!patterns.contains(&"manual/**".to_string()));
        assert!(!patterns.contains(&"other/**".to_string()));
    }

    #[test]
    fn ignores_gitattributes_macro_definitions() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".gitattributes",
                "\
[attr]generated linguist-generated=true
generated/** linguist-generated=true
",
                true,
            )
            .expect("write attrs");
        tmp_repo
            .commit_with_message("add gitattributes")
            .expect("commit");

        let patterns =
            load_linguist_generated_patterns_from_root_gitattributes(tmp_repo.gitai_repo());

        assert!(patterns.contains(&"generated/**".to_string()));
        assert!(!patterns.contains(&"[attr]generated".to_string()));
    }

    #[test]
    fn invalid_patterns_fallback_to_exact_path_or_filename() {
        let patterns = vec!["[".to_string(), "docs/[bad".to_string()];
        let matcher = build_ignore_matcher(&patterns);

        assert!(should_ignore_file_with_matcher("[", &matcher));
        assert!(should_ignore_file_with_matcher("docs/[bad", &matcher));
        assert!(!should_ignore_file_with_matcher("docs/good.rs", &matcher));
    }

    #[test]
    fn loads_linguist_generated_from_bare_repo_head() {
        let (_tmp, bare_repo) = make_bare_repo(
            Some("generated/** linguist-generated=true\nmanual/** linguist-generated=false\n"),
            None,
        );

        let patterns = load_linguist_generated_patterns_from_root_gitattributes(&bare_repo);
        assert!(patterns.contains(&"generated/**".to_string()));
        assert!(!patterns.contains(&"manual/**".to_string()));
    }

    #[test]
    fn bare_repo_does_not_read_parent_directory_gitattributes() {
        let (_tmp, bare_repo) = make_bare_repo(None, Some("leak/** linguist-generated=true\n"));

        let patterns = load_linguist_generated_patterns_from_root_gitattributes(&bare_repo);
        assert!(patterns.is_empty());
    }

    #[test]
    fn loads_git_ai_ignore_patterns_from_workdir() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".git-ai-ignore",
                "\
# This is a comment
docs/**
*.pdf

assets/images/**
",
                true,
            )
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add .git-ai-ignore")
            .expect("commit");

        let patterns = load_git_ai_ignore_patterns(tmp_repo.gitai_repo());
        assert_eq!(patterns.len(), 3);
        assert!(patterns.contains(&"docs/**".to_string()));
        assert!(patterns.contains(&"*.pdf".to_string()));
        assert!(patterns.contains(&"assets/images/**".to_string()));
    }

    #[test]
    fn git_ai_ignore_skips_comments_and_blank_lines() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".git-ai-ignore",
                "\
# comment line
   # indented comment

  *.log  
build/**
",
                true,
            )
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add .git-ai-ignore")
            .expect("commit");

        let patterns = load_git_ai_ignore_patterns(tmp_repo.gitai_repo());
        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"*.log".to_string()));
        assert!(patterns.contains(&"build/**".to_string()));
    }

    #[test]
    fn git_ai_ignore_deduplicates_patterns() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".git-ai-ignore",
                "\
*.pdf
docs/**
*.pdf
",
                true,
            )
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add .git-ai-ignore")
            .expect("commit");

        let patterns = load_git_ai_ignore_patterns(tmp_repo.gitai_repo());
        assert_eq!(patterns.len(), 2);
    }

    #[test]
    fn git_ai_ignore_returns_empty_when_file_missing() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file("README.md", "# repo\n", true)
            .expect("write readme");
        tmp_repo.commit_with_message("initial").expect("commit");

        let patterns = load_git_ai_ignore_patterns(tmp_repo.gitai_repo());
        assert!(patterns.is_empty());
    }

    #[test]
    fn effective_patterns_include_git_ai_ignore() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(".git-ai-ignore", "custom/**\n*.secret\n", true)
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add .git-ai-ignore")
            .expect("commit");

        let patterns = effective_ignore_patterns(tmp_repo.gitai_repo(), &[], &[]);
        assert!(patterns.contains(&"custom/**".to_string()));
        assert!(patterns.contains(&"*.secret".to_string()));
        // Default patterns should still be present
        assert!(patterns.contains(&"*.lock".to_string()));
    }

    #[test]
    fn effective_patterns_union_gitattributes_and_git_ai_ignore() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(
                ".gitattributes",
                "generated/** linguist-generated=true\n",
                true,
            )
            .expect("write .gitattributes");
        tmp_repo
            .write_file(".git-ai-ignore", "docs/**\n", true)
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add gitattributes and git-ai-ignore")
            .expect("commit");

        let patterns = effective_ignore_patterns(tmp_repo.gitai_repo(), &[], &[]);
        // From .gitattributes linguist-generated
        assert!(patterns.contains(&"generated/**".to_string()));
        // From .git-ai-ignore
        assert!(patterns.contains(&"docs/**".to_string()));
        // Defaults
        assert!(patterns.contains(&"*.lock".to_string()));
    }

    #[test]
    fn effective_patterns_union_git_ai_ignore_and_user_patterns() {
        let tmp_repo = TmpRepo::new().expect("tmp repo");
        tmp_repo
            .write_file(".git-ai-ignore", "docs/**\n", true)
            .expect("write .git-ai-ignore");
        tmp_repo
            .commit_with_message("add .git-ai-ignore")
            .expect("commit");

        let user = vec!["tests/**".to_string()];
        let patterns = effective_ignore_patterns(tmp_repo.gitai_repo(), &user, &[]);
        // From .git-ai-ignore
        assert!(patterns.contains(&"docs/**".to_string()));
        // From user --ignore flag
        assert!(patterns.contains(&"tests/**".to_string()));
        // Defaults
        assert!(patterns.contains(&"*.lock".to_string()));
    }

    fn make_bare_repo_with_ignore(
        root_gitattributes: Option<&str>,
        git_ai_ignore: Option<&str>,
    ) -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let bare = temp.path().join("bare.git");
        fs::create_dir_all(&source).expect("create source");

        run_git(&source, &["init"]);
        run_git(&source, &["config", "user.name", "Test User"]);
        run_git(&source, &["config", "user.email", "test@example.com"]);

        fs::write(source.join("README.md"), "# repo\n").expect("write readme");
        if let Some(attrs) = root_gitattributes {
            fs::write(source.join(".gitattributes"), attrs).expect("write attrs");
        }
        if let Some(ignore) = git_ai_ignore {
            fs::write(source.join(".git-ai-ignore"), ignore).expect("write .git-ai-ignore");
        }

        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "initial"]);
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        (
            temp,
            from_bare_repository(&bare).expect("bare repository should load"),
        )
    }

    #[test]
    fn loads_git_ai_ignore_from_bare_repo_head() {
        let (_tmp, bare_repo) = make_bare_repo_with_ignore(None, Some("docs/**\n*.pdf\n"));

        let patterns = load_git_ai_ignore_patterns(&bare_repo);
        assert!(patterns.contains(&"docs/**".to_string()));
        assert!(patterns.contains(&"*.pdf".to_string()));
    }

    #[test]
    fn bare_repo_returns_empty_when_git_ai_ignore_missing() {
        let (_tmp, bare_repo) = make_bare_repo_with_ignore(None, None);

        let patterns = load_git_ai_ignore_patterns(&bare_repo);
        assert!(patterns.is_empty());
    }

    #[test]
    fn bare_repo_effective_patterns_union_gitattributes_and_git_ai_ignore() {
        let (_tmp, bare_repo) = make_bare_repo_with_ignore(
            Some("generated/** linguist-generated=true\n"),
            Some("docs/**\n"),
        );

        let patterns = effective_ignore_patterns(&bare_repo, &[], &[]);
        assert!(patterns.contains(&"generated/**".to_string()));
        assert!(patterns.contains(&"docs/**".to_string()));
        assert!(patterns.contains(&"*.lock".to_string()));
    }
}
