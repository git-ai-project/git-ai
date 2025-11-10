use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::working_log::{CHECKPOINT_API_VERSION, Checkpoint, CheckpointKind};
use crate::error::GitAiError;
use crate::git::rewrite_log::{RewriteLogEvent, append_event_to_file};
use crate::utils::debug_log;
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

        // @todo - @acunniffe, make this lazy on a read or write.
        // it's probably fine to run this when Repository is loaded but there
        // are many git commands for which it is not needed
        config.ensure_config_directory().unwrap();
        return config;
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
        PersistedWorkingLog::new(working_log_dir, sha, self.repo_workdir.clone(), None)
    }

    #[allow(dead_code)]
    pub fn delete_working_log_for_base_commit(&self, sha: &str) -> Result<(), GitAiError> {
        let working_log_dir = self.working_logs.join(sha);
        if working_log_dir.exists() {
            fs::remove_dir_all(&working_log_dir)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn delete_all_working_logs(&self) -> Result<(), GitAiError> {
        if self.working_logs.exists() {
            fs::remove_dir_all(&self.working_logs)?;
            // Recreate the empty directory structure
            fs::create_dir_all(&self.working_logs)?;
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
    pub dirty_files: Option<HashMap<String, String>>,
}

impl PersistedWorkingLog {
    pub fn new(
        dir: PathBuf,
        base_commit: &str,
        repo_root: PathBuf,
        dirty_files: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            dir,
            base_commit: base_commit.to_string(),
            repo_workdir: repo_root,
            dirty_files,
        }
    }

    pub fn set_dirty_files(&mut self, dirty_files: Option<HashMap<String, String>>) {
        self.dirty_files = dirty_files.map(|map| {
            map.into_iter()
                .map(|(file_path, content)| (self.to_repo_relative_path(&file_path), content))
                .collect()
        });
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

        Ok(())
    }

    /* blob storage */
    pub fn get_file_version(&self, sha: &str) -> Result<String, GitAiError> {
        let blob_path = self.dir.join("blobs").join(sha);
        Ok(fs::read_to_string(blob_path)?)
    }

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
        let canonical_workdir = match self.repo_workdir.canonicalize() {
            Ok(p) => p,
            Err(_) => self.repo_workdir.clone(),
        };
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };
        if canonical_path.starts_with(&canonical_workdir) {
            return canonical_path
                .strip_prefix(&canonical_workdir)
                .unwrap()
                .to_string_lossy()
                .to_string();
        }
        return file_path.to_string();
    }

    pub fn read_current_file_content(&self, file_path: &str) -> Result<String, GitAiError> {
        // First try to read from dirty_files (using raw path)
        if let Some(ref dirty_files) = self.dirty_files {
            if let Some(content) = dirty_files.get(&file_path.to_string()) {
                return Ok(content.clone());
            }
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
        let checkpoints_file = self.dir.join("checkpoints.jsonl");

        // Serialize checkpoint to JSON and append to JSONL file
        let json_line = serde_json::to_string(checkpoint)?;

        // Open file in append mode and write the JSON line
        use std::fs::OpenOptions;
        use std::io::Write;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&checkpoints_file)?;

        writeln!(file, "{}", json_line)?;

        Ok(())
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

        Ok(checkpoints)
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

        let initial_file = self.dir.join("INITIAL");
        let json = serde_json::to_string_pretty(&initial_data)?;
        fs::write(initial_file, json)?;

        Ok(())
    }

    /// Read initial attributions from the INITIAL file.
    /// Returns empty attributions and prompts if the file doesn't exist.
    pub fn read_initial_attributions(&self) -> InitialAttributions {
        let initial_file = self.dir.join("INITIAL");

        if !initial_file.exists() {
            return InitialAttributions::default();
        }

        match fs::read_to_string(&initial_file) {
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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());

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
        let repo_storage = RepoStorage::for_repo_path(
            &tmp_repo.repo().path(),
            &tmp_repo.repo().workdir().unwrap(),
        );

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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());
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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());
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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());
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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());
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
            RepoStorage::for_repo_path(tmp_repo.repo().path(), &tmp_repo.repo().workdir().unwrap());

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
