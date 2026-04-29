//! Format-specific transcript readers.
//!
//! Each module implements incremental reading of a specific agent's transcript format.

pub mod claude;
pub mod copilot;
pub mod cursor;
pub mod droid;

// Re-export reader functions for convenience
pub use claude::read_incremental as read_claude_incremental;
pub use copilot::{
    read_event_stream as read_copilot_event_stream, read_session_json as read_copilot_session_json,
};
pub use cursor::read_incremental as read_cursor_incremental;
pub use droid::read_incremental as read_droid_incremental;
