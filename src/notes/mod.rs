//! Notes backend module.
//!
//! `notes::db` provides the dedicated `~/.git-ai/internal/notes-db` SQLite store
//! used by the HTTP notes backend as both a write queue and a local read cache.
//!
//! `notes::reference_server` is an in-memory reference implementation of the
//! HTTP wire contract — used for local testing, benchmarking, and as
//! documentation of what a real backend must implement.
//!
//! `notes::migration_state` models the per-repo HTTP-backend rollout state
//! (control plane, keyed by normalized remote URL) and resolves it best-effort,
//! defaulting to the no-op `GitNotesOnly`.

pub mod db;
pub mod migration_state;
pub mod reference_server;
