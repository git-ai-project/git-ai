use std::process::Command;
use std::sync::OnceLock;

/// Returns a new Command configured to run git.
/// Uses PATH lookup to find the git binary, with fallback to common install locations on Unix.
pub fn git_cmd() -> Command {
    Command::new(git_path())
}

/// Returns the path to the git binary.
/// On Unix, tries common install locations first, then falls back to PATH lookup.
/// On Windows, uses PATH lookup directly.
pub fn git_path() -> &'static str {
    static GIT: OnceLock<String> = OnceLock::new();
    GIT.get_or_init(|| {
        #[cfg(unix)]
        {
            for candidate in &[
                "/usr/bin/git",
                "/usr/local/bin/git",
                "/opt/homebrew/bin/git",
            ] {
                if std::path::Path::new(candidate).is_file() {
                    return candidate.to_string();
                }
            }
        }
        "git".to_string()
    })
}
