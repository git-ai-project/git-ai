//! Format-specific transcript readers.
//!
//! Each module implements incremental reading of a specific agent's transcript format.

pub mod claude;

// Re-export reader functions for convenience
pub use claude::read_incremental as read_claude_incremental;
