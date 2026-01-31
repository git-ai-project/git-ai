use crate::error::GitAiError;
use crate::git::diff_tree_to_tree::Diff;
use std::io::IsTerminal;
use std::path::PathBuf;

/// Check if debug logging is enabled via environment variable
///
/// This is checked once at module initialization to avoid repeated environment variable lookups.
static DEBUG_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static DEBUG_PERFORMANCE_LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
static IS_TERMINAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn is_debug_enabled() -> bool {
    *DEBUG_ENABLED.get_or_init(|| {
        (cfg!(debug_assertions)
            || std::env::var("GIT_AI_DEBUG").unwrap_or_default() == "1"
            || std::env::var("GIT_AI_DEBUG_PERFORMANCE").unwrap_or_default() != "")
            && std::env::var("GIT_AI_DEBUG").unwrap_or_default() != "0"
    })
}

fn is_debug_performance_enabled() -> bool {
    debug_performance_level() >= 1
}

fn debug_performance_level() -> u8 {
    *DEBUG_PERFORMANCE_LEVEL.get_or_init(|| {
        std::env::var("GIT_AI_DEBUG_PERFORMANCE")
            .unwrap_or_default()
            .parse::<u8>()
            .unwrap_or(0)
    })
}

pub fn debug_performance_log(msg: &str) {
    if is_debug_performance_enabled() {
        eprintln!("\x1b[1;33m[git-ai (perf)]\x1b[0m {}", msg);
    }
}

pub fn debug_performance_log_structured(json: serde_json::Value) {
    if debug_performance_level() >= 2 {
        eprintln!("\x1b[1;33m[git-ai (perf-json)]\x1b[0m {}", json);
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
    let git_ai_name = if cfg!(windows) {
        "git-ai.exe"
    } else {
        "git-ai"
    };

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

pub fn is_interactive_terminal() -> bool {
    *IS_TERMINAL.get_or_init(|| std::io::stdin().is_terminal())
}

/// Windows-specific flag to prevent console window creation
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;
