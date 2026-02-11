use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::working_log::{CHECKPOINT_API_VERSION, Checkpoint, CheckpointKind};
use crate::error::GitAiError;
use crate::git::rewrite_log::{RewriteLogEvent, append_event_to_file};
use crate::utils::{debug_log, normalize_to_posix};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Initial attributions data structure stored in the INITIAL file
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitialAttributions {
    /// Map of file path to line attributions
    pub files: HashMap<String, Vec<LineAttribution>>,
    /// Map of author_id (hash) to PromptRecord for prompt tracking
    pub prompts: HashMap<String, PromptRecord>,
}

#[derive(Debug, Clone)]
pub struct RepoStorage {
    pub repo_path: PathBuf,
    pub repo_workdir: PathBuf,
    pub working_logs: PathBuf,
    pub rewrite_log: PathBuf,
    pub logs: PathBuf,
}

impl RepoStorage {
    pub fn for_repo_path(repo_path: &Path, repo_workdir: &Path) -> RepoStorage {
        let ai_dir = repo_path.join("ai");
        let working_logs_dir = ai_dir.join("working_logs");
        let rewrite_log_file = ai_dir.join("rewrite_log");
        let logs_dir = ai_dir.join("logs");

        let config = RepoStorage {
            repo_path: repo_path.to_path_buf(),
            repo_workdir: repo_workdir.to_path_buf(),
            working_logs: working_logs_dir,
            rewrite_log: rewrite_log_file,
            logs: logs_dir,
        };

        config.ensure_config_directory().unwrap();
        config
    }

    fn ensure_config_directory(&self) -> Result<(), GitAiError> {
        let ai_dir = self.repo_path.join("ai");

        fs::create_dir_all(ai_dir)?;

        // Create working_logs directory
        fs::create_dir_all(&self.working_logs)?;

        // Create logs directory for Sentry events
        fs::create_dir_all(&self.logs)?;

        if !&self.rewrite_log.exists() && !&self.rewrite_log.is_file() {
            fs::write(&self.rewrite_log, "")?;
        }

        Ok(())
    }

    /* Working Log Persistance */

    pub fn working_log_for_base_commit(&self, sha: &str) -> PersistedWorkingLog {
        let working_log_dir = self.working_logs.join(sha);
        fs::create_dir_all(&working_log_dir).unwrap();
        let canonical_workdir = self
            .repo_workdir
            .canonicalize()
            .unwrap_or_else(|_| self.repo_workdir.clone());
        PersistedWorkingLog::new(
            working_log_dir,
            sha,
            self.repo_workdir.clone(),
            canonical_workdir,
            None,
        )
    }

    #[allow(dead_code)]
    pub fn delete_working_log_for_base_commit(&self, sha: &str) -> Result<(), GitAiError> {
        let working_log_dir = self.working_logs.join(sha);
        if working_log_dir.exists() {
            if cfg!(debug_assertions) {
                // In debug mode, move to old-{sha} instead of deleting
                let old_dir = self.working_logs.join(format!("old-{}", sha));
                // If old-{sha} already exists, remove it first
                if old_dir.exists() {
                    fs::remove_dir_all(&old_dir)?;
                }
                fs::rename(&working_log_dir, &old_dir)?;
                debug_log(&format!(
                    "Debug mode: moved checkpoint directory from {} to old-{}",
                    sha, sha
                ));
            } else {
                // In non-debug mode, delete as before
                fs::remove_dir_all(&working_log_dir)?;
            }
        }
        Ok(())
    }

    /// Rename a working log directory from one commit SHA to another.
    /// Used when fast-forward pull changes HEAD but preserves working directory state.
    /// Only renames if old directory exists and new directory doesn't exist.
    pub fn rename_working_log(&self, old_sha: &str, new_sha: &str) -> Result<(), GitAiError> {
        let old_dir = self.working_logs.join(old_sha);
        let new_dir = self.working_logs.join(new_sha);
        if old_dir.exists() && !new_dir.exists() {
            fs::rename(&old_dir, &new_dir)?;
            debug_log(&format!(
                "Renamed working log from {} to {}",
                old_sha, new_sha
            ));
        }
        Ok(())
    }

