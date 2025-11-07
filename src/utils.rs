use crate::git::diff_tree_to_tree::Diff;

/// Normalize a file path to Git's format (forward slashes, relative)
///
/// Git always uses forward slashes for paths, even on Windows.
/// This function converts OS-native paths to Git's normalized format.
///
/// # Arguments
///
/// * `path` - The file path to normalize (can be absolute or relative, with any separator)
///
/// # Returns
///
/// A normalized path with forward slashes, relative to the working directory
pub fn normalize_path_to_git_format(path: &str) -> String {
    // Replace backslashes with forward slashes for Windows compatibility
    // Git always uses forward slashes internally, even on Windows
    path.replace('\\', "/")
}

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
