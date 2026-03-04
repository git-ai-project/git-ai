use crate::git::rewrite_log::RewriteLogEvent;
use serde::{Deserialize, Serialize};

/// Async job payload sent over the socket to the worker.
///
/// Contains all the state needed to reconstruct a Repository and replay
/// a rewrite log event without referencing the worktree directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncJob {
    /// The type of job to execute
    pub job_type: AsyncJobType,

    /// Global args for reconstructing the Repository (e.g., ["-C", "/path/to/repo"])
    pub repo_global_args: Vec<String>,

    /// Path to the .git directory
    pub git_dir: String,

    /// Path to the shared git directory (same as git_dir for non-worktree repos)
    pub git_common_dir: String,

    /// Path to the working directory
    pub workdir: String,

    /// The rewrite log event to process
    pub rewrite_log_event: RewriteLogEvent,

    /// The commit author string (e.g., "Name <email>")
    pub commit_author: String,

    /// Whether to suppress output
    pub suppress_output: bool,

    /// Whether to apply side effects (authorship rewriting)
    pub apply_side_effects: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncJobType {
    RewriteLogEvent,
}

impl AsyncJob {
    /// Serialize the job to a length-prefixed message for socket transmission.
    /// Format: [4-byte big-endian length][JSON payload]
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let json = serde_json::to_vec(self)?;
        let len = json.len() as u32;
        let mut buf = Vec::with_capacity(4 + json.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&json);
        Ok(buf)
    }

    /// Deserialize a job from a JSON byte slice (without the length prefix).
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::rewrite_log::RewriteLogEvent;

    #[test]
    fn test_async_job_roundtrip() {
        let job = AsyncJob {
            job_type: AsyncJobType::RewriteLogEvent,
            repo_global_args: vec!["-C".to_string(), "/tmp/test-repo".to_string()],
            git_dir: "/tmp/test-repo/.git".to_string(),
            git_common_dir: "/tmp/test-repo/.git".to_string(),
            workdir: "/tmp/test-repo".to_string(),
            rewrite_log_event: RewriteLogEvent::commit(
                Some("abc123".to_string()),
                "def456".to_string(),
            ),
            commit_author: "Test User <test@example.com>".to_string(),
            suppress_output: false,
            apply_side_effects: true,
        };

        let wire_bytes = job.to_wire_bytes().expect("serialization should succeed");
        assert!(wire_bytes.len() > 4);

        // Verify length prefix
        let len = u32::from_be_bytes([wire_bytes[0], wire_bytes[1], wire_bytes[2], wire_bytes[3]]);
        assert_eq!(len as usize, wire_bytes.len() - 4);

        // Verify roundtrip
        let deserialized =
            AsyncJob::from_json_bytes(&wire_bytes[4..]).expect("deserialization should succeed");
        assert_eq!(deserialized.git_dir, "/tmp/test-repo/.git");
        assert_eq!(deserialized.commit_author, "Test User <test@example.com>");
        assert!(deserialized.apply_side_effects);
    }

    #[test]
    fn test_async_job_commit_amend_roundtrip() {
        let job = AsyncJob {
            job_type: AsyncJobType::RewriteLogEvent,
            repo_global_args: vec!["-C".to_string(), "/tmp/repo".to_string()],
            git_dir: "/tmp/repo/.git".to_string(),
            git_common_dir: "/tmp/repo/.git".to_string(),
            workdir: "/tmp/repo".to_string(),
            rewrite_log_event: RewriteLogEvent::commit_amend(
                "old_sha".to_string(),
                "new_sha".to_string(),
            ),
            commit_author: "Author <a@b.com>".to_string(),
            suppress_output: true,
            apply_side_effects: true,
        };

        let wire_bytes = job.to_wire_bytes().unwrap();
        let deserialized = AsyncJob::from_json_bytes(&wire_bytes[4..]).unwrap();

        match &deserialized.rewrite_log_event {
            RewriteLogEvent::CommitAmend { commit_amend } => {
                assert_eq!(commit_amend.original_commit, "old_sha");
                assert_eq!(commit_amend.amended_commit_sha, "new_sha");
            }
            _ => panic!("Expected CommitAmend event"),
        }
    }
}
