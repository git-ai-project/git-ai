use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::{
    HumanRecord, MAX_MATERIALIZED_LINE_COUNT, PromptRecord, SessionRecord,
};
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::working_log::{CHECKPOINT_API_VERSION, Checkpoint, CheckpointKind};
use crate::error::GitAiError;
use crate::git::repository::{BatchMaterializationBudget, ensure_batch_git_item_limit};
use crate::utils::normalize_to_posix;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub const MAX_CHECKPOINTS_JSONL_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_INITIAL_ATTRIBUTIONS_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_WORKING_LOG_BLOB_BYTES: u64 = 16 * 1_024 * 1_024;

#[cfg(feature = "test-support")]
const TEST_CHECKPOINTS_JSONL_MAX_BYTES_ENV: &str = "GIT_AI_TEST_CHECKPOINTS_JSONL_MAX_BYTES";

/// Initial attributions data structure stored in the INITIAL file
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitialAttributions {
    /// Map of file path to line attributions
    pub files: HashMap<String, Vec<LineAttribution>>,
    /// Map of author_id (hash) to PromptRecord for prompt tracking
    pub prompts: HashMap<String, PromptRecord>,
    /// Optional blob snapshot of the file content represented by INITIAL.
    #[serde(default)]
    pub file_blobs: HashMap<String, String>,
    /// Known human records: `h_<hash>` -> HumanRecord
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub humans: std::collections::BTreeMap<String, HumanRecord>,
    /// Session records: `s_<session_id>` -> SessionRecord
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sessions: std::collections::BTreeMap<String, SessionRecord>,
}

#[derive(Debug, Clone)]
pub struct RepoStorage {
    pub ai_dir: PathBuf,
    pub repo_workdir: PathBuf,
    pub working_logs: PathBuf,
    pub logs: PathBuf,
}

impl RepoStorage {
    pub fn for_repo_path(repo_path: &Path, repo_workdir: &Path) -> Result<RepoStorage, GitAiError> {
        Self::for_ai_dir(&repo_path.join("ai"), repo_workdir)
    }

    pub fn for_isolated_worktree_storage(
        ai_dir: &Path,
        repo_workdir: &Path,
    ) -> Result<RepoStorage, GitAiError> {
        Self::for_ai_dir(ai_dir, repo_workdir)
    }

    fn for_ai_dir(ai_dir: &Path, repo_workdir: &Path) -> Result<RepoStorage, GitAiError> {
        let working_logs_dir = ai_dir.join("working_logs");
        let logs_dir = ai_dir.join("logs");

        let config = RepoStorage {
            ai_dir: ai_dir.to_path_buf(),
            repo_workdir: repo_workdir.to_path_buf(),
            working_logs: working_logs_dir,
            logs: logs_dir,
        };

        config.ensure_config_directory()?;
        Ok(config)
    }

    #[doc(hidden)]
    pub fn ensure_config_directory(&self) -> Result<(), GitAiError> {
        fs::create_dir_all(&self.ai_dir)?;

        // Create working_logs directory
        fs::create_dir_all(&self.working_logs)?;

        // Create logs directory for Sentry events
        fs::create_dir_all(&self.logs)?;

        Ok(())
    }

    /* Working Log Persistance */

    pub fn has_working_log(&self, sha: &str) -> bool {
        self.working_logs.join(sha).exists()
    }

    pub fn working_log_for_base_commit(
        &self,
        sha: &str,
    ) -> Result<PersistedWorkingLog, GitAiError> {
        let working_log_dir = self.working_logs.join(sha);
        fs::create_dir_all(&working_log_dir)?;
        let canonical_workdir = self
            .repo_workdir
            .canonicalize()
            .unwrap_or_else(|_| self.repo_workdir.clone());
        Ok(PersistedWorkingLog::new(
            working_log_dir,
            sha,
            self.repo_workdir.clone(),
            canonical_workdir,
            None,
        ))
    }

