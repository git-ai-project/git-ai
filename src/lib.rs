pub mod api;
pub mod ci;
pub mod core;
pub mod daemon;
pub mod mdm;
pub mod metrics;
pub mod observability;
pub mod presets;
pub mod transcripts;

pub mod authorship {
    pub mod authorship_log_serialization {
        pub use crate::core::authorship_log::AuthorshipLog;
    }
    pub mod authorship_log {
        pub use crate::core::authorship_log::{
            AgentId, AuthorshipLog, FileAttestation, AttestationEntry, HumanRecord, LineRange,
            Metadata, PromptRecord, SessionRecord, generate_short_hash, generate_session_id,
            generate_human_hash,
        };
    }
    pub mod working_log {
        pub use crate::core::working_log::{AgentId, Checkpoint, CheckpointKind};
    }
}

pub mod git {
    pub mod refs {
        use std::path::Path;
        use std::process::Command;

        /// Add or replace a git note in the `refs/notes/ai` namespace.
        pub fn notes_add(repo_path: &Path, commit_sha: &str, note_content: &str) -> bool {
            let result = Command::new("/usr/bin/git")
                .arg("-C")
                .arg(repo_path)
                .args(["notes", "--ref=ai", "add", "-f", "-m", note_content, commit_sha])
                .output();
            matches!(result, Ok(o) if o.status.success())
        }
    }
}
