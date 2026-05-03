use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::{HumanRecord, PromptRecord, SessionRecord};
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::working_log::{CHECKPOINT_API_VERSION, Checkpoint, CheckpointKind};
use crate::error::GitAiError;
use crate::git::rewrite_log::{RewriteLogEvent, append_event_to_file};
use crate::utils::normalize_to_posix;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
    pub rewrite_log: PathBuf,
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
        let rewrite_log_file = ai_dir.join("rewrite_log");
        let logs_dir = ai_dir.join("logs");

        let config = RepoStorage {
            ai_dir: ai_dir.to_path_buf(),
            repo_workdir: repo_workdir.to_path_buf(),
            working_logs: working_logs_dir,
            rewrite_log: rewrite_log_file,
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

        if !&self.rewrite_log.exists() && !&self.rewrite_log.is_file() {
            fs::write(&self.rewrite_log, "")?;
        }

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
            let archived_at = match fs::read_to_string(&marker) {
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

    /// Rename a working log directory from one commit SHA to another.
    /// Used when fast-forward pull changes HEAD but preserves working directory state.
    /// Only renames if old directory exists and new directory doesn't exist.
    pub fn rename_working_log(&self, old_sha: &str, new_sha: &str) -> Result<(), GitAiError> {
        let old_dir = self.working_logs.join(old_sha);
        let new_dir = self.working_logs.join(new_sha);
        if old_dir.exists() && !new_dir.exists() {
            fs::rename(&old_dir, &new_dir)?;
            tracing::debug!("Renamed working log from {} to {}", old_sha, new_sha);
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
                tracing::debug!(
                    "unsupported checkpoint api version: {} (silently skipping checkpoint)",
                    checkpoint.api_version
                );
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

    /// Write initial attributions to the INITIAL file.
    /// This seeds the working log with known attributions from rewrite operations.
    /// Only writes files that have non-empty attributions.
    pub fn write_initial_attributions(
        &self,
        attributions: HashMap<String, Vec<LineAttribution>>,
        prompts: HashMap<String, PromptRecord>,
    ) -> Result<(), GitAiError> {
        self.write_initial(InitialAttributions {
            files: attributions,
            prompts,
            file_blobs: HashMap::new(),
            humans: std::collections::BTreeMap::new(),
            sessions: std::collections::BTreeMap::new(),
        })
    }

    /// Persist INITIAL attributions plus exact file snapshots for the target working log.
    pub fn write_initial_attributions_with_contents(
        &self,
        attributions: HashMap<String, Vec<LineAttribution>>,
        prompts: HashMap<String, PromptRecord>,
        humans: std::collections::BTreeMap<String, HumanRecord>,
        file_contents: HashMap<String, String>,
        sessions: std::collections::BTreeMap<String, SessionRecord>,
    ) -> Result<(), GitAiError> {
        let filtered: HashMap<String, Vec<LineAttribution>> = attributions
            .into_iter()
            .filter(|(_, attrs)| !attrs.is_empty())
            .collect();
        let mut file_blobs = HashMap::new();
        for file_path in filtered.keys() {
            if let Some(content) = file_contents.get(file_path) {
                let blob_sha = self.persist_file_version(content)?;
                file_blobs.insert(file_path.clone(), blob_sha);
            }
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

        let json = serde_json::to_string_pretty(&initial_data)?;
        fs::write(&self.initial_file, json)?;

        Ok(())
    }

    pub fn initial_file_content_from(
        &self,
        initial: &InitialAttributions,
        file_path: &str,
    ) -> Option<String> {
        if let Some(content) = self.stored_initial_file_content_from(initial, file_path) {
            return Some(content);
        }
        if initial.files.contains_key(file_path) {
            return self.read_current_file_content(file_path).ok();
        }
        None
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
    ) -> Option<String> {
        self.latest_checkpoint_file_content(file_path)
            .or_else(|| self.initial_file_content_from(initial, file_path))
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