    pub fn delete_working_log_for_base_commit(&self, sha: &str) -> Result<(), GitAiError> {
        let working_log_dir = self.working_logs.join(sha);
        if working_log_dir.exists() {
            // Both debug and release: move to old-{sha} for retention
            let old_dir = self.working_logs.join(format!("old-{}", sha));
            // If old-{sha} already exists, remove it first
            if old_dir.exists() {
                fs::remove_dir_all(&old_dir)?;
            }
            fs::rename(&working_log_dir, &old_dir)?;

            // Write a timestamp marker so we know when it was archived
            let marker = old_dir.join(".archived_at");
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            // Best-effort; don't fail the commit if we can't write the marker
            let _ = fs::write(&marker, now.to_string());

            tracing::debug!("Moved checkpoint directory from {} to old-{}", sha, sha);

            // In production builds, prune old working logs that have expired.
            // Debug builds never prune so developers can inspect old state.
            if !cfg!(debug_assertions) {
                self.prune_expired_old_working_logs();
            }
        }
        Ok(())
    }

    /// Number of seconds to retain archived working logs in production builds (7 days).
    const OLD_WORKING_LOG_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

    /// Remove archived (`old-*`) working log directories whose `.archived_at`
    /// timestamp is older than `OLD_WORKING_LOG_RETENTION_SECS`.
    /// Errors are intentionally swallowed so pruning never breaks the commit flow.
    #[doc(hidden)]
    pub fn prune_expired_old_working_logs(&self) {
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let entries = match fs::read_dir(&self.working_logs) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("old-") {
                continue;
            }

            let dir_path = entry.path();
            if !dir_path.is_dir() {
                continue;
            }

            let marker = dir_path.join(".archived_at");
            let archived_at =
                match read_bounded_utf8_file(&marker, "working-log archive marker", 64) {
                    Ok(contents) => contents.trim().parse::<u64>().unwrap_or(0),
                    // No marker means this was created before the retention feature;
                    // treat it as immediately expired so it gets cleaned up.
                    Err(_) => 0,
                };

            if now_secs.saturating_sub(archived_at) >= Self::OLD_WORKING_LOG_RETENTION_SECS {
                tracing::debug!("Pruning expired old working log: {}", name_str);
                let _ = fs::remove_dir_all(&dir_path);
            }
        }
    }

    /// Move a working log directory from one commit SHA to another.
    /// If the destination already has checkpoints, preserve the old-base entries first and
    /// append the destination entries after them.
    pub fn rename_working_log(&self, old_sha: &str, new_sha: &str) -> Result<(), GitAiError> {
        let old_dir = self.working_logs.join(old_sha);
        let new_dir = self.working_logs.join(new_sha);
        if !old_dir.exists() {
            return Ok(());
        }
        if !new_dir.exists() {
            fs::rename(&old_dir, &new_dir)?;
            tracing::debug!("Renamed working log from {} to {}", old_sha, new_sha);
        } else {
            self.merge_working_log_dirs(old_sha, new_sha, &old_dir, &new_dir)?;
            fs::remove_dir_all(&old_dir)?;
            tracing::debug!("Merged working log from {} into {}", old_sha, new_sha);
        }
        Ok(())
    }

    fn merge_working_log_dirs(
        &self,
        old_sha: &str,
        new_sha: &str,
        old_dir: &Path,
        new_dir: &Path,
    ) -> Result<(), GitAiError> {
        copy_dir_contents(&old_dir.join("blobs"), &new_dir.join("blobs"))?;

        let canonical = self
            .repo_workdir
            .canonicalize()
            .unwrap_or_else(|_| self.repo_workdir.clone());
        let old_log = PersistedWorkingLog::new(
            old_dir.to_path_buf(),
            old_sha,
            self.repo_workdir.clone(),
            canonical.clone(),
            None,
        );
        let new_log = PersistedWorkingLog::new(
            new_dir.to_path_buf(),
            new_sha,
            self.repo_workdir.clone(),
            canonical,
            None,
        );

        // Preserve OLD-base entries first (per rename_working_log's contract):
        // start from the old INITIAL and only insert a new-base entry when its
        // key is absent, so old wins on any shared key. HashMap::extend would do
        // the opposite (new clobbers old). The checkpoints Vec below is already
        // old-then-new, so it needs no such guard.
        let mut merged_initial = old_log.read_initial_attributions();
        let new_initial = new_log.read_initial_attributions();
        for (k, v) in new_initial.files {
            merged_initial.files.entry(k).or_insert(v);
        }
        for (k, v) in new_initial.prompts {
            merged_initial.prompts.entry(k).or_insert(v);
        }
        for (k, v) in new_initial.file_blobs {
            merged_initial.file_blobs.entry(k).or_insert(v);
        }
        for (k, v) in new_initial.humans {
            merged_initial.humans.entry(k).or_insert(v);
        }
        for (k, v) in new_initial.sessions {
            merged_initial.sessions.entry(k).or_insert(v);
        }
        new_log.write_initial(merged_initial)?;

        let mut checkpoints = old_log.read_all_checkpoints()?;
        checkpoints.extend(new_log.read_all_checkpoints()?);
        new_log.write_all_checkpoints(&checkpoints)?;
        Ok(())
    }
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), GitAiError> {
    if !src.exists() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
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
    pub dirty_files: Option<HashMap<String, Arc<str>>>,
    pub initial_file: PathBuf,
}