    /* Rewrite Log Persistance */

    /// Append a rewrite event to the rewrite log file and return the full log
    pub fn append_rewrite_event(
        &self,
        event: RewriteLogEvent,
    ) -> Result<Vec<RewriteLogEvent>, GitAiError> {
        append_event_to_file(&self.rewrite_log, event)?;
        self.read_rewrite_events()
    }

    /// Read all rewrite events from the rewrite log file
    pub fn read_rewrite_events(&self) -> Result<Vec<RewriteLogEvent>, GitAiError> {
        if !self.rewrite_log.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.rewrite_log)?;
        crate::git::rewrite_log::deserialize_events_from_jsonl(&content)
    }
}

#[derive(Clone)]
pub struct PersistedWorkingLog {
    pub dir: PathBuf,
    #[allow(dead_code)]
    pub base_commit: String,
    pub repo_workdir: PathBuf,
    /// Canonical (absolute, resolved) version of workdir for reliable path comparisons
    /// On Windows, this uses the \\?\ UNC prefix format
    #[allow(dead_code)]
    pub canonical_workdir: PathBuf,
    pub dirty_files: Option<HashMap<String, String>>,
    pub initial_file: PathBuf,
}

impl PersistedWorkingLog {
    pub fn new(
        dir: PathBuf,
        base_commit: &str,
        repo_root: PathBuf,
        canonical_workdir: PathBuf,
        dirty_files: Option<HashMap<String, String>>,
    ) -> Self {
        let initial_file = dir.join("INITIAL");
        Self {
            dir,
            base_commit: base_commit.to_string(),
            repo_workdir: repo_root,
            canonical_workdir,
            dirty_files,
            initial_file,
        }
    }

    pub fn set_dirty_files(&mut self, dirty_files: Option<HashMap<String, String>>) {
        let normalized_dirty_files = dirty_files.map(|map| {
            map.into_iter()
                .map(|(file_path, content)| {
                    let relative_path = self.to_repo_relative_path(&file_path);
                    let normalized_path = normalize_to_posix(&relative_path);
                    (normalized_path, content)
                })
                .collect::<HashMap<_, _>>()
        });

        self.dirty_files = normalized_dirty_files;
    }

    pub fn reset_working_log(&self) -> Result<(), GitAiError> {
        // Clear all blobs by removing the blobs directory
        let blobs_dir = self.dir.join("blobs");
        if blobs_dir.exists() {
            fs::remove_dir_all(&blobs_dir)?;
        }

        // Clear checkpoints by truncating the JSONL file
        let checkpoints_file = self.dir.join("checkpoints.jsonl");
        fs::write(&checkpoints_file, "")?;

        // Clear INITIAL attributions file so stale attributions from a
        // previous working state do not persist across resets
        if self.initial_file.exists() {
            fs::remove_file(&self.initial_file)?;
        }

        Ok(())
    }

    /* blob storage */
    pub fn get_file_version(&self, sha: &str) -> Result<String, GitAiError> {
        let blob_path = self.dir.join("blobs").join(sha);
        Ok(fs::read_to_string(blob_path)?)
    }

    #[allow(dead_code)]
    pub fn persist_file_version(&self, content: &str) -> Result<String, GitAiError> {
        // Create SHA256 hash of the content
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let sha = format!("{:x}", hasher.finalize());

        // Ensure blobs directory exists
        let blobs_dir = self.dir.join("blobs");
        fs::create_dir_all(&blobs_dir)?;

        // Write content to blob file
        let blob_path = blobs_dir.join(&sha);
        fs::write(blob_path, content)?;

        Ok(sha)
    }

    pub fn to_repo_absolute_path(&self, file_path: &str) -> String {
        if Path::new(file_path).is_absolute() {
            return file_path.to_string();
        }
        self.repo_workdir
            .join(file_path)
            .to_string_lossy()
            .to_string()
    }

