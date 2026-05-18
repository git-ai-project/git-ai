pub mod api;
pub mod auth;
pub mod ci;
pub mod core;
pub mod daemon;
pub mod git_cmd;
pub mod mdm;
pub mod metrics;
pub mod observability;
pub mod paths;
pub mod presets;
pub mod transcripts;

pub mod authorship {
    pub mod authorship_log_serialization {
        pub use crate::core::authorship_log::{
            AttestationEntry, AuthorshipLog, FileAttestation, generate_short_hash,
        };
    }
    pub mod authorship_log {
        pub use crate::core::authorship_log::{
            AgentId, AttestationEntry, AuthorshipLog, FileAttestation, HumanRecord, LineRange,
            Metadata, PromptRecord, SessionRecord, generate_human_hash, generate_session_id,
            generate_short_hash,
        };
    }
    pub mod working_log {
        pub use crate::core::working_log::{AgentId, Checkpoint, CheckpointKind};
    }
    pub mod stats {
        pub use crate::metrics::cache::{CommitStats, FileStats, StatsCache};
    }
    pub mod attribution_tracker {
        pub use crate::core::attribution::LineAttribution;
    }
}

pub mod error {
    pub type GitAiError = String;
}

pub mod commands {
    pub mod checkpoint_agent {
        pub mod presets {
            pub use crate::presets::{
                KnownHumanEdit, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit, UntrackedEdit,
            };
            pub use crate::presets::{ParsedHookEvent, PresetContext};

            pub struct ResolvedPreset {
                agent_name: String,
            }

            impl ResolvedPreset {
                pub fn parse(
                    &self,
                    hook_input: &str,
                    _session_hint: &str,
                ) -> Result<Vec<ParsedHookEvent>, String> {
                    crate::presets::parse_hook_input(&self.agent_name, hook_input)
                }
            }

            pub fn resolve_preset(name: &str) -> Result<ResolvedPreset, String> {
                Ok(ResolvedPreset {
                    agent_name: name.to_string(),
                })
            }
        }
    }
}

pub mod git {
    pub mod refs {
        use std::path::Path;

        use crate::core::git_binary::git_cmd as git_command;

        /// Add or replace a git note in the `refs/notes/ai` namespace.
        pub fn notes_add(repo_path: &Path, commit_sha: &str, note_content: &str) -> bool {
            let result = git_command()
                .arg("-C")
                .arg(repo_path)
                .args([
                    "notes",
                    "--ref=ai",
                    "add",
                    "-f",
                    "-m",
                    note_content,
                    commit_sha,
                ])
                .output();
            matches!(result, Ok(o) if o.status.success())
        }
    }
}