impl PersistedWorkingLog {
    pub fn new(
        dir: PathBuf,
        base_commit: &str,
        repo_root: PathBuf,
        canonical_workdir: PathBuf,
        dirty_files: Option<HashMap<String, Arc<str>>>,
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

    pub fn set_dirty_files(&mut self, dirty_files: Option<HashMap<String, Arc<str>>>) {
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
        let checkpoints_file = self.checkpoints_file();
        fs::write(&checkpoints_file, "")?;

        // Clear INITIAL attributions file so stale attributions from a
        // previous working state do not persist across resets
        if self.initial_file.exists() {
            fs::remove_file(&self.initial_file)?;
        }

        Ok(())
    }

    pub fn checkpoints_file(&self) -> PathBuf {
        self.dir.join("checkpoints.jsonl")
    }

    /* blob storage */
    pub fn get_file_version(&self, sha: &str) -> Result<String, GitAiError> {
        let blob_path = self.dir.join("blobs").join(sha);
        read_bounded_utf8_file(&blob_path, "working-log blob", MAX_WORKING_LOG_BLOB_BYTES)
    }

    #[allow(dead_code)]
    pub fn persist_file_version(&self, content: &str) -> Result<String, GitAiError> {
        if content.len() as u64 > MAX_WORKING_LOG_BLOB_BYTES {
            return Err(GitAiError::Generic(format!(
                "working-log blob exceeded the {MAX_WORKING_LOG_BLOB_BYTES} byte limit ({})",
                content.len()
            )));
        }
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

    pub fn read_current_file_content(&self, file_path: &str) -> Result<Arc<str>, GitAiError> {
        if let Some(ref dirty_files) = self.dirty_files
            && let Some(content) = dirty_files.get(&file_path.to_string())
        {
            return Ok(content.clone());
        }

        Err(GitAiError::Generic(format!(
            "read_current_file_content: file '{}' not found in dirty_files snapshot (filesystem fallback is not allowed in checkpoint flow)",
            file_path
        )))
    }

    /* append checkpoint */
    pub fn append_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), GitAiError> {
        // Read existing checkpoints
        let mut checkpoints = self.read_all_checkpoints().unwrap_or_default();

        // Create a copy, potentially without transcript to reduce storage size.
        //
        // Tools that DON'T support refetch (transcript must be kept):
        // - "mock_ai" - test preset, transcript not stored externally
        // - Any other agent-v1 custom tools (detected by lack of tool-specific metadata)
        checkpoints.push(checkpoint.clone());

        // Prune char-level attributions from older checkpoints for the same files
        // Only the most recent checkpoint per file needs char-level precision
        self.prune_old_char_attributions(&mut checkpoints);

        // Write all checkpoints back
        self.write_all_checkpoints(&checkpoints)
    }

    pub fn read_all_checkpoints(&self) -> Result<Vec<Checkpoint>, GitAiError> {
        self.read_all_checkpoints_with_size_limit(Self::checkpoints_file_size_limit_bytes())
    }

    #[cfg(feature = "test-support")]
    pub fn read_all_checkpoints_with_size_limit_for_test(
        &self,
        max_bytes: u64,
    ) -> Result<Vec<Checkpoint>, GitAiError> {
        self.read_all_checkpoints_with_size_limit(max_bytes)
    }

    pub fn ensure_checkpoints_file_size_limit(&self) -> Result<(), GitAiError> {
        self.truncate_oversized_checkpoints_file(Self::checkpoints_file_size_limit_bytes())?;
        Ok(())
    }

    fn read_all_checkpoints_with_size_limit(
        &self,
        max_bytes: u64,
    ) -> Result<Vec<Checkpoint>, GitAiError> {
        let checkpoints_file = self.checkpoints_file();

        if !checkpoints_file.exists() {
            return Ok(Vec::new());
        }

        if self.truncate_oversized_checkpoints_file(max_bytes)? {
            return Ok(Vec::new());
        }

        let input = fs::File::open(&checkpoints_file)?;
        let mut checkpoints = Vec::new();

        // Parse JSONL file - each line is a separate JSON object
        for line in BufReader::new(input).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let checkpoint: Checkpoint = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            if checkpoint.api_version != CHECKPOINT_API_VERSION {
                tracing::debug!(
                    "unsupported checkpoint api version: {} (silently skipping checkpoint)",
                    checkpoint.api_version
                );
                continue;
            }

            validate_checkpoint_collections(&checkpoint)?;
            ensure_batch_git_item_limit(
                "working-log checkpoint",
                checkpoints.len().saturating_add(1),
            )?;
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

    fn checkpoints_file_size_limit_bytes() -> u64 {
        #[cfg(feature = "test-support")]
        if let Ok(raw) = std::env::var(TEST_CHECKPOINTS_JSONL_MAX_BYTES_ENV)
            && let Ok(value) = raw.parse::<u64>()
            && value > 0
        {
            return value;
        }

        MAX_CHECKPOINTS_JSONL_BYTES
    }

    fn truncate_oversized_checkpoints_file(&self, max_bytes: u64) -> Result<bool, GitAiError> {
        let checkpoints_file = self.checkpoints_file();
        let metadata = match fs::metadata(&checkpoints_file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let size_bytes = metadata.len();
        if size_bytes <= max_bytes {
            return Ok(false);
        }

        let message = format!(
            "checkpoints.jsonl exceeded maximum size: {} bytes > {} bytes; deleting and recreating {}",
            size_bytes,
            max_bytes,
            checkpoints_file.display()
        );
        tracing::error!(
            base_commit = %self.base_commit,
            path = %checkpoints_file.display(),
            size_bytes,
            max_bytes,
            "checkpoints.jsonl exceeded maximum size; deleting and recreating empty file"
        );
        crate::observability::log_error(
            &GitAiError::Generic(message),
            Some(serde_json::json!({
                "event": "checkpoints_jsonl_oversized_reset",
                "base_commit": self.base_commit,
                "path": checkpoints_file.to_string_lossy(),
                "size_bytes": size_bytes,
                "max_bytes": max_bytes,
            })),
        );

        match fs::remove_file(&checkpoints_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::File::create(&checkpoints_file)?;
        Ok(true)
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
        ensure_batch_git_item_limit("working-log checkpoint", checkpoints.len())?;
        for checkpoint in checkpoints {
            validate_checkpoint_collections(checkpoint)?;
        }
        let checkpoints_file = self.checkpoints_file();
        let mut output = BoundedWriter::new(
            Vec::new(),
            Self::checkpoints_file_size_limit_bytes(),
            "checkpoints.jsonl",
        );

        for checkpoint in checkpoints {
            serde_json::to_writer(&mut output, checkpoint)?;
            output.write_all(b"\n")?;
        }

        output.flush()?;
        let serialized = output.into_inner();
        fs::write(&checkpoints_file, serialized)?;
        Ok(())
    }

    pub fn mutate_all_checkpoints<F>(&self, mutator: F) -> Result<Vec<Checkpoint>, GitAiError>
    where
        F: FnOnce(&mut Vec<Checkpoint>) -> Result<(), GitAiError>,
    {
        let mut checkpoints = self.read_all_checkpoints()?;
        mutator(&mut checkpoints)?;
        self.write_all_checkpoints(&checkpoints)?;
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

    pub fn observed_file_snapshot(&self) -> Result<HashMap<String, String>, GitAiError> {
        let initial = self.read_initial_attributions();
        let mut blob_by_path = HashMap::new();

        for file_path in initial.files.keys() {
            let blob_sha = initial.file_blobs.get(file_path).ok_or_else(|| {
                GitAiError::Generic(format!(
                    "INITIAL missing persisted file snapshot for {}",
                    file_path
                ))
            })?;
            ensure_new_working_log_snapshot_path(&blob_by_path, file_path)?;
            blob_by_path.insert(file_path.clone(), blob_sha.clone());
        }

        for checkpoint in self.read_all_checkpoints()? {
            for entry in checkpoint.entries {
                ensure_new_working_log_snapshot_path(&blob_by_path, &entry.file)?;
                blob_by_path.insert(entry.file, entry.blob_sha);
            }
        }

        let mut blob_contents = HashMap::new();
        let mut snapshot = HashMap::with_capacity(blob_by_path.len());
        let mut budget = BatchMaterializationBudget::new();
        for (file_path, blob_sha) in blob_by_path {
            if !blob_contents.contains_key(&blob_sha) {
                blob_contents.insert(
                    blob_sha.clone(),
                    self.get_file_version(&blob_sha).unwrap_or_default(),
                );
            }
            let content = blob_contents.get(&blob_sha).expect("inserted above");
            budget.reserve("working-log snapshot content", content.len())?;
            snapshot.insert(file_path, content.clone());
        }

        Ok(snapshot)
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
                CheckpointKind::Human | CheckpointKind::KnownHuman => {
                    // Skip human checkpoints
                }
            }
        }
        Ok(touched_files)
    }

    /* INITIAL attributions file */

    /// Persist INITIAL attributions plus exact file snapshots for the target working log.
    pub fn write_initial_attributions_with_contents(
        &self,
        attributions: HashMap<String, Vec<LineAttribution>>,
        prompts: HashMap<String, PromptRecord>,
        humans: std::collections::BTreeMap<String, HumanRecord>,
        file_contents: HashMap<String, String>,
        sessions: std::collections::BTreeMap<String, SessionRecord>,
    ) -> Result<(), GitAiError> {
        validate_initial_files(&attributions)?;
        ensure_batch_git_item_limit("INITIAL prompt", prompts.len())?;
        ensure_batch_git_item_limit("INITIAL human", humans.len())?;
        ensure_batch_git_item_limit("INITIAL file content", file_contents.len())?;
        ensure_batch_git_item_limit("INITIAL session", sessions.len())?;
        let filtered: HashMap<String, Vec<LineAttribution>> = attributions
            .into_iter()
            .filter(|(_, attrs)| !attrs.is_empty())
            .collect();

        let mut content_budget = BatchMaterializationBudget::new();
        for file_path in filtered.keys() {
            let content = file_contents.get(file_path).ok_or_else(|| {
                GitAiError::Generic(format!(
                    "INITIAL missing file content snapshot for {}",
                    file_path
                ))
            })?;
            content_budget.reserve("working-log INITIAL content", content.len())?;
        }

        let mut file_blobs = HashMap::new();
        for file_path in filtered.keys() {
            let content = file_contents.get(file_path).ok_or_else(|| {
                GitAiError::Generic(format!(
                    "INITIAL missing file content snapshot for {}",
                    file_path
                ))
            })?;
            let blob_sha = self.persist_file_version(content)?;
            file_blobs.insert(file_path.clone(), blob_sha);
        }

        self.write_initial(InitialAttributions {
            files: filtered,
            prompts,
            file_blobs,
            humans,
            sessions,
        })
    }

    /// Write a fully-formed INITIAL state, preserving any persisted blob references.
    pub fn write_initial(&self, initial: InitialAttributions) -> Result<(), GitAiError> {
        validate_initial_attributions(&initial)?;
        let filtered_files: HashMap<String, Vec<LineAttribution>> = initial
            .files
            .into_iter()
            .filter(|(_, attrs)| !attrs.is_empty())
            .collect();

        if filtered_files.is_empty() {
            if self.initial_file.exists() {
                fs::remove_file(&self.initial_file)?;
            }
            return Ok(());
        }

        let mut file_blobs = initial.file_blobs;
        file_blobs.retain(|file_path, _| filtered_files.contains_key(file_path));

        let initial_data = InitialAttributions {
            files: filtered_files,
            prompts: initial.prompts,
            file_blobs,
            humans: initial.humans,
            sessions: initial.sessions,
        };

        let mut output = BoundedWriter::new(
            Vec::new(),
            MAX_INITIAL_ATTRIBUTIONS_BYTES,
            "working-log INITIAL",
        );
        serde_json::to_writer_pretty(&mut output, &initial_data)?;
        output.flush()?;
        let serialized = output.into_inner();
        fs::write(&self.initial_file, serialized)?;

        Ok(())
    }

    pub fn initial_file_content_from(
        &self,
        initial: &InitialAttributions,
        file_path: &str,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(content) = self.stored_initial_file_content_from(initial, file_path) {
            return Ok(Some(content));
        }
        if initial.files.contains_key(file_path) {
            return Err(GitAiError::Generic(format!(
                "INITIAL missing persisted file snapshot for {}",
                file_path
            )));
        }
        Ok(None)
    }

    pub fn stored_initial_file_content_from(
        &self,
        initial: &InitialAttributions,
        file_path: &str,
    ) -> Option<String> {
        if let Some(blob_sha) = initial.file_blobs.get(file_path) {
            return self.get_file_version(blob_sha).ok();
        }
        None
    }

    pub fn latest_checkpoint_file_content(&self, file_path: &str) -> Option<String> {
        let checkpoints = self.read_all_checkpoints().ok()?;
        let entry = checkpoints.iter().rev().find_map(|checkpoint| {
            checkpoint
                .entries
                .iter()
                .find(|entry| entry.file == file_path)
        })?;
        self.get_file_version(&entry.blob_sha).ok()
    }

    pub fn effective_tracked_file_content(
        &self,
        initial: &InitialAttributions,
        file_path: &str,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(content) = self.latest_checkpoint_file_content(file_path) {
            return Ok(Some(content));
        }
        self.initial_file_content_from(initial, file_path)
    }

    /// Read initial attributions from the INITIAL file.
    /// Returns empty attributions and prompts if the file doesn't exist.
    pub fn read_initial_attributions(&self) -> InitialAttributions {
        if !self.initial_file.exists() {
            return InitialAttributions::default();
        }

        match read_bounded_utf8_file(
            &self.initial_file,
            "working-log INITIAL",
            MAX_INITIAL_ATTRIBUTIONS_BYTES,
        ) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(initial_data) => match validate_initial_attributions(&initial_data) {
                    Ok(()) => initial_data,
                    Err(error) => {
                        tracing::debug!("Invalid INITIAL file: {}. Returning empty.", error);
                        InitialAttributions::default()
                    }
                },
                Err(e) => {
                    tracing::debug!("Failed to parse INITIAL file: {}. Returning empty.", e);
                    InitialAttributions::default()
                }
            },
            Err(e) => {
                tracing::debug!("Failed to read INITIAL file: {}. Returning empty.", e);
                InitialAttributions::default()
            }
        }
    }
}

fn validate_initial_attributions(initial: &InitialAttributions) -> Result<(), GitAiError> {
    validate_initial_files(&initial.files)?;
    ensure_batch_git_item_limit("INITIAL prompt", initial.prompts.len())?;
    ensure_batch_git_item_limit("INITIAL blob", initial.file_blobs.len())?;
    ensure_batch_git_item_limit("INITIAL human", initial.humans.len())?;
    ensure_batch_git_item_limit("INITIAL session", initial.sessions.len())?;
    Ok(())
}

fn validate_initial_files(files: &HashMap<String, Vec<LineAttribution>>) -> Result<(), GitAiError> {
    ensure_batch_git_item_limit("INITIAL file", files.len())?;
    for line_attributions in files.values() {
        ensure_batch_git_item_limit("INITIAL line attribution", line_attributions.len())?;
    }
    validate_line_attribution_ranges(
        files.values().flat_map(|attributions| attributions.iter()),
        "INITIAL line attribution",
    )?;
    Ok(())
}

fn validate_line_attribution_ranges<'a>(
    attributions: impl Iterator<Item = &'a LineAttribution>,
    kind: &str,
) -> Result<(), GitAiError> {
    let mut covered_lines = 0u64;
    for attribution in attributions {
        if attribution.start_line == 0 || attribution.start_line > attribution.end_line {
            return Err(GitAiError::Generic(format!(
                "{kind} contains an invalid line range {}-{}",
                attribution.start_line, attribution.end_line
            )));
        }
        let range_lines = u64::from(attribution.end_line)
            .saturating_sub(u64::from(attribution.start_line))
            .saturating_add(1);
        covered_lines = covered_lines.saturating_add(range_lines);
        if covered_lines > MAX_MATERIALIZED_LINE_COUNT {
            return Err(GitAiError::Generic(format!(
                "{kind} exceeded the {MAX_MATERIALIZED_LINE_COUNT} line materialization limit"
            )));
        }
    }
    Ok(())
}

