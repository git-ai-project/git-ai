use crate::error::GitAiError;
use crate::git::diff_tree_to_tree::Diff;
use std::path::PathBuf;
use std::fs;

/// Check if debug logging is enabled via environment variable
///
/// This is checked once at module initialization to avoid repeated environment variable lookups.
static DEBUG_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static DEBUG_PERFORMANCE_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn is_debug_enabled() -> bool {
    *DEBUG_ENABLED.get_or_init(|| {
        cfg!(debug_assertions)
            || std::env::var("GIT_AI_DEBUG").unwrap_or_default() == "1"
            || std::env::var("GIT_AI_DEBUG_PERFORMANCE").unwrap_or_default() == "1"
    })
}

fn is_debug_performance_enabled() -> bool {
    is_debug_enabled()
        && *DEBUG_PERFORMANCE_ENABLED
            .get_or_init(|| std::env::var("GIT_AI_DEBUG_PERFORMANCE").unwrap_or_default() == "1")
}

pub fn debug_performance_log(msg: &str) {
    if is_debug_performance_enabled() {
        eprintln!("\x1b[1;33m[git-ai (perf)]\x1b[0m {}", msg);
    }
}

/// Debug logging utility function
///
/// Prints debug messages with a colored prefix when debug assertions are enabled or when
/// the `GIT_AI_DEBUG` environment variable is set to "1".
///
/// # Arguments
///
/// * `msg` - The debug message to print
pub fn debug_log(msg: &str) {
    if is_debug_enabled() {
        eprintln!("\x1b[1;33m[git-ai]\x1b[0m {}", msg);
    }
}

/// Print a git diff in a readable format
///
/// Prints the diff between two commits/trees showing which files changed and their status.
/// This is useful for debugging and understanding what changes occurred.
///
/// # Arguments
///
/// * `diff` - The git diff object to print
/// * `old_label` - Label for the "old" side (e.g., commit SHA or description)
/// * `new_label` - Label for the "new" side (e.g., commit SHA or description)
pub fn _print_diff(diff: &Diff, old_label: &str, new_label: &str) {
    println!("Diff between {} and {}:", old_label, new_label);

    let mut file_count = 0;
    for delta in diff.deltas() {
        file_count += 1;
        let old_file = delta.old_file().path().unwrap_or(std::path::Path::new(""));
        let new_file = delta.new_file().path().unwrap_or(std::path::Path::new(""));
        let status = delta.status();

        println!(
            "  File {}: {} -> {} (status: {:?})",
            file_count,
            old_file.display(),
            new_file.display(),
            status
        );
    }

    if file_count == 0 {
        println!("  No changes between {} and {}", old_label, new_label);
    }
}


#[inline]
pub fn normalize_to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

pub fn current_git_ai_exe() -> Result<PathBuf, GitAiError> {
    let path = std::env::current_exe()?;
    
    // Get platform-specific executable names
    let git_name = if cfg!(windows) { "git.exe" } else { "git" };
    let git_ai_name = if cfg!(windows) { "git-ai.exe" } else { "git-ai" };
    
    // Check if the filename matches the git executable name for this platform
    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        if file_name == git_name {
            // Try replacing with git-ai executable name for this platform
            let git_ai_path = path.with_file_name(git_ai_name);
            
            // Check if the git-ai file exists
            if git_ai_path.exists() {
                return Ok(git_ai_path);
            }
            
            // If it doesn't exist, return the git-ai executable name as a PathBuf
            return Ok(PathBuf::from(git_ai_name));
        }
    }
    
    Ok(path)
}

/// Get the user's home directory path
pub fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    #[cfg(windows)]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            return PathBuf::from(userprofile);
        }
    }
    PathBuf::from(".")
}

/// Get the git-ai bin directory path
pub fn git_ai_bin_dir() -> PathBuf {
    home_dir().join(".git-ai").join("bin")
}

/// Check if git-ai is disabled by checking if git.disabled exists
pub fn is_git_ai_disabled() -> bool {
    let bin_dir = git_ai_bin_dir();
    let git_disabled_path = bin_dir.join(if cfg!(windows) { "git.disabled.exe" } else { "git.disabled" });
    git_disabled_path.exists()
}

/// Get the path to the git binary in git-ai bin directory
pub fn git_bin_path() -> PathBuf {
    git_ai_bin_dir().join(if cfg!(windows) { "git.exe" } else { "git" })
}

/// Get the path to the git.disabled binary in git-ai bin directory
pub fn git_disabled_bin_path() -> PathBuf {
    git_ai_bin_dir().join(if cfg!(windows) { "git.disabled.exe" } else { "git.disabled" })
}

/// Enable git-ai by renaming git.disabled back to git
pub fn enable_git_ai() -> Result<(), GitAiError> {
    let git_disabled = git_disabled_bin_path();
    let git_path = git_bin_path();
    
    if !git_disabled.exists() {
        return Err(GitAiError::Generic(
            "git-ai is already enabled (git.disabled not found)".to_string(),
        ));
    }
    
    fs::rename(&git_disabled, &git_path)
        .map_err(|e| GitAiError::Generic(format!(
            "Failed to enable git-ai: {}",
            e
        )))?;
    
    Ok(())
}

/// Disable git-ai by renaming git to git.disabled
pub fn disable_git_ai() -> Result<(), GitAiError> {
    let git_path = git_bin_path();
    let git_disabled = git_disabled_bin_path();
    
    if !git_path.exists() {
        return Err(GitAiError::Generic(
            "git-ai is already disabled (git binary not found)".to_string(),
        ));
    }
    
    if git_disabled.exists() {
        return Err(GitAiError::Generic(
            "git-ai is already disabled (git.disabled already exists)".to_string(),
        ));
    }
    
    fs::rename(&git_path, &git_disabled)
        .map_err(|e| GitAiError::Generic(format!(
            "Failed to disable git-ai: {}",
            e
        )))?;
    
    Ok(())
}