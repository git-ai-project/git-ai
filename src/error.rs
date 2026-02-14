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
    #[allow(dead_code)]
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