fn ensure_new_working_log_snapshot_path(
    paths: &HashMap<String, String>,
    file_path: &str,
) -> Result<(), GitAiError> {
    if !paths.contains_key(file_path) {
        ensure_batch_git_item_limit("working-log snapshot path", paths.len().saturating_add(1))?;
    }
    Ok(())
}

fn validate_checkpoint_collections(checkpoint: &Checkpoint) -> Result<(), GitAiError> {
    ensure_batch_git_item_limit("working-log entry", checkpoint.entries.len())?;
    if let Some(metadata) = &checkpoint.agent_metadata {
        ensure_batch_git_item_limit("working-log agent metadata", metadata.len())?;
    }
    for entry in &checkpoint.entries {
        ensure_batch_git_item_limit("working-log attribution", entry.attributions.len())?;
        ensure_batch_git_item_limit(
            "working-log line attribution",
            entry.line_attributions.len(),
        )?;
    }
    validate_line_attribution_ranges(
        checkpoint
            .entries
            .iter()
            .flat_map(|entry| entry.line_attributions.iter()),
        "working-log line attribution",
    )?;
    Ok(())
}

struct BoundedWriter<W> {
    inner: W,
    written: u64,
    max_bytes: u64,
    kind: &'static str,
}

impl<W> BoundedWriter<W> {
    fn new(inner: W, max_bytes: u64, kind: &'static str) -> Self {
        Self {
            inner,
            written: 0,
            max_bytes,
            kind,
        }
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next_size = self.written.saturating_add(buffer.len() as u64);
        if next_size > self.max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} exceeded the {} byte limit ({next_size})",
                    self.kind, self.max_bytes
                ),
            ));
        }

        let written = self.inner.write(buffer)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn read_bounded_utf8_file(path: &Path, kind: &str, max_bytes: u64) -> Result<String, GitAiError> {
    let file = fs::File::open(path)?;
    let size_bytes = file.metadata()?.len();
    if size_bytes > max_bytes {
        return Err(GitAiError::Generic(format!(
            "{kind} exceeded the {max_bytes} byte limit ({size_bytes})"
        )));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(GitAiError::Generic(format!(
            "{kind} exceeded the {max_bytes} byte limit ({})",
            bytes.len()
        )));
    }
    String::from_utf8(bytes)
        .map_err(|error| GitAiError::Generic(format!("{kind} is not UTF-8: {error}")))
}

