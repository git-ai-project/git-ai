use std::fs;
use std::path::Path;

use crate::config::Config;
use crate::git::repository::Repository;
use crate::utils::debug_log;

/// Load ignore patterns from various sources and merge with CLI args
/// Priority order: global config > project file > CLI args (all are additive)
///
/// Sources:
/// 1. Global config: ~/.git-ai/config.json (stats_ignore_patterns field)
/// 2. Project file: .git-ai-ignore in repo root
/// 3. CLI args: --ignore patterns passed by user
pub fn load_ignore_patterns_from_files(repo: &Repository) -> Vec<String> {
    let mut patterns = Vec::new();

    // 1. Load from global config (~/.git-ai/config.json)
    let global_config_patterns = Config::get().stats_ignore_patterns();
    if !global_config_patterns.is_empty() {
        debug_log(&format!(
            "Loaded {} patterns from ~/.git-ai/config.json",
            global_config_patterns.len()
        ));
        patterns.extend_from_slice(global_config_patterns);
    }

    // 2. Load project ignore (.git-ai-ignore in workdir root)
    if let Some(project_patterns) = load_project_ignore(repo) {
        debug_log(&format!(
            "Loaded {} patterns from .git-ai-ignore",
            project_patterns.len()
        ));
        patterns.extend(project_patterns);
    }

    patterns
}

/// Load project ignore from .git-ai-ignore in workdir root
fn load_project_ignore(repo: &Repository) -> Option<Vec<String>> {
    let workdir = repo.workdir().ok()?;
    let path = workdir.join(".git-ai-ignore");
    read_ignore_file(&path)
}

/// Read ignore file and parse patterns
/// Supports comments (#) and blank lines
fn read_ignore_file(path: &Path) -> Option<Vec<String>> {
    if !path.exists() {
        return None;
    }

    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            debug_log(&format!("Failed to read ignore file {:?}: {}", path, e));
            return None;
        }
    };

    let patterns: Vec<String> = content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect();

    if patterns.is_empty() {
        None
    } else {
        Some(patterns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_ignore_file_with_comments() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let ignore_file = temp_dir.path().join("test-ignore");

        let mut file = fs::File::create(&ignore_file).unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "").unwrap(); // blank line
        writeln!(file, "*.lock").unwrap();
        writeln!(file, "dist/**").unwrap();
        writeln!(file, "  # Another comment  ").unwrap();
        writeln!(file, "  *.min.js  ").unwrap(); // with spaces
        drop(file);

        let patterns = read_ignore_file(&ignore_file).unwrap();
        assert_eq!(patterns.len(), 3);
        assert_eq!(patterns[0], "*.lock");
        assert_eq!(patterns[1], "dist/**");
        assert_eq!(patterns[2], "*.min.js");
    }

    #[test]
    fn test_read_ignore_file_empty() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let ignore_file = temp_dir.path().join("empty-ignore");

        let mut file = fs::File::create(&ignore_file).unwrap();
        writeln!(file, "# Only comments").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "  ").unwrap();
        drop(file);

        let patterns = read_ignore_file(&ignore_file);
        assert!(patterns.is_none());
    }

    #[test]
    fn test_read_ignore_file_nonexistent() {
        let patterns = read_ignore_file(Path::new("/nonexistent/path/ignore"));
        assert!(patterns.is_none());
    }
}
