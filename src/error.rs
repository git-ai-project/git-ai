use std::fmt;

#[derive(Debug)]
pub enum GitAiError {
    #[cfg(feature = "test-support")]
    GitError(git2::Error),
    IoError(std::io::Error),
    /// Errors from invoking the git CLI that exited with a non-zero status
    GitCliError {
        code: Option<i32>,
        stderr: String,
        args: Vec<String>,
    },
    /// Errors from  Gix
    GixError(String),
    JsonError(serde_json::Error),
    Utf8Error(std::str::Utf8Error),
    FromUtf8Error(std::string::FromUtf8Error),
    PresetError(String),
    SqliteError(rusqlite::Error),
    Generic(String),
}

impl fmt::Display for GitAiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "test-support")]
            GitAiError::GitError(e) => write!(f, "Git error: {}", e),
            GitAiError::IoError(e) => write!(f, "IO error: {}", e),
            GitAiError::GitCliError { code, stderr, args } => match code {
                Some(c) => write!(
                    f,
                    "Git CLI ({}) failed with exit code {}: {}",
                    args.join(" "),
                    c,
                    stderr
                ),
                None => write!(f, "Git CLI ({}) failed: {}", args.join(" "), stderr),
            },
            GitAiError::JsonError(e) => write!(f, "JSON error: {}", e),
            GitAiError::Utf8Error(e) => write!(f, "UTF-8 error: {}", e),
            GitAiError::FromUtf8Error(e) => write!(f, "From UTF-8 error: {}", e),
            GitAiError::PresetError(e) => write!(f, "{}", e),
            GitAiError::SqliteError(e) => write!(f, "SQLite error: {}", e),
            GitAiError::Generic(e) => write!(f, "Generic error: {}", e),
            GitAiError::GixError(e) => write!(f, "Gix error: {}", e),
        }
    }
}

impl std::error::Error for GitAiError {}

#[cfg(feature = "test-support")]
impl From<git2::Error> for GitAiError {
    fn from(err: git2::Error) -> Self {
        GitAiError::GitError(err)
    }
}

impl From<std::io::Error> for GitAiError {
    fn from(err: std::io::Error) -> Self {
        GitAiError::IoError(err)
    }
}

impl From<serde_json::Error> for GitAiError {
    fn from(err: serde_json::Error) -> Self {
        GitAiError::JsonError(err)
    }
}

impl From<std::str::Utf8Error> for GitAiError {
    fn from(err: std::str::Utf8Error) -> Self {
        GitAiError::Utf8Error(err)
    }
}

impl From<std::string::FromUtf8Error> for GitAiError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        GitAiError::FromUtf8Error(err)
    }
}

impl From<rusqlite::Error> for GitAiError {
    fn from(err: rusqlite::Error) -> Self {
        GitAiError::SqliteError(err)
    }
}