pub(crate) fn read_bounded_working_log_file(path: &Path) -> Result<String, GitAiError> {
    read_bounded_utf8_file(path, "working-log file", MAX_WORKING_LOG_BLOB_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn attr(author: &str) -> Vec<LineAttribution> {
        vec![LineAttribution::new(1, 1, author.to_string(), None)]
    }

    #[test]
    fn initial_validation_rejects_oversized_line_attribution_span() {
        let files = HashMap::from([(
            "large.txt".to_string(),
            vec![LineAttribution::new(1, 1_000_001, "test".to_string(), None)],
        )]);

        assert!(validate_initial_files(&files).is_err());
    }

    /// Regression (#9): merge_working_log_dirs (via rename_working_log when the
    /// destination already exists) must preserve OLD-base INITIAL entries on a
    /// shared key, per the documented "preserve the old-base entries first".
    /// The old code used HashMap::extend(new), so `new` clobbered `old` for any
    /// shared path. Each side's unique entries must also survive.
    #[test]
    fn test_merge_working_log_dirs_old_base_wins_on_conflict() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(&workdir).unwrap();
        let ai_dir = tmp.path().join("ai");
        let storage = RepoStorage::for_repo_path(&ai_dir, &workdir).unwrap();

        let old_sha = "1111111111111111111111111111111111111111";
        let new_sha = "2222222222222222222222222222222222222222";

        // OLD base: shared.txt -> old author, plus a unique old-only file.
        let old_log = storage.working_log_for_base_commit(old_sha).unwrap();
        let mut old_initial = InitialAttributions::default();
        old_initial.files.insert("shared.txt".into(), attr("h_OLD"));
        old_initial
            .files
            .insert("old_only.txt".into(), attr("h_OLD"));
        old_initial
            .file_blobs
            .insert("shared.txt".into(), "OLD CONTENT".into());
        old_log.write_initial(old_initial).unwrap();

        // NEW base: shared.txt -> new author (conflict), plus a unique new-only file.
        let new_log = storage.working_log_for_base_commit(new_sha).unwrap();
        let mut new_initial = InitialAttributions::default();
        new_initial
            .files
            .insert("shared.txt".into(), attr("ai_NEW"));
        new_initial
            .files
            .insert("new_only.txt".into(), attr("ai_NEW"));
        new_initial
            .file_blobs
            .insert("shared.txt".into(), "NEW CONTENT".into());
        new_log.write_initial(new_initial).unwrap();

        // Merge old into new (destination already exists).
        storage.rename_working_log(old_sha, new_sha).unwrap();

        let merged = storage
            .working_log_for_base_commit(new_sha)
            .unwrap()
            .read_initial_attributions();

        // Shared key: OLD base wins.
        assert_eq!(
            merged
                .files
                .get("shared.txt")
                .map(|a| a[0].author_id.as_str()),
            Some("h_OLD"),
            "old-base attribution must win on a shared path"
        );
        assert_eq!(
            merged.file_blobs.get("shared.txt").map(|s| s.as_str()),
            Some("OLD CONTENT"),
            "old-base blob must win on a shared path (kept consistent with files)"
        );
        // Both sides' unique entries survive.
        assert!(merged.files.contains_key("old_only.txt"));
        assert!(merged.files.contains_key("new_only.txt"));
    }
}