    pub fn to_repo_relative_path(&self, file_path: &str) -> String {
        if !Path::new(file_path).is_absolute() {
            return file_path.to_string();
        }
        let path = Path::new(file_path);

        // Try without canonicalizing first
        if path.starts_with(&self.repo_workdir) {
            return path
                .strip_prefix(&self.repo_workdir)
                .unwrap()
                .to_string_lossy()
                .to_string();
        }

        // If we couldn't match yet, try canonicalizing both repo_workdir and the input path
        // On Windows, this uses the canonical_workdir that was pre-computed
        #[cfg(windows)]
        let canonical_workdir = &self.canonical_workdir;

        #[cfg(not(windows))]
        let canonical_workdir = match self.repo_workdir.canonicalize() {
            Ok(p) => p,
            Err(_) => self.repo_workdir.clone(),
        };

        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };

        #[cfg(windows)]
        if canonical_path.starts_with(canonical_workdir) {
            return canonical_path
                .strip_prefix(canonical_workdir)
                .unwrap()
                .to_string_lossy()
                .to_string();
        }

        #[cfg(not(windows))]
        if canonical_path.starts_with(&canonical_workdir) {
            return canonical_path
                .strip_prefix(&canonical_workdir)
                .unwrap()
                .to_string_lossy()
                .to_string();
        }

