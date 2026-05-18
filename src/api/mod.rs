pub mod auth;
pub mod cas;
pub mod client;
pub mod metrics;
pub mod rate_limit;

use std::path::PathBuf;

/// Get the user's home directory (shared across api submodules).
pub fn home_dir() -> Option<PathBuf> {
    crate::paths::home_dir()
}
