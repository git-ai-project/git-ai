/// Fast config reading without subprocess spawns
///
/// Only reads .git/config (local). Does NOT read global or system config.

use crate::error::GitAiError;
use std::fs;
use std::path::{Path, PathBuf};

pub struct FastConfigReader {
    git_dir: PathBuf,
}

impl FastConfigReader {
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
        }
    }

    /// Read a config value from .git/config
    ///
    /// Git config format (simplified):
    /// ```ini
    /// [section]
    ///     key = value
    /// [section "subsection"]
    ///     key = value
    /// ```
    ///
    /// This is a VERY simplified parser that handles basic cases only.
    /// Falls back to git CLI for complex configs.
    pub fn read_value(&self, section: &str, key: &str) -> Result<Option<String>, GitAiError> {
        let config_path = self.git_dir.join("config");

        let content = match fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };

        self.parse_config(&content, section, key)
    }

    fn parse_config(
        &self,
        content: &str,
        target_section: &str,
        target_key: &str,
    ) -> Result<Option<String>, GitAiError> {
        let mut current_section = String::new();

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            // Section header: [section] or [section "subsection"]
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let section_content = &trimmed[1..trimmed.len() - 1];
                current_section = section_content.to_string();
                continue;
            }

            // Key-value pair: key = value
            if let Some(eq_pos) = trimmed.find('=') {
                if current_section == target_section {
                    let key = trimmed[..eq_pos].trim();
                    if key == target_key {
                        let value = trimmed[eq_pos + 1..].trim();
                        // Remove quotes if present
                        let unquoted = value.trim_matches('"');
                        return Ok(Some(unquoted.to_string()));
                    }
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_git_dir() -> TempDir {
        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        temp
    }

    #[test]
    fn test_read_simple_config() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let config = r#"
[user]
    name = Test User
    email = test@example.com
[core]
    bare = false
"#;
        fs::write(git_dir.join("config"), config).unwrap();

        let reader = FastConfigReader::new(&git_dir);

        let name = reader.read_value("user", "name").unwrap();
        assert_eq!(name, Some("Test User".to_string()));

        let email = reader.read_value("user", "email").unwrap();
        assert_eq!(email, Some("test@example.com".to_string()));

        let bare = reader.read_value("core", "bare").unwrap();
        assert_eq!(bare, Some("false".to_string()));
    }

    #[test]
    fn test_read_nonexistent_key() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let config = r#"
[user]
    name = Test User
"#;
        fs::write(git_dir.join("config"), config).unwrap();

        let reader = FastConfigReader::new(&git_dir);

        let result = reader.read_value("user", "nonexistent").unwrap();
        assert_eq!(result, None);

        let result = reader.read_value("nonexistent", "key").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_config_with_quotes() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let config = r#"
[user]
    name = "Test User"
    email = "test@example.com"
"#;
        fs::write(git_dir.join("config"), config).unwrap();

        let reader = FastConfigReader::new(&git_dir);

        let name = reader.read_value("user", "name").unwrap();
        assert_eq!(name, Some("Test User".to_string()));

        let email = reader.read_value("user", "email").unwrap();
        assert_eq!(email, Some("test@example.com".to_string()));
    }

    #[test]
    fn test_config_with_comments() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let config = r#"
# This is a comment
[user]
    # Another comment
    name = Test User
    email = test@example.com  ; inline comment style
"#;
        fs::write(git_dir.join("config"), config).unwrap();

        let reader = FastConfigReader::new(&git_dir);

        let name = reader.read_value("user", "name").unwrap();
        assert_eq!(name, Some("Test User".to_string()));
    }

    #[test]
    fn test_nonexistent_config_file() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let reader = FastConfigReader::new(&git_dir);
        let result = reader.read_value("user", "name").unwrap();

        assert_eq!(result, None);
    }
}