        file_path.to_string()
    }

    pub fn read_current_file_content(&self, file_path: &str) -> Result<String, GitAiError> {
        // First try to read from dirty_files (using raw path)
        if let Some(ref dirty_files) = self.dirty_files
            && let Some(content) = dirty_files.get(&file_path.to_string())
        {
            return Ok(content.clone());
        }

        let file_path = self.to_repo_absolute_path(file_path);

        // Fall back to reading from filesystem
        match fs::read(&file_path) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
            Err(_) => Ok(String::new()),
        }
    }

    /* append checkpoint */
    pub fn append_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), GitAiError> {
        // Read existing checkpoints
        let mut checkpoints = self.read_all_checkpoints().unwrap_or_default();

        // Create a copy, potentially without transcript to reduce storage size.
        // Transcripts are refetched in update_prompts_to_latest() before post-commit
        // using tool-specific sources (transcript_path for Claude, cursor_db_path for Cursor, etc.)
        //
        // Tools that DON'T support refetch (transcript must be kept):
        // - "opencode" - uses agent-v1 format, transcript provided inline
        // - "mock_ai" - test preset, transcript not stored externally
        // - Any other agent-v1 custom tools (detected by lack of tool-specific metadata)
        let mut storage_checkpoint = checkpoint.clone();
        let tool = checkpoint
            .agent_id
            .as_ref()
            .map(|a| a.tool.as_str())
            .unwrap_or("");
        let metadata = &checkpoint.agent_metadata;

        // Blacklist: tools that cannot refetch transcripts
        let cannot_refetch = match tool {
            "opencode" | "mock_ai" => true,
            // human checkpoints have no transcript anyway
            "human" => false,
            // For other tools, check if they have the necessary metadata for refetching
            // cursor can always refetch from its database
            "cursor" => false,
            // claude, codex, gemini, continue-cli need transcript_path
            "claude" | "codex" | "gemini" | "continue-cli" => metadata
                .as_ref()
                .and_then(|m| m.get("transcript_path"))
                .is_none(),
            // github-copilot needs chat_session_path
            "github-copilot" => metadata
                .as_ref()
                .and_then(|m| m.get("chat_session_path"))
                .is_none(),
            // Unknown tools (like custom agent-v1 tools) can't refetch
            _ => true,
        };

        if !cannot_refetch {
            storage_checkpoint.transcript = None;
        }

        // Add the new checkpoint
        checkpoints.push(storage_checkpoint);

        // Prune char-level attributions from older checkpoints for the same files
        // Only the most recent checkpoint per file needs char-level precision
        self.prune_old_char_attributions(&mut checkpoints);

        // Write all checkpoints back
        self.write_all_checkpoints(&checkpoints)
    }

    pub fn read_all_checkpoints(&self) -> Result<Vec<Checkpoint>, GitAiError> {
        let checkpoints_file = self.dir.join("checkpoints.jsonl");

        if !checkpoints_file.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&checkpoints_file)?;
        let mut checkpoints = Vec::new();

        // Parse JSONL file - each line is a separate JSON object
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let checkpoint: Checkpoint = serde_json::from_str(line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            if checkpoint.api_version != CHECKPOINT_API_VERSION {
                debug_log(&format!(
                    "unsupported checkpoint api version: {} (silently skipping checkpoint)",
                    checkpoint.api_version
                ));
                continue;
            }

            checkpoints.push(checkpoint);
        }

        // Migrate 7-char prompt hashes to 16-char hashes
        // Step 1: Build mapping from old 7-char hash to new 16-char hash
        let mut old_to_new_hash: HashMap<String, String> = HashMap::new();

        for checkpoint in &checkpoints {
            if let Some(agent_id) = &checkpoint.agent_id {
                let new_hash = generate_short_hash(&agent_id.id, &agent_id.tool);
                let old_hash = new_hash[..7].to_string();
                old_to_new_hash.insert(old_hash, new_hash);
            }
        }

        // Step 2: Replace 7-char author_ids in all checkpoints' attributions and line_attributions
        let mut migrated_checkpoints = Vec::new();
        for mut checkpoint in checkpoints {
            for entry in &mut checkpoint.entries {
                // Replace author_ids in attributions
                for attr in &mut entry.attributions {
                    if attr.author_id.len() == 7
                        && let Some(new_hash) = old_to_new_hash.get(&attr.author_id)
                    {
                        attr.author_id = new_hash.clone();
                    }
                }

                // Replace author_ids in line_attributions
                for line_attr in &mut entry.line_attributions {
                    if line_attr.author_id.len() == 7
                        && let Some(new_hash) = old_to_new_hash.get(&line_attr.author_id)
                    {
                        line_attr.author_id = new_hash.clone();
                    }
                    // Also migrate the overrode field if it contains a 7-char hash
                    if let Some(ref overrode_id) = line_attr.overrode
                        && overrode_id.len() == 7
                        && let Some(new_hash) = old_to_new_hash.get(overrode_id)
                    {
                        line_attr.overrode = Some(new_hash.clone());
                    }
                }
            }
            migrated_checkpoints.push(checkpoint);
        }

        Ok(migrated_checkpoints)
    }

    /// Remove char-level attributions from all but the most recent checkpoint per file.
    /// This reduces storage size while preserving precision for the entries that matter.
    /// Only the most recent checkpoint entry for each file is used when computing new entries.
    fn prune_old_char_attributions(&self, checkpoints: &mut [Checkpoint]) {
        // Track which checkpoint index has the most recent entry for each file
        // Iterate from newest to oldest
        let mut newest_for_file: HashMap<String, usize> = HashMap::new();

        for (checkpoint_idx, checkpoint) in checkpoints.iter().enumerate().rev() {
            for entry in &checkpoint.entries {
                newest_for_file
                    .entry(entry.file.clone())
                    .or_insert(checkpoint_idx);
            }
        }

        // Clear attributions from entries that aren't the most recent for their file
        for (checkpoint_idx, checkpoint) in checkpoints.iter_mut().enumerate() {
            for entry in &mut checkpoint.entries {
                if let Some(&newest_idx) = newest_for_file.get(&entry.file)
                    && checkpoint_idx != newest_idx
                {
                    entry.attributions.clear();
                }
            }
        }
    }

    /// Write all checkpoints to the JSONL file, replacing any existing content
    /// Note: Unlike append_checkpoint(), this preserves transcripts because it's used
    /// by post-commit after transcripts have been refetched and need to be preserved
    /// for from_just_working_log() to read them.
    pub fn write_all_checkpoints(&self, checkpoints: &[Checkpoint]) -> Result<(), GitAiError> {
        let checkpoints_file = self.dir.join("checkpoints.jsonl");

        // Serialize all checkpoints to JSONL
        let mut lines = Vec::new();
        for checkpoint in checkpoints {
            let json_line = serde_json::to_string(checkpoint)?;
            lines.push(json_line);
        }

        // Write all lines to file
        let content = lines.join("\n");
        if !content.is_empty() {
            fs::write(&checkpoints_file, format!("{}\n", content))?;
        } else {
            fs::write(&checkpoints_file, "")?;
        }

        Ok(())
    }

    pub fn all_touched_files(&self) -> Result<HashSet<String>, GitAiError> {
        let checkpoints = self.read_all_checkpoints()?;
        let mut touched_files = HashSet::new();
        for checkpoint in checkpoints {
            for entry in checkpoint.entries {
                touched_files.insert(entry.file);
            }
        }
        Ok(touched_files)
    }

    #[allow(dead_code)]
    pub fn all_ai_touched_files(&self) -> Result<HashSet<String>, GitAiError> {
        let checkpoints = self.read_all_checkpoints()?;
        let mut touched_files = HashSet::new();
        for checkpoint in checkpoints {
            // Only include files from AI checkpoints (AiAgent or AiTab)
            match checkpoint.kind {
                CheckpointKind::AiAgent | CheckpointKind::AiTab => {
                    for entry in checkpoint.entries {
                        touched_files.insert(entry.file);
                    }
                }
                CheckpointKind::Human => {
                    // Skip human checkpoints
                }
            }
        }
        Ok(touched_files)
    }

    /* INITIAL attributions file */

    /// Write initial attributions to the INITIAL file.
    /// This seeds the working log with known attributions from rewrite operations.
    /// Only writes files that have non-empty attributions.
    pub fn write_initial_attributions(
        &self,
        attributions: HashMap<String, Vec<LineAttribution>>,
        prompts: HashMap<String, PromptRecord>,
    ) -> Result<(), GitAiError> {
        // Filter out empty attributions
        let filtered: HashMap<String, Vec<LineAttribution>> = attributions
            .into_iter()
            .filter(|(_, attrs)| !attrs.is_empty())
            .collect();

        if filtered.is_empty() {
            // Don't create an INITIAL file if there are no attributions
            return Ok(());
        }

        let initial_data = InitialAttributions {
            files: filtered,
            prompts,
        };

        let json = serde_json::to_string_pretty(&initial_data)?;
        fs::write(&self.initial_file, json)?;

        Ok(())
    }

    /// Read initial attributions from the INITIAL file.
    /// Returns empty attributions and prompts if the file doesn't exist.
    pub fn read_initial_attributions(&self) -> InitialAttributions {
        if !self.initial_file.exists() {
            return InitialAttributions::default();
        }

        match fs::read_to_string(&self.initial_file) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(initial_data) => initial_data,
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse INITIAL file: {}. Returning empty.",
                        e
                    ));
                    InitialAttributions::default()
                }
            },
            Err(e) => {
                debug_log(&format!(
                    "Failed to read INITIAL file: {}. Returning empty.",
                    e
                ));
                InitialAttributions::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::git::test_utils::TmpRepo;

    use super::*;
    use std::fs;

    #[test]
    fn test_ensure_config_directory_creates_structure() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage
        let _repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());

        // Verify .git/ai directory exists
        let ai_dir = tmp_repo.repo().path().join("ai");
        assert!(ai_dir.exists(), ".git/ai directory should exist");
        assert!(ai_dir.is_dir(), ".git/ai should be a directory");

        // Verify working_logs directory exists
        let working_logs_dir = ai_dir.join("working_logs");
        assert!(
            working_logs_dir.exists(),
            "working_logs directory should exist"
        );
        assert!(
            working_logs_dir.is_dir(),
            "working_logs should be a directory"
        );

        // Verify rewrite_log file exists and is empty
        let rewrite_log_file = ai_dir.join("rewrite_log");
        assert!(rewrite_log_file.exists(), "rewrite_log file should exist");
        assert!(rewrite_log_file.is_file(), "rewrite_log should be a file");

        let content = fs::read_to_string(&rewrite_log_file).expect("Failed to read rewrite_log");
        assert_eq!(content, "", "rewrite_log should be empty by default");
    }

    #[test]
    fn test_ensure_config_directory_handles_existing_files() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());

        // Add some content to rewrite_log
        let rewrite_log_file = tmp_repo.repo().path().join("ai").join("rewrite_log");
        fs::write(&rewrite_log_file, "existing content").expect("Failed to write to rewrite_log");

        // Second call - should not overwrite existing file
        repo_storage
            .ensure_config_directory()
            .expect("Failed to ensure config directory again");

        // Verify the content is preserved
        let content = fs::read_to_string(&rewrite_log_file).expect("Failed to read rewrite_log");
        assert_eq!(
            content, "existing content",
            "Existing rewrite_log content should be preserved"
        );

        // Verify directories still exist
        let ai_dir = tmp_repo.repo().path().join("ai");
        let working_logs_dir = ai_dir.join("working_logs");
        assert!(ai_dir.exists(), ".git/ai directory should still exist");
        assert!(
            working_logs_dir.exists(),
            "working_logs directory should still exist"
        );
    }

    #[test]
    fn test_persisted_working_log_blob_storage() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage and PersistedWorkingLog
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());
        let working_log = repo_storage.working_log_for_base_commit("test-commit-sha");

        // Test persisting a file version
        let content = "Hello, World!\nThis is a test file.";
        let sha = working_log
            .persist_file_version(content)
            .expect("Failed to persist file version");

        // Verify the SHA is not empty
        assert!(!sha.is_empty(), "SHA should not be empty");

        // Test retrieving the file version
        let retrieved_content = working_log
            .get_file_version(&sha)
            .expect("Failed to get file version");

        assert_eq!(
            content, retrieved_content,
            "Retrieved content should match original"
        );

        // Verify the blob file exists
        let blob_path = working_log.dir.join("blobs").join(&sha);
        assert!(blob_path.exists(), "Blob file should exist");
        assert!(blob_path.is_file(), "Blob should be a file");

        // Test persisting the same content again should return the same SHA
        let sha2 = working_log
            .persist_file_version(content)
            .expect("Failed to persist file version again");

        assert_eq!(sha, sha2, "Same content should produce same SHA");
    }

    #[test]
    fn test_persisted_working_log_checkpoint_storage() {
        use crate::authorship::working_log::CheckpointKind;

        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage and PersistedWorkingLog
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());
        let working_log = repo_storage.working_log_for_base_commit("test-commit-sha");

        // Create a test checkpoint
        let checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            "test-diff".to_string(),
            "test-author".to_string(),
            vec![], // empty entries for simplicity
        );

        // Test appending checkpoint
        working_log
            .append_checkpoint(&checkpoint)
            .expect("Failed to append checkpoint");

        // Test reading all checkpoints
        let checkpoints = working_log
            .read_all_checkpoints()
            .expect("Failed to read checkpoints");

        assert_eq!(checkpoints.len(), 1, "Should have one checkpoint");
        assert_eq!(checkpoints[0].author, "test-author");

        // Verify the JSONL file exists
        let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
        assert!(checkpoints_file.exists(), "Checkpoints file should exist");

        // Test appending another checkpoint
        let checkpoint2 = Checkpoint::new(
            CheckpointKind::Human,
            "test-diff-2".to_string(),
            "test-author-2".to_string(),
            vec![],
        );

        working_log
            .append_checkpoint(&checkpoint2)
            .expect("Failed to append second checkpoint");

        let checkpoints = working_log
            .read_all_checkpoints()
            .expect("Failed to read checkpoints after second append");

        assert_eq!(checkpoints.len(), 2, "Should have two checkpoints");
        assert_eq!(checkpoints[1].author, "test-author-2");
    }

    #[test]
    fn test_read_all_checkpoints_filters_incompatible_versions() {
        use crate::authorship::working_log::CheckpointKind;

        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage and PersistedWorkingLog
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());
        let working_log = repo_storage.working_log_for_base_commit("test-commit-sha");

        // Build three checkpoints: missing version, wrong version, and correct version
        let base_checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            "diff --git a/file b/file".to_string(),
            "base-author".to_string(),
            vec![],
        );

        let missing_version_json = {
            let mut value = serde_json::to_value(&base_checkpoint).unwrap();
            if let serde_json::Value::Object(ref mut map) = value {
                map.remove("api_version");
            }
            serde_json::to_string(&value).unwrap()
        };

        let mut wrong_version_checkpoint = base_checkpoint.clone();
        wrong_version_checkpoint.api_version = "checkpoint/0.9.0".to_string();
        let wrong_version_json = serde_json::to_string(&wrong_version_checkpoint).unwrap();

        let mut correct_checkpoint = base_checkpoint.clone();
        correct_checkpoint.author = "correct-author".to_string();
        let correct_json = serde_json::to_string(&correct_checkpoint).unwrap();

        let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
        let combined = [missing_version_json, wrong_version_json, correct_json].join("\n");
        fs::write(&checkpoints_file, combined).expect("Failed to write checkpoints.jsonl");

        let checkpoints = working_log
            .read_all_checkpoints()
            .expect("Failed to read checkpoints");

        assert_eq!(
            checkpoints.len(),
            1,
            "Only the correct version should remain"
        );
        assert_eq!(checkpoints[0].author, "correct-author");
        assert_eq!(checkpoints[0].api_version, CHECKPOINT_API_VERSION);
    }

    #[test]
    fn test_persisted_working_log_reset() {
        use crate::authorship::working_log::CheckpointKind;

        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage and PersistedWorkingLog
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());
        let working_log = repo_storage.working_log_for_base_commit("test-commit-sha");

        // Add some blobs
        let content = "Test content";
        let sha = working_log
            .persist_file_version(content)
            .expect("Failed to persist file version");

        // Add some checkpoints
        let checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            "test-diff".to_string(),
            "test-author".to_string(),
            vec![],
        );
        working_log
            .append_checkpoint(&checkpoint)
            .expect("Failed to append checkpoint");

        // Verify they exist
        assert!(working_log.dir.join("blobs").join(&sha).exists());
        let checkpoints = working_log
            .read_all_checkpoints()
            .expect("Failed to read checkpoints");
        assert_eq!(checkpoints.len(), 1);

        // Reset the working log
        working_log
            .reset_working_log()
            .expect("Failed to reset working log");

        // Verify blobs are cleared
        assert!(
            !working_log.dir.join("blobs").exists(),
            "Blobs directory should be removed"
        );

        // Verify checkpoints are cleared
        let checkpoints = working_log
            .read_all_checkpoints()
            .expect("Failed to read checkpoints after reset");
        assert_eq!(
            checkpoints.len(),
            0,
            "Should have no checkpoints after reset"
        );

        // Verify checkpoints.jsonl exists but is empty
        let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
        assert!(
            checkpoints_file.exists(),
            "Checkpoints file should still exist"
        );
        let content =
            fs::read_to_string(&checkpoints_file).expect("Failed to read checkpoints file");
        assert!(
            content.trim().is_empty(),
            "Checkpoints file should be empty"
        );
    }

    #[test]
    fn test_working_log_for_base_commit_creates_directory() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create RepoStorage
        let repo_storage =
            RepoStorage::for_repo_path(tmp_repo.repo().path(), tmp_repo.repo().workdir().unwrap());

        // Create working log for a specific commit
        let commit_sha = "abc123def456";
        let working_log = repo_storage.working_log_for_base_commit(commit_sha);

        // Verify the directory was created
        assert!(
            working_log.dir.exists(),
            "Working log directory should exist"
        );
        assert!(
            working_log.dir.is_dir(),
            "Working log should be a directory"
        );

        // Verify it's in the correct location
        let expected_path = tmp_repo
            .repo()
            .path()
            .join("ai")
            .join("working_logs")
            .join(commit_sha);
        assert_eq!(
            working_log.dir, expected_path,
            "Working log directory should be in correct location"
        );
    }
}
