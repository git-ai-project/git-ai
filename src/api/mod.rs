pub mod auth;
pub mod cas;
pub mod client;
pub mod metrics;
pub mod rate_limit;

use std::path::PathBuf;

/// Get the user's home directory (shared across api submodules).
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}