impl Clone for GitAiError {
    fn clone(&self) -> Self {
        match self {
            #[cfg(feature = "test-support")]
            GitAiError::GitError(e) => GitAiError::Generic(format!("Git error: {}", e)),
            GitAiError::IoError(e) => {
                GitAiError::IoError(std::io::Error::new(e.kind(), e.to_string()))
            }
            GitAiError::GitCliError { code, stderr, args } => GitAiError::GitCliError {
                code: *code,
                stderr: stderr.clone(),
                args: args.clone(),
            },
            GitAiError::JsonError(e) => GitAiError::Generic(format!("JSON error: {}", e)),
            GitAiError::Utf8Error(e) => GitAiError::Utf8Error(*e),
            GitAiError::FromUtf8Error(e) => GitAiError::FromUtf8Error(e.clone()),
            GitAiError::PresetError(s) => GitAiError::PresetError(s.clone()),
            GitAiError::SqliteError(e) => GitAiError::Generic(format!("SQLite error: {}", e)),
            GitAiError::Generic(s) => GitAiError::Generic(s.clone()),
            GitAiError::GixError(e) => GitAiError::Generic(format!("Gix error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = GitAiError::from(io_err);
        let display = format!("{}", err);
        assert!(display.contains("IO error"));
        assert!(display.contains("file not found"));
    }

    #[test]
    fn test_error_display_git_cli_error_with_code() {
        let err = GitAiError::GitCliError {
            code: Some(128),
            stderr: "fatal: not a git repository".to_string(),
            args: vec!["git".to_string(), "status".to_string()],
        };
        let display = format!("{}", err);
        assert!(display.contains("128"));
        assert!(display.contains("fatal: not a git repository"));
        assert!(display.contains("git status"));
    }

    #[test]
    fn test_error_display_git_cli_error_without_code() {
        let err = GitAiError::GitCliError {
            code: None,
            stderr: "command terminated".to_string(),
            args: vec!["git".to_string(), "push".to_string()],
        };
        let display = format!("{}", err);
        assert!(display.contains("Git CLI"));
        assert!(display.contains("command terminated"));
        assert!(display.contains("git push"));
    }

    #[test]
    fn test_error_display_json_error() {
        let json_str = "{invalid json";
        let json_err = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let err = GitAiError::from(json_err);
        let display = format!("{}", err);
        assert!(display.contains("JSON error"));
    }

    #[test]
    fn test_error_display_utf8_error() {
        let invalid_utf8 = vec![0xFF, 0xFE, 0xFD];
        let utf8_err = std::str::from_utf8(&invalid_utf8).unwrap_err();
        let err = GitAiError::from(utf8_err);
        let display = format!("{}", err);
        assert!(display.contains("UTF-8 error"));
    }

    #[test]
    fn test_error_display_from_utf8_error() {
        let invalid_utf8 = vec![0xFF, 0xFE, 0xFD];
        let from_utf8_err = String::from_utf8(invalid_utf8).unwrap_err();
        let err = GitAiError::from(from_utf8_err);
        let display = format!("{}", err);
        assert!(display.contains("From UTF-8 error"));
    }

    #[test]
    fn test_error_display_preset_error() {
        let err = GitAiError::PresetError("invalid preset configuration".to_string());
        let display = format!("{}", err);
        assert_eq!(display, "invalid preset configuration");
    }

    #[test]
    fn test_error_display_sqlite_error() {
        use rusqlite::{Connection, Error as SqlError};
        let conn = Connection::open_in_memory().unwrap();
        let sql_err = conn.execute("INVALID SQL", []).unwrap_err();
        let err = GitAiError::from(sql_err);
        let display = format!("{}", err);
        assert!(display.contains("SQLite error"));
    }

    #[test]
    fn test_error_display_generic() {
        let err = GitAiError::Generic("custom error message".to_string());
        let display = format!("{}", err);
        assert!(display.contains("Generic error"));
        assert!(display.contains("custom error message"));
    }

    #[test]
    fn test_error_display_gix_error() {
        let err = GitAiError::GixError("gix operation failed".to_string());
        let display = format!("{}", err);
        assert!(display.contains("Gix error"));
        assert!(display.contains("gix operation failed"));
    }

    #[test]
    fn test_error_clone_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = GitAiError::from(io_err);
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::IoError(_)));
        let display = format!("{}", cloned);
        assert!(display.contains("access denied"));
    }

    #[test]
    fn test_error_clone_git_cli_error() {
        let err = GitAiError::GitCliError {
            code: Some(1),
            stderr: "error message".to_string(),
            args: vec!["git".to_string(), "commit".to_string()],
        };
        let cloned = err.clone();
        match cloned {
            GitAiError::GitCliError { code, stderr, args } => {
                assert_eq!(code, Some(1));
                assert_eq!(stderr, "error message");
                assert_eq!(args, vec!["git".to_string(), "commit".to_string()]);
            }
            _ => panic!("Expected GitCliError"),
        }
    }

    #[test]
    fn test_error_clone_utf8_error() {
        let invalid_utf8 = vec![0xFF];
        let utf8_err = std::str::from_utf8(&invalid_utf8).unwrap_err();
        let err = GitAiError::from(utf8_err);
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::Utf8Error(_)));
    }

    #[test]
    fn test_error_clone_from_utf8_error() {
        let invalid_utf8 = vec![0xFF];
        let from_utf8_err = String::from_utf8(invalid_utf8).unwrap_err();
        let err = GitAiError::from(from_utf8_err);
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::FromUtf8Error(_)));
    }

    #[test]
    fn test_error_clone_preset_error() {
        let err = GitAiError::PresetError("preset error".to_string());
        let cloned = err.clone();
        match cloned {
            GitAiError::PresetError(msg) => assert_eq!(msg, "preset error"),
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_error_clone_generic() {
        let err = GitAiError::Generic("generic".to_string());
        let cloned = err.clone();
        match cloned {
            GitAiError::Generic(msg) => assert_eq!(msg, "generic"),
            _ => panic!("Expected Generic"),
        }
    }

    #[test]
    fn test_error_clone_json_converts_to_generic() {
        let json_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
        let err = GitAiError::from(json_err);
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::Generic(_)));
        let display = format!("{}", cloned);
        assert!(display.contains("JSON error"));
    }

    #[test]
    fn test_error_clone_sqlite_converts_to_generic() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        let sql_err = conn.execute("BAD SQL", []).unwrap_err();
        let err = GitAiError::from(sql_err);
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::Generic(_)));
        let display = format!("{}", cloned);
        assert!(display.contains("SQLite error"));
    }

    #[test]
    fn test_error_clone_gix_converts_to_generic() {
        let err = GitAiError::GixError("gix error".to_string());
        let cloned = err.clone();
        assert!(matches!(cloned, GitAiError::Generic(_)));
        let display = format!("{}", cloned);
        assert!(display.contains("Gix error"));
    }

    #[test]
    fn test_error_is_std_error() {
        let err = GitAiError::Generic("test".to_string());
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn test_error_debug_trait() {
        let err = GitAiError::Generic("debug test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("Generic"));
        assert!(debug_str.contains("debug test"));
    }
}
