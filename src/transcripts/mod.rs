//! Transcript processing module for tracking and reading AI agent transcripts.
//!
//! This module provides:
//! - Watermarking strategies for incremental transcript processing
//! - SQLite database for session tracking and state persistence
//! - Error types for transcript processing failures
//!
//! # Architecture
//!
//! The transcripts module is designed to work with the daemon worker to:
//! 1. Track transcript files for multiple AI agents (Claude Code, Cursor, etc.)
//! 2. Maintain processing state via watermarks (byte offsets, record indices, timestamps)
//! 3. Emit telemetry events from transcript data
//!
//! # Example
//!
//! ```ignore
//! use crate::transcripts::{TranscriptsDatabase, SessionRecord};
//! use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};
//!
//! // Open database
//! let db = TranscriptsDatabase::open("~/.git-ai/transcripts-db")?;
//!
//! // Create session with watermark
//! let session = SessionRecord {
//!     session_id: "session-123".to_string(),
//!     agent_type: "claude-code".to_string(),
//!     transcript_path: "/path/to/transcript.jsonl".to_string(),
//!     watermark_type: "ByteOffset".to_string(),
//!     watermark_value: "0".to_string(),
//!     // ... other fields
//! };
//! db.insert_session(&session)?;
//!
//! // Process transcript and update watermark
//! let mut watermark = ByteOffsetWatermark::new(0);
//! // ... read and process transcript ...
//! watermark.advance(1024, 10);
//! db.update_watermark("session-123", &watermark)?;
//! ```

pub mod agent;
pub mod agents;
pub mod db;
pub mod model_extraction;
pub mod sweep;
pub mod types;
pub mod watermark;

// Re-export main types for convenient access
pub use db::{SessionRecord, TranscriptsDatabase};
pub use types::{TranscriptBatch, TranscriptError};
pub use watermark::{
    ByteOffsetWatermark, HybridWatermark, RecordIndexWatermark, TimestampWatermark,
    WatermarkStrategy, WatermarkType,
};
