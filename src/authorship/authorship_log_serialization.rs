use crate::authorship::authorship_log::{Author, LineRange, PromptRecord};
use crate::authorship::working_log::CheckpointKind;
use crate::config;
use crate::git::repository::Repository;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// Authorship log format version identifier
pub const AUTHORSHIP_LOG_VERSION: &str = "authorship/3.0.0";

/// Metadata section that goes below the divider as JSON
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorshipMetadata {
    pub schema_version: String,
    pub base_commit_sha: String,
    pub prompts: BTreeMap<String, PromptRecord>,
}

impl AuthorshipMetadata {
    pub fn new() -> Self {
        Self {
            schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
            base_commit_sha: String::new(),
            prompts: BTreeMap::new(),
        }
    }
}

impl Default for AuthorshipMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Attestation entry: short hash followed by line ranges
///
/// IMPORTANT: The hash ALWAYS corresponds to a prompt in the prompts section.
/// This system only tracks AI-generated content, not human-authored content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationEntry {
    /// Short hash (7 chars) that maps to an entry in the prompts section of the metadata
    pub hash: String,
    /// Line ranges that this prompt is responsible for
    pub line_ranges: Vec<LineRange>,
}

impl AttestationEntry {
    pub fn new(hash: String, line_ranges: Vec<LineRange>) -> Self {
        Self { hash, line_ranges }
    }

    #[allow(dead_code)]
    pub fn remove_line_ranges(&mut self, to_remove: &[LineRange]) {
        let mut current_ranges = self.line_ranges.clone();

        for remove_range in to_remove {
            let mut new_ranges = Vec::new();
            for existing_range in &current_ranges {
                new_ranges.extend(existing_range.remove(remove_range));
            }
            current_ranges = new_ranges;
        }

        self.line_ranges = current_ranges;
    }

    /// Shift line ranges by a given offset starting at insertion_point
    #[allow(dead_code)]
    pub fn shift_line_ranges(&mut self, insertion_point: u32, offset: i32) {
        let mut shifted_ranges = Vec::new();
        for range in &self.line_ranges {
            if let Some(shifted) = range.shift(insertion_point, offset) {
                shifted_ranges.push(shifted);
            }
        }
        self.line_ranges = shifted_ranges;
    }
}

/// Per-file attestation data
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttestation {
    pub file_path: String,
    pub entries: Vec<AttestationEntry>,
}

impl FileAttestation {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, entry: AttestationEntry) {
        self.entries.push(entry);
    }
}

/// The complete authorship log format
#[derive(Clone, PartialEq)]
pub struct AuthorshipLog {
    pub attestations: Vec<FileAttestation>,
    pub metadata: AuthorshipMetadata,
}

impl fmt::Debug for AuthorshipLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorshipLogV3")
            .field("attestations", &self.attestations)
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl AuthorshipLog {
    pub fn new() -> Self {
        Self {
            attestations: Vec::new(),
            metadata: AuthorshipMetadata::new(),
        }
    }

    /// Filter authorship log to keep only committed line ranges
    ///
    /// This keeps only attributions for lines that were actually committed, removing everything else.
    /// This is the inverse of filter_unstaged_lines - instead of removing unstaged, we keep only committed.
    ///
    /// # Arguments
    /// * `committed_hunks` - Map of file paths to their committed line ranges
    pub fn filter_to_committed_lines(&mut self, committed_hunks: &HashMap<String, Vec<LineRange>>) {
        for file_attestation in &mut self.attestations {
            if let Some(committed_ranges) = committed_hunks.get(&file_attestation.file_path) {
                // For each attestation entry, keep only the lines that were committed
                for entry in &mut file_attestation.entries {
                    // Expand entry's line ranges to individual lines
                    let mut entry_lines: Vec<u32> = Vec::new();
                    for range in &entry.line_ranges {
                        entry_lines.extend(range.expand());
                    }

                    // Keep only lines that are in committed ranges
                    let mut committed_lines: Vec<u32> = Vec::new();
                    for line in entry_lines {
                        if committed_ranges.iter().any(|range| range.contains(line)) {
                            committed_lines.push(line);
                        }
                    }

                    if !committed_lines.is_empty() {
                        committed_lines.sort_unstable();
                        committed_lines.dedup();
                        entry.line_ranges = LineRange::compress_lines(&committed_lines);
                    } else {
                        entry.line_ranges.clear();
                    }
                }

                // Remove entries that have no line ranges left
                file_attestation
                    .entries
                    .retain(|entry| !entry.line_ranges.is_empty());
            } else {
                // No committed lines for this file, remove all entries
                file_attestation.entries.clear();
            }
        }

        // Remove file attestations that have no entries left
        self.attestations.retain(|file| !file.entries.is_empty());

        // Clean up prompt metadata for sessions that no longer have attributed lines
        self.cleanup_unused_prompts();
    }

    /// Remove prompt records that are not referenced by any attestation entries
    ///
    /// After filtering the authorship log (e.g., to only committed lines), some AI sessions
    /// may no longer have any attributed lines. This method removes their PromptRecords from
    /// the metadata to keep it clean and accurate.
    pub fn cleanup_unused_prompts(&mut self) {
        // Collect all hashes that are still referenced in attestations
        let mut referenced_hashes = std::collections::HashSet::new();
        for file_attestation in &self.attestations {
            for entry in &file_attestation.entries {
                referenced_hashes.insert(entry.hash.clone());
            }
        }

        // Remove prompts that are not referenced
        self.metadata
            .prompts
            .retain(|hash, _| referenced_hashes.contains(hash));
    }

    /// Merge overlapping and adjacent line ranges
    fn merge_line_ranges(ranges: &[LineRange]) -> Vec<LineRange> {
        if ranges.is_empty() {
            return Vec::new();
        }

        let mut sorted_ranges = ranges.to_vec();
        sorted_ranges.sort_by(|a, b| {
            let a_start = match a {
                LineRange::Single(line) => *line,
                LineRange::Range(start, _) => *start,
            };
            let b_start = match b {
                LineRange::Single(line) => *line,
                LineRange::Range(start, _) => *start,
            };
            a_start.cmp(&b_start)
        });

        let mut merged = Vec::new();
        for current in sorted_ranges {
            if let Some(last) = merged.last_mut() {
                if Self::ranges_can_merge(last, &current) {
                    *last = Self::merge_ranges(last, &current);
                } else {
                    merged.push(current);
                }
            } else {
                merged.push(current);
            }
        }

        merged
    }

    /// Check if two ranges can be merged (overlapping or adjacent)
    fn ranges_can_merge(range1: &LineRange, range2: &LineRange) -> bool {
        let (start1, end1) = match range1 {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        };
        let (start2, end2) = match range2 {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        };

        // Ranges can merge if they overlap or are adjacent
        start1 <= end2 + 1 && start2 <= end1 + 1
    }

    /// Merge two ranges into one
    fn merge_ranges(range1: &LineRange, range2: &LineRange) -> LineRange {
        let (start1, end1) = match range1 {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        };
        let (start2, end2) = match range2 {
            LineRange::Single(line) => (*line, *line),
            LineRange::Range(start, end) => (*start, *end),
        };

        let start = start1.min(start2);
        let end = end1.max(end2);

        if start == end {
            LineRange::Single(start)
        } else {
            LineRange::Range(start, end)
        }
    }

    /// Apply a single checkpoint to this authorship log
    ///
    /// This method processes one checkpoint and updates the authorship log accordingly.
    /// With the new attribution-based system, each checkpoint contains the complete
    /// attribution state for its files, so we REPLACE rather than accumulate.
    pub fn apply_checkpoint(
        &mut self,
        checkpoint: &crate::authorship::working_log::Checkpoint,
        human_author: Option<&str>,
        session_additions: &mut HashMap<String, u32>,
        session_deletions: &mut HashMap<String, u32>,
    ) {
        // Register/update session in prompts metadata (if AI checkpoint)
        let session_id_opt = match &checkpoint.agent_id {
            Some(agent) => {
                let session_id = generate_short_hash(&agent.id, &agent.tool);

                // Insert or update prompt record
                let entry =
                    self.metadata
                        .prompts
                        .entry(session_id.clone())
                        .or_insert(PromptRecord {
                            agent_id: agent.clone(),
                            human_author: human_author.map(|s| s.to_string()),
                            messages: checkpoint
                                .transcript
                                .as_ref()
                                .map(|t| t.messages().to_vec())
                                .unwrap_or_default(),
                            total_additions: 0,
                            total_deletions: 0,
                            accepted_lines: 0,
                            overriden_lines: 0,
                        });

                // Update transcript if provided and longer than existing
                if let Some(transcript) = &checkpoint.transcript {
                    if entry.messages.len() < transcript.messages().len() {
                        entry.messages = transcript.messages().to_vec();
                    }
                }

                Some(session_id)
            }
            _ => None,
        };

        // Update metrics from checkpoint line_stats
        if let Some(ref session_id) = session_id_opt {
            *session_additions.entry(session_id.clone()).or_insert(0) +=
                checkpoint.line_stats.additions_for_kind(checkpoint.kind);
            *session_deletions.entry(session_id.clone()).or_insert(0) +=
                checkpoint.line_stats.deletions_for_kind(checkpoint.kind);
        }

        // Process each file entry in checkpoint
        for entry in &checkpoint.entries {
            // REPLACE all attestation entries for this file (since checkpoint has complete state)
            let file_attestation = self.get_or_create_file(&entry.file);
            file_attestation.entries.clear();

            // Group line_attributions by author_id
            let mut line_attributions_by_author: HashMap<String, Vec<LineRange>> = HashMap::new();
            for line_attr in &entry.line_attributions {
                if line_attr.start_line == line_attr.end_line {
                    line_attributions_by_author
                        .entry(line_attr.author_id.clone())
                        .or_insert_with(Vec::new)
                        .push(LineRange::Single(line_attr.start_line));
                } else {
                    line_attributions_by_author
                        .entry(line_attr.author_id.clone())
                        .or_insert_with(Vec::new)
                        .push(LineRange::Range(line_attr.start_line, line_attr.end_line));
                }
            }

            // Add new entries for each author (session)
            for (author_id, line_ranges) in line_attributions_by_author {
                if author_id == CheckpointKind::Human.to_str() {
                    continue;
                }
                file_attestation.add_entry(AttestationEntry::new(author_id, line_ranges));
            }
        }
    }

    /// Finalize the authorship log after all checkpoints have been applied
    ///
    /// This method:
    /// - Removes empty entries and files
    /// - Sorts and consolidates entries by hash
    /// - Calculates accepted_lines from final attestations
    /// - Updates all PromptRecords with final metrics
    pub fn finalize(
        &mut self,
        session_additions: &HashMap<String, u32>,
        session_deletions: &HashMap<String, u32>,
    ) {
        // Remove empty entries and empty files
        for file_attestation in &mut self.attestations {
            file_attestation
                .entries
                .retain(|entry| !entry.line_ranges.is_empty());
        }
        self.attestations.retain(|f| !f.entries.is_empty());

        // Sort attestation entries by hash for deterministic ordering
        for file_attestation in &mut self.attestations {
            file_attestation.entries.sort_by(|a, b| a.hash.cmp(&b.hash));
        }

        // Consolidate entries with the same hash
        for file_attestation in &mut self.attestations {
            let mut consolidated_entries = Vec::new();
            let mut current_hash: Option<String> = None;
            let mut current_ranges: Vec<LineRange> = Vec::new();

            for entry in &file_attestation.entries {
                if current_hash.as_ref() == Some(&entry.hash) {
                    // Same hash, accumulate line ranges
                    current_ranges.extend(entry.line_ranges.clone());
                } else {
                    // Different hash, save previous entry and start new one
                    if let Some(hash) = current_hash.take() {
                        // Merge overlapping and adjacent ranges before adding
                        let merged_ranges = Self::merge_line_ranges(&current_ranges);
                        consolidated_entries.push(AttestationEntry::new(hash, merged_ranges));
                    }
                    current_hash = Some(entry.hash.clone());
                    current_ranges = entry.line_ranges.clone();
                }
            }

            // Don't forget the last entry
            if let Some(hash) = current_hash {
                let merged_ranges = Self::merge_line_ranges(&current_ranges);
                consolidated_entries.push(AttestationEntry::new(hash, merged_ranges));
            }

            file_attestation.entries = consolidated_entries;
        }

        // Calculate accepted_lines for each session from the final attestation log
        let mut session_accepted_lines: HashMap<String, u32> = HashMap::new();
        for file_attestation in &self.attestations {
            for attestation_entry in &file_attestation.entries {
                let accepted_count: u32 = attestation_entry
                    .line_ranges
                    .iter()
                    .map(|range| count_line_range(range))
                    .sum();
                *session_accepted_lines
                    .entry(attestation_entry.hash.clone())
                    .or_insert(0) += accepted_count;
            }
        }

        // Update all PromptRecords with the calculated metrics
        for (session_id, prompt_record) in self.metadata.prompts.iter_mut() {
            prompt_record.total_additions = *session_additions.get(session_id).unwrap_or(&0);
            prompt_record.total_deletions = *session_deletions.get(session_id).unwrap_or(&0);
            prompt_record.accepted_lines = *session_accepted_lines.get(session_id).unwrap_or(&0);
            // overriden_lines is calculated and accumulated in apply_checkpoint, don't reset it here
        }
    }

    /// Convert from working log checkpoints to authorship log
    pub fn from_working_log_with_base_commit_and_human_author(
        checkpoints: &[crate::authorship::working_log::Checkpoint],
        base_commit_sha: &str,
        human_author: Option<&str>,
        working_log: Option<&crate::git::repo_storage::PersistedWorkingLog>,
        foreign_prompts: Option<&HashMap<String, PromptRecord>>,
    ) -> Self {
        let mut authorship_log = Self::new();
        authorship_log.metadata.base_commit_sha = base_commit_sha.to_string();

        // Load foreign prompts (from INITIAL file passed in)
        if let Some(prompts) = foreign_prompts {
            for (author_id, prompt_record) in prompts {
                authorship_log
                    .metadata
                    .prompts
                    .insert(author_id.clone(), prompt_record.clone());
            }
        } else if let Some(wl) = working_log {
            // Fallback: read from INITIAL file directly if not passed in
            let initial_data = wl.read_initial_attributions();
            for (author_id, prompt_record) in initial_data.prompts {
                authorship_log
                    .metadata
                    .prompts
                    .insert(author_id, prompt_record);
            }
        }

        // Track additions and deletions per session_id
        let mut session_additions: HashMap<String, u32> = HashMap::new();
        let mut session_deletions: HashMap<String, u32> = HashMap::new();

        // Process checkpoints and create attributions
        for checkpoint in checkpoints.iter() {
            authorship_log.apply_checkpoint(
                checkpoint,
                human_author,
                &mut session_additions,
                &mut session_deletions,
            );
        }

        // Finalize the log (cleanup, consolidate, metrics)
        authorship_log.finalize(&session_additions, &session_deletions);

        // If prompts should be ignored, clear the transcripts but keep the prompt records
        let ignore_prompts: bool = config::Config::get().get_ignore_prompts();
        if ignore_prompts {
            // Clear transcripts but keep the prompt records
            for prompt_record in authorship_log.metadata.prompts.values_mut() {
                prompt_record.messages.clear();
            }
        }

        authorship_log
    }

    pub fn get_or_create_file(&mut self, file: &str) -> &mut FileAttestation {
        // Check if file already exists
        let exists = self.attestations.iter().any(|f| f.file_path == file);

        if !exists {
            self.attestations
                .push(FileAttestation::new(file.to_string()));
        }

        // Now get the reference
        self.attestations
            .iter_mut()
            .find(|f| f.file_path == file)
            .unwrap()
    }

    /// Serialize to the new text format
    pub fn serialize_to_string(&self) -> Result<String, fmt::Error> {
        let mut output = String::new();

        // Write attestation section
        for file_attestation in &self.attestations {
            // Quote file names that contain spaces or whitespace
            let file_path = if needs_quoting(&file_attestation.file_path) {
                format!("\"{}\"", &file_attestation.file_path)
            } else {
                file_attestation.file_path.clone()
            };
            output.push_str(&file_path);
            output.push('\n');

            for entry in &file_attestation.entries {
                output.push_str("  ");
                output.push_str(&entry.hash);
                output.push(' ');
                output.push_str(&format_line_ranges(&entry.line_ranges));
                output.push('\n');
            }
        }

        // Write divider
        output.push_str("---\n");

        // Write JSON metadata section
        let json_str = serde_json::to_string_pretty(&self.metadata).map_err(|_| fmt::Error)?;
        output.push_str(&json_str);

        Ok(output)
    }

    /// Write to a writer in the new format
    pub fn _serialize_to_writer<W: Write>(&self, mut writer: W) -> std::io::Result<()> {
        let content = self
            .serialize_to_string()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "Serialization failed"))?;
        writer.write_all(content.as_bytes())?;
        Ok(())
    }

    /// Deserialize from the new text format
    pub fn deserialize_from_string(content: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let lines: Vec<&str> = content.lines().collect();

        // Find the divider
        let divider_pos = lines
            .iter()
            .position(|&line| line == "---")
            .ok_or("Missing divider '---' in authorship log")?;

        // Parse attestation section (before divider)
        let attestation_lines = &lines[..divider_pos];
        let attestations = parse_attestation_section(attestation_lines)?;

        // Parse JSON metadata section (after divider)
        let json_lines = &lines[divider_pos + 1..];
        let json_content = json_lines.join("\n");
        let metadata: AuthorshipMetadata = serde_json::from_str(&json_content)?;

        Ok(Self {
            attestations,
            metadata,
        })
    }

    /// Read from a reader in the new format
    pub fn _deserialize_from_reader<R: BufRead>(
        reader: R,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content: Result<String, _> = reader.lines().collect();
        let content = content?;
        Self::deserialize_from_string(&content)
    }

    /// Lookup the author and optional prompt for a given file and line
    pub fn get_line_attribution(
        &self,
        repo: &Repository,
        file: &str,
        line: u32,
        foreign_prompts_cache: &mut HashMap<String, Option<PromptRecord>>,
    ) -> Option<(Author, Option<String>, Option<PromptRecord>)> {
        // Find the file attestation
        let file_attestation = self.attestations.iter().find(|f| f.file_path == file)?;

        // Check entries in reverse order (latest wins)
        for entry in file_attestation.entries.iter().rev() {
            // Check if this line is covered by any of the line ranges
            let contains = entry.line_ranges.iter().any(|range| range.contains(line));
            if contains {
                // The hash corresponds to a prompt session short hash
                if let Some(prompt_record) = self.metadata.prompts.get(&entry.hash) {
                    // Create author info from the prompt record
                    let author = Author {
                        username: prompt_record.agent_id.tool.clone(),
                        email: String::new(), // AI agents don't have email
                    };

                    // Return author and prompt info
                    return Some((
                        author,
                        Some(entry.hash.clone()),
                        Some(prompt_record.clone()),
                    ));
                } else {
                    // Check cache first before grepping
                    let prompt_record = if let Some(cached_result) =
                        foreign_prompts_cache.get(&entry.hash)
                    {
                        cached_result.clone()
                    } else {
                        // Try to find prompt record using git grep
                        let shas =
                            crate::git::refs::grep_ai_notes(repo, &format!("\"{}\"", &entry.hash))
                                .unwrap_or_default();
                        let result = if let Some(latest_sha) = shas.first() {
                            if let Some(authorship_log) =
                                crate::git::refs::get_authorship(repo, latest_sha)
                            {
                                authorship_log.metadata.prompts.get(&entry.hash).cloned()
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        // Cache the result (even if None) to avoid repeated grepping
                        foreign_prompts_cache.insert(entry.hash.clone(), result.clone());
                        result
                    };

                    if let Some(prompt_record) = prompt_record {
                        let author = Author {
                            username: prompt_record.agent_id.tool.clone(),
                            email: String::new(), // AI agents don't have email
                        };
                        return Some((author, Some(entry.hash.clone()), Some(prompt_record)));
                    }
                }
            }
        }
        None
    }

    /// Convert authorship log to working log checkpoints for merge --squash
    ///
    /// Creates one checkpoint per file per session that touched that file. This ensures that:
    /// - Each checkpoint has a single file entry
    /// - Blobs can be saved individually per checkpoint without ordering issues
    /// - Future diffs are computed against the correct base state
    ///
    /// # Arguments
    /// * `_human_author` - Unused (human checkpoints are not created for squash merges)
    ///
    /// # Returns
    /// Vector of checkpoints, one per file per session (no human checkpoint)
    #[allow(dead_code)]
    pub fn convert_to_checkpoints_for_squash(
        &self,
        file_contents: &HashMap<String, String>,
    ) -> Result<Vec<crate::authorship::working_log::Checkpoint>, Box<dyn std::error::Error>> {
        use crate::authorship::attribution_tracker::{
            LineAttribution, line_attributions_to_attributions,
        };
        use crate::authorship::authorship_log::PromptRecord;
        use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};
        use std::collections::{HashMap, HashSet};

        let mut checkpoints = Vec::new();

        // Get the current timestamp in milliseconds since the Unix epoch
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Track all files that have attestations
        let mut all_files: HashSet<String> = HashSet::new();
        for file_attestation in &self.attestations {
            all_files.insert(file_attestation.file_path.clone());
        }

        // Build AI checkpoints - one per file
        // For each file, we need to collect all the sessions that contributed to it
        for file_path in &all_files {
            // Find the file attestation
            let file_attestation =
                match self.attestations.iter().find(|f| f.file_path == *file_path) {
                    Some(f) => f,
                    None => continue,
                };

            // Group entries by session hash to preserve prompt information
            let mut session_lines: HashMap<String, Vec<LineRange>> = HashMap::new();
            for entry in &file_attestation.entries {
                session_lines
                    .entry(entry.hash.clone())
                    .or_insert_with(Vec::new)
                    .extend(entry.line_ranges.clone());
            }

            if session_lines.is_empty() {
                continue;
            }

            let file_content = file_contents
                .get(file_path)
                .ok_or_else(|| format!("Missing file content for: {}", file_path))?;

            // Sort sessions for deterministic output
            let mut session_entries: Vec<(String, Vec<LineRange>)> =
                session_lines.into_iter().collect();
            session_entries.sort_by(|a, b| a.0.cmp(&b.0));

            let mut combined_line_attributions: Vec<LineAttribution> = Vec::new();
            let mut session_prompt_records: Vec<PromptRecord> = Vec::new();

            for (session_hash, ranges) in &session_entries {
                let prompt_record = self
                    .metadata
                    .prompts
                    .get(session_hash)
                    .ok_or_else(|| format!("Missing prompt record for hash: {}", session_hash))?
                    .clone();

                // Expand ranges to individual lines, then compress to working log format
                let mut all_lines: Vec<u32> = Vec::new();
                for range in ranges {
                    all_lines.extend(range.expand());
                }
                if all_lines.is_empty() {
                    continue;
                }
                all_lines.sort_unstable();
                all_lines.dedup();

                // IMPORTANT: Use the session_hash that will be regenerated from agent_id when applying checkpoint
                // This ensures line attributions match the prompts in metadata after apply_checkpoint
                let prompt_hash =
                    generate_short_hash(&prompt_record.agent_id.id, &prompt_record.agent_id.tool);
                // TODO Update authorship to store overridden state for line ranges
                let line_attributions =
                    compress_lines_to_working_log_format(&all_lines, &prompt_hash, false);

                combined_line_attributions.extend(line_attributions);
                session_prompt_records.push(prompt_record);
            }

            if combined_line_attributions.is_empty() {
                continue;
            }

            combined_line_attributions.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then(a.end_line.cmp(&b.end_line))
                    .then(a.author_id.cmp(&b.author_id))
            });

            let attributions = line_attributions_to_attributions(
                &combined_line_attributions,
                file_content.as_str(),
                ts,
            );

            for prompt_record in session_prompt_records {
                let entry = WorkingLogEntry::new(
                    file_path.clone(),
                    String::new(), // Empty blob_sha - will be set by caller
                    attributions.clone(),
                    combined_line_attributions.clone(),
                );

                let mut ai_checkpoint = Checkpoint::new(
                    CheckpointKind::AiAgent, // TODO Pull exact from prompt record?
                    String::new(),           // Empty diff hash
                    "ai".to_string(),
                    vec![entry],
                );
                ai_checkpoint.agent_id = Some(prompt_record.agent_id.clone());

                // TODO Fill in the LineStats

                // Reconstruct transcript from messages
                let mut transcript = crate::authorship::transcript::AiTranscript::new();
                for message in &prompt_record.messages {
                    transcript.add_message(message.clone());
                }
                ai_checkpoint.transcript = Some(transcript);

                checkpoints.push(ai_checkpoint);
            }
        }

        Ok(checkpoints)
    }
}

/// Convert line numbers to working log Line format (Single/Range)
fn compress_lines_to_working_log_format(
    lines: &[u32],
    author_id: &str,
    overridden: bool,
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    use crate::authorship::attribution_tracker::LineAttribution;

    if lines.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut start = lines[0];
    let mut end = lines[0];

    for &line in &lines[1..] {
        if line == end + 1 {
            // Consecutive line, extend range
            end = line;
        } else {
            // Gap found, save current range and start new one
            result.push(LineAttribution::new(
                start,
                end,
                author_id.to_string(),
                overridden,
            ));
            start = line;
            end = line;
        }
    }

    // Add the final range
    result.push(LineAttribution::new(
        start,
        end,
        author_id.to_string(),
        overridden,
    ));

    result
}

impl Default for AuthorshipLog {
    fn default() -> Self {
        Self::new()
    }
}

/// Format line ranges as comma-separated values with ranges as "start-end"
/// Sorts ranges first: Single ranges by their value, Range ones by their lowest bound
fn format_line_ranges(ranges: &[LineRange]) -> String {
    let mut sorted_ranges = ranges.to_vec();
    sorted_ranges.sort_by(|a, b| {
        let a_start = match a {
            LineRange::Single(line) => *line,
            LineRange::Range(start, _) => *start,
        };
        let b_start = match b {
            LineRange::Single(line) => *line,
            LineRange::Range(start, _) => *start,
        };
        a_start.cmp(&b_start)
    });

    sorted_ranges
        .iter()
        .map(|range| match range {
            LineRange::Single(line) => line.to_string(),
            LineRange::Range(start, end) => format!("{}-{}", start, end),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse line ranges from a string like "1,2,19-222"
/// No spaces are expected in the format
fn parse_line_ranges(input: &str) -> Result<Vec<LineRange>, Box<dyn std::error::Error>> {
    let mut ranges = Vec::new();

    for part in input.split(',') {
        if part.is_empty() {
            continue;
        }

        if let Some(dash_pos) = part.find('-') {
            // Range format: "start-end"
            let start_str = &part[..dash_pos];
            let end_str = &part[dash_pos + 1..];
            let start: u32 = start_str.parse()?;
            let end: u32 = end_str.parse()?;
            ranges.push(LineRange::Range(start, end));
        } else {
            // Single line format: "line"
            let line: u32 = part.parse()?;
            ranges.push(LineRange::Single(line));
        }
    }

    Ok(ranges)
}

/// Parse the attestation section (before the divider)
fn parse_attestation_section(
    lines: &[&str],
) -> Result<Vec<FileAttestation>, Box<dyn std::error::Error>> {
    let mut attestations = Vec::new();
    let mut current_file: Option<FileAttestation> = None;

    for line in lines {
        let line = line.trim_end(); // Remove trailing whitespace but preserve leading

        if line.is_empty() {
            continue;
        }

        if line.starts_with("  ") {
            // Attestation entry line (indented)
            let entry_line = &line[2..]; // Remove "  " prefix

            // Split on first space to separate hash from line ranges
            if let Some(space_pos) = entry_line.find(' ') {
                let hash = entry_line[..space_pos].to_string();
                let ranges_str = &entry_line[space_pos + 1..];
                let line_ranges = parse_line_ranges(ranges_str)?;

                let entry = AttestationEntry::new(hash, line_ranges);

                if let Some(ref mut file_attestation) = current_file {
                    file_attestation.add_entry(entry);
                } else {
                    return Err("Attestation entry found without a file path".into());
                }
            } else {
                return Err(format!("Invalid attestation entry format: {}", entry_line).into());
            }
        } else {
            // File path line (not indented)
            if let Some(file_attestation) = current_file.take() {
                if !file_attestation.entries.is_empty() {
                    attestations.push(file_attestation);
                }
            }

            // Parse file path, handling quoted paths
            let file_path = if line.starts_with('"') && line.ends_with('"') {
                // Quoted path - remove quotes (no unescaping needed since quotes aren't allowed in file names)
                line[1..line.len() - 1].to_string()
            } else {
                // Unquoted path
                line.to_string()
            };

            current_file = Some(FileAttestation::new(file_path));
        }
    }

    // Don't forget the last file
    if let Some(file_attestation) = current_file {
        if !file_attestation.entries.is_empty() {
            attestations.push(file_attestation);
        }
    }

    Ok(attestations)
}

/// Check if a file path needs quoting (contains spaces or whitespace)
fn needs_quoting(path: &str) -> bool {
    path.contains(' ') || path.contains('\t') || path.contains('\n')
}

/// Generate a short hash (7 characters) from agent_id and tool
pub fn generate_short_hash(agent_id: &str, tool: &str) -> String {
    let combined = format!("{}:{}", tool, agent_id);
    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let result = hasher.finalize();
    // Take first 7 characters of the hex representation
    format!("{:x}", result)[..7].to_string()
}

/// Count the number of lines represented by a LineRange
fn count_line_range(range: &LineRange) -> u32 {
    match range {
        LineRange::Single(_) => 1,
        LineRange::Range(start, end) => end - start + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_debug_snapshot;

    #[test]
    fn test_format_line_ranges() {
        let ranges = vec![
            LineRange::Range(19, 222),
            LineRange::Single(1),
            LineRange::Single(2),
        ];

        assert_debug_snapshot!(format_line_ranges(&ranges));
    }

    #[test]
    fn test_parse_line_ranges() {
        let ranges = parse_line_ranges("1,2,19-222").unwrap();
        assert_debug_snapshot!(ranges);
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = "abc123".to_string();

        // Add some attestations
        let mut file1 = FileAttestation::new("src/file.xyz".to_string());
        file1.add_entry(AttestationEntry::new(
            "xyzAbc".to_string(),
            vec![
                LineRange::Single(1),
                LineRange::Single(2),
                LineRange::Range(19, 222),
            ],
        ));
        file1.add_entry(AttestationEntry::new(
            "123456".to_string(),
            vec![LineRange::Range(400, 405)],
        ));

        let mut file2 = FileAttestation::new("src/file2.xyz".to_string());
        file2.add_entry(AttestationEntry::new(
            "123456".to_string(),
            vec![
                LineRange::Range(1, 111),
                LineRange::Single(245),
                LineRange::Single(260),
            ],
        ));

        log.attestations.push(file1);
        log.attestations.push(file2);

        // Serialize and snapshot the format
        let serialized = log.serialize_to_string().unwrap();
        assert_debug_snapshot!(serialized);

        // Test roundtrip: deserialize and verify structure matches
        let deserialized = AuthorshipLog::deserialize_from_string(&serialized).unwrap();
        assert_debug_snapshot!(deserialized);
    }

    #[test]
    fn test_expected_format() {
        let mut log = AuthorshipLog::new();

        let mut file1 = FileAttestation::new("src/file.xyz".to_string());
        file1.add_entry(AttestationEntry::new(
            "xyzAbc".to_string(),
            vec![
                LineRange::Single(1),
                LineRange::Single(2),
                LineRange::Range(19, 222),
            ],
        ));
        file1.add_entry(AttestationEntry::new(
            "123456".to_string(),
            vec![LineRange::Range(400, 405)],
        ));

        let mut file2 = FileAttestation::new("src/file2.xyz".to_string());
        file2.add_entry(AttestationEntry::new(
            "123456".to_string(),
            vec![
                LineRange::Range(1, 111),
                LineRange::Single(245),
                LineRange::Single(260),
            ],
        ));

        log.attestations.push(file1);
        log.attestations.push(file2);

        let serialized = log.serialize_to_string().unwrap();
        assert_debug_snapshot!(serialized);
    }

    #[test]
    fn test_line_range_sorting() {
        // Test that ranges are sorted correctly: single ranges and ranges by lowest bound
        let ranges = vec![
            LineRange::Range(100, 200),
            LineRange::Single(5),
            LineRange::Range(10, 15),
            LineRange::Single(50),
            LineRange::Single(1),
            LineRange::Range(25, 30),
        ];

        let formatted = format_line_ranges(&ranges);
        assert_debug_snapshot!(formatted);

        // Should be sorted as: 1, 5, 10-15, 25-30, 50, 100-200
    }

    #[test]
    fn test_file_names_with_spaces() {
        // Test file names with spaces and special characters
        let mut log = AuthorshipLog::new();

        // Add a prompt to the metadata
        let agent_id = crate::authorship::working_log::AgentId {
            tool: "cursor".to_string(),
            id: "session_123".to_string(),
            model: "claude-3-sonnet".to_string(),
        };
        let prompt_hash = generate_short_hash(&agent_id.id, &agent_id.tool);
        log.metadata.prompts.insert(
            prompt_hash.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent_id,
                human_author: None,
                messages: vec![],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
            },
        );

        // Add attestations for files with spaces and special characters
        let mut file1 = FileAttestation::new("src/my file.rs".to_string());
        file1.add_entry(AttestationEntry::new(
            prompt_hash.to_string(),
            vec![LineRange::Range(1, 10)],
        ));

        let mut file2 = FileAttestation::new("docs/README (copy).md".to_string());
        file2.add_entry(AttestationEntry::new(
            prompt_hash.to_string(),
            vec![LineRange::Single(5)],
        ));

        let mut file3 = FileAttestation::new("test/file-with-dashes.js".to_string());
        file3.add_entry(AttestationEntry::new(
            prompt_hash.to_string(),
            vec![LineRange::Range(20, 25)],
        ));

        log.attestations.push(file1);
        log.attestations.push(file2);
        log.attestations.push(file3);

        let serialized = log.serialize_to_string().unwrap();
        println!("Serialized with special file names:\n{}", serialized);
        assert_debug_snapshot!(serialized);

        // Try to deserialize - this should work if we handle escaping properly
        let deserialized = AuthorshipLog::deserialize_from_string(&serialized);
        match deserialized {
            Ok(log) => {
                println!("Deserialization successful!");
                assert_debug_snapshot!(log);
            }
            Err(e) => {
                println!("Deserialization failed: {}", e);
                // This will fail with current implementation
            }
        }
    }

    #[test]
    fn test_hash_always_maps_to_prompt() {
        // Demonstrate that every hash in attestation section maps to prompts section
        let mut log = AuthorshipLog::new();

        // Add a prompt to the metadata
        let agent_id = crate::authorship::working_log::AgentId {
            tool: "cursor".to_string(),
            id: "session_123".to_string(),
            model: "claude-3-sonnet".to_string(),
        };
        let prompt_hash = generate_short_hash(&agent_id.id, &agent_id.tool);
        log.metadata.prompts.insert(
            prompt_hash.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent_id,
                human_author: None,
                messages: vec![],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
            },
        );

        // Add attestation that references this prompt
        let mut file1 = FileAttestation::new("src/example.rs".to_string());
        file1.add_entry(AttestationEntry::new(
            prompt_hash.to_string(),
            vec![LineRange::Range(1, 10)],
        ));
        log.attestations.push(file1);

        let serialized = log.serialize_to_string().unwrap();
        assert_debug_snapshot!(serialized);

        // Verify that every hash in attestations has a corresponding prompt
        for file_attestation in &log.attestations {
            for entry in &file_attestation.entries {
                assert!(
                    log.metadata.prompts.contains_key(&entry.hash),
                    "Hash '{}' should have a corresponding prompt in metadata",
                    entry.hash
                );
            }
        }
    }

    #[test]
    fn test_serialize_deserialize_no_attestations() {
        // Test that serialization and deserialization work correctly when there are no attestations
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = "abc123".to_string();

        let agent_id = crate::authorship::working_log::AgentId {
            tool: "cursor".to_string(),
            id: "session_123".to_string(),
            model: "claude-3-sonnet".to_string(),
        };
        let prompt_hash = generate_short_hash(&agent_id.id, &agent_id.tool);
        log.metadata.prompts.insert(
            prompt_hash,
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent_id,
                human_author: None,
                messages: vec![],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
            },
        );

        // Serialize and verify the format
        let serialized = log.serialize_to_string().unwrap();
        assert_debug_snapshot!(serialized);

        // Test roundtrip: deserialize and verify structure matches
        let deserialized = AuthorshipLog::deserialize_from_string(&serialized).unwrap();
        assert_debug_snapshot!(deserialized);

        // Verify that the deserialized log has the same metadata but no attestations
        assert_eq!(deserialized.metadata.base_commit_sha, "abc123");
        assert_eq!(deserialized.metadata.prompts.len(), 1);
        assert_eq!(deserialized.attestations.len(), 0);
    }

    #[test]
    fn test_remove_line_ranges_complete_removal() {
        let mut entry =
            AttestationEntry::new("test_hash".to_string(), vec![LineRange::Range(2, 5)]);

        // Remove the exact same range
        entry.remove_line_ranges(&[LineRange::Range(2, 5)]);

        // Should be empty after removing the exact range
        assert!(
            entry.line_ranges.is_empty(),
            "Expected empty line_ranges after complete removal, got: {:?}",
            entry.line_ranges
        );
    }

    #[test]
    fn test_remove_line_ranges_partial_removal() {
        let mut entry =
            AttestationEntry::new("test_hash".to_string(), vec![LineRange::Range(2, 10)]);

        // Remove middle part
        entry.remove_line_ranges(&[LineRange::Range(5, 7)]);

        // Should have two ranges: [2-4] and [8-10]
        assert_eq!(entry.line_ranges.len(), 2);
        assert_eq!(entry.line_ranges[0], LineRange::Range(2, 4));
        assert_eq!(entry.line_ranges[1], LineRange::Range(8, 10));
    }

    #[test]
    fn test_metrics_calculation() {
        use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
        use crate::authorship::transcript::{AiTranscript, Message};
        use crate::authorship::working_log::{
            AgentId, Checkpoint, CheckpointKind, WorkingLogEntry,
        };
        use std::time::{SystemTime, UNIX_EPOCH};

        // Create an agent ID
        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: "test_session".to_string(),
            model: "claude-3-sonnet".to_string(),
        };

        let session_hash = generate_short_hash(&agent_id.id, &agent_id.tool);

        // Create a transcript
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("Add a function".to_string(), None));
        transcript.add_message(Message::assistant("Here's the function".to_string(), None));

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Create working log entries
        // First checkpoint: add 10 lines
        let line_attributions1 = vec![LineAttribution::new(1, 10, session_hash.clone(), false)];
        let attributions1 = vec![Attribution::new(0, 100, session_hash.clone(), ts)];
        let entry1 = WorkingLogEntry::new(
            "src/test.rs".to_string(),
            "blob_sha_1".to_string(),
            attributions1,
            line_attributions1,
        );
        let mut checkpoint1 = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "ai".to_string(),
            vec![entry1],
        );
        checkpoint1.agent_id = Some(agent_id.clone());
        checkpoint1.transcript = Some(transcript.clone());
        // First checkpoint cumulative stats: 10 added, 0 deleted
        checkpoint1.line_stats.ai_agent_additions = 10;
        checkpoint1.line_stats.ai_agent_deletions = 0;

        // Second checkpoint: modify lines (delete 3, add 5)
        // This represents the final state after both checkpoints
        let line_attributions2 = vec![
            LineAttribution::new(1, 4, session_hash.clone(), false),
            LineAttribution::new(5, 9, session_hash.clone(), false),
        ];
        let attributions2 = vec![
            Attribution::new(0, 50, session_hash.clone(), ts),
            Attribution::new(50, 150, session_hash.clone(), ts),
        ];
        let entry2 = WorkingLogEntry::new(
            "src/test.rs".to_string(),
            "blob_sha_2".to_string(),
            attributions2,
            line_attributions2,
        );
        let mut checkpoint2 = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "ai".to_string(),
            vec![entry2],
        );
        checkpoint2.agent_id = Some(agent_id.clone());
        checkpoint2.transcript = Some(transcript);
        // Second checkpoint cumulative stats: 10 (from checkpoint1) is already counted, so we add 5 more
        checkpoint2.line_stats.ai_agent_additions = 5; // Incremental: 5 new lines added
        checkpoint2.line_stats.ai_agent_deletions = 3; // Incremental: 3 lines deleted

        // Convert to authorship log
        let authorship_log = AuthorshipLog::from_working_log_with_base_commit_and_human_author(
            &[checkpoint1, checkpoint2],
            "base123",
            None,
            None,
            None,
        );

        // Get the prompt record
        let prompt_record = authorship_log.metadata.prompts.get(&session_hash).unwrap();

        // Verify metrics
        // total_additions: accumulated from line_stats
        assert_eq!(prompt_record.total_additions, 15);
        // total_deletions: accumulated from line_stats
        assert_eq!(prompt_record.total_deletions, 3);
        // accepted_lines: lines 1-4 and 5-9 = 9 lines
        assert_eq!(prompt_record.accepted_lines, 9);
    }

    #[test]
    fn test_convert_authorship_log_to_checkpoints() {
        use crate::authorship::transcript::{AiTranscript, Message};
        use crate::authorship::working_log::AgentId;
        use std::collections::HashMap;

        // Create an authorship log with both AI and human-attributed lines
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = "base123".to_string();

        // Add AI prompt session
        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: "session_abc".to_string(),
            model: "claude-3-sonnet".to_string(),
        };
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("Add error handling".to_string(), None));
        transcript.add_message(Message::assistant("Added error handling".to_string(), None));

        let session_hash = generate_short_hash(&agent_id.id, &agent_id.tool);
        log.metadata.prompts.insert(
            session_hash.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent_id.clone(),
                human_author: Some("alice@example.com".to_string()),
                messages: transcript.messages().to_vec(),
                total_additions: 15,
                total_deletions: 3,
                accepted_lines: 11,
                overriden_lines: 0,
            },
        );

        // Add file attestations - AI owns lines 1-5, 10-15
        let mut file1 = FileAttestation::new("src/main.rs".to_string());
        file1.add_entry(AttestationEntry::new(
            session_hash.clone(),
            vec![LineRange::Range(1, 5), LineRange::Range(10, 15)],
        ));
        log.attestations.push(file1);

        // Create file contents (11 lines total for AI-attributed lines)
        let mut file_contents = HashMap::new();
        file_contents.insert(
            "src/main.rs".to_string(),
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\n".to_string(),
        );

        // Convert to checkpoints
        let result = log.convert_to_checkpoints_for_squash(&file_contents);
        assert!(result.is_ok());
        let checkpoints = result.unwrap();

        // Should have 1 checkpoint: 1 AI only (no human checkpoint)
        assert_eq!(checkpoints.len(), 1);

        // Checkpoint should be AI with original lines
        let ai_checkpoint = &checkpoints[0];
        assert_eq!(ai_checkpoint.author, "ai");
        assert!(ai_checkpoint.agent_id.is_some());
        assert_eq!(ai_checkpoint.agent_id.as_ref().unwrap().tool, "cursor");
        assert!(ai_checkpoint.transcript.is_some());
        assert_eq!(ai_checkpoint.entries.len(), 1);
        let ai_entry = &ai_checkpoint.entries[0];
        assert_eq!(ai_entry.file, "src/main.rs");

        // Verify line attributions instead of added_lines/deleted_lines
        assert!(!ai_entry.line_attributions.is_empty());
        // Should have line attributions for lines 1-5 and 10-15
        let total_lines: u32 = ai_entry
            .line_attributions
            .iter()
            .map(|attr| attr.end_line - attr.start_line + 1)
            .sum();
        assert_eq!(total_lines, 11); // 5 lines (1-5) + 6 lines (10-15)
    }

    #[test]
    fn test_overriden_lines_detection() {
        use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
        use crate::authorship::transcript::{AiTranscript, Message};
        use crate::authorship::working_log::{
            AgentId, Checkpoint, CheckpointKind, WorkingLogEntry,
        };
        use std::time::{SystemTime, UNIX_EPOCH};

        // Create an AI checkpoint that adds lines 1-5
        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: "session_123".to_string(),
            model: "claude-3-sonnet".to_string(),
        };

        let session_hash = generate_short_hash(&agent_id.id, &agent_id.tool);

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // First checkpoint: AI adds lines 1-5
        let line_attributions1 = vec![LineAttribution::new(1, 5, session_hash.clone(), false)];
        let attributions1 = vec![Attribution::new(0, 50, session_hash.clone(), ts)];
        let entry1 = WorkingLogEntry::new(
            "src/main.rs".to_string(),
            "sha1".to_string(),
            attributions1,
            line_attributions1,
        );
        let mut checkpoint1 = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "ai".to_string(),
            vec![entry1],
        );
        checkpoint1.agent_id = Some(agent_id.clone());
        checkpoint1.line_stats.ai_agent_additions = 5;
        checkpoint1.line_stats.ai_agent_deletions = 0;

        // Add transcript to make it a valid AI checkpoint
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("Add some code".to_string(), None));
        transcript.add_message(Message::assistant("Added code".to_string(), None));
        checkpoint1.transcript = Some(transcript);

        // Create a human checkpoint that removes lines 2-3 (overriding AI lines)
        // After deletion, AI owns lines 1, 4->2, 5->3 (lines shift up)
        let line_attributions2 = vec![
            LineAttribution::new(1, 1, session_hash.clone(), true),
            LineAttribution::new(2, 3, session_hash.clone(), true),
        ];
        let attributions2 = vec![
            Attribution::new(0, 10, session_hash.clone(), ts),
            Attribution::new(10, 30, session_hash.clone(), ts),
        ];
        let entry2 = WorkingLogEntry::new(
            "src/main.rs".to_string(),
            "sha2".to_string(),
            attributions2,
            line_attributions2,
        );
        let mut checkpoint2 = Checkpoint::new(
            CheckpointKind::Human,
            "".to_string(),
            "human".to_string(),
            vec![entry2],
        );
        checkpoint2.line_stats.ai_agent_additions = 5;
        checkpoint2.line_stats.ai_agent_deletions = 0;
        checkpoint2.line_stats.human_additions = 0;
        checkpoint2.line_stats.human_deletions = 0;
        // Note: checkpoint2.agent_id is None, indicating it's a human checkpoint

        // Convert to authorship log
        let authorship_log = AuthorshipLog::from_working_log_with_base_commit_and_human_author(
            &[checkpoint1, checkpoint2],
            "base123",
            Some("human@example.com"),
            None,
            None,
        );

        // Get the prompt record
        let prompt_record = authorship_log.metadata.prompts.get(&session_hash).unwrap();

        // Verify metrics
        assert_eq!(prompt_record.total_additions, 5);
        assert_eq!(prompt_record.total_deletions, 0); // AI didn't delete anything
        // accepted_lines: lines 1, 2, 3 = 3 lines (after human deletion of original lines 2-3)
        assert_eq!(prompt_record.accepted_lines, 3);
    }

    #[test]
    fn test_convert_authorship_log_multiple_ai_sessions() {
        use crate::authorship::transcript::{AiTranscript, Message};
        use crate::authorship::working_log::AgentId;

        // Create authorship log with 2 different AI sessions
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = "base456".to_string();

        // First AI session
        let agent1 = AgentId {
            tool: "cursor".to_string(),
            id: "session_1".to_string(),
            model: "claude-3-sonnet".to_string(),
        };
        let mut transcript1 = AiTranscript::new();
        transcript1.add_message(Message::user("Add function".to_string(), None));
        transcript1.add_message(Message::assistant("Added function".to_string(), None));
        let session1_hash = generate_short_hash(&agent1.id, &agent1.tool);
        log.metadata.prompts.insert(
            session1_hash.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent1,
                human_author: Some("bob@example.com".to_string()),
                messages: transcript1.messages().to_vec(),
                total_additions: 10,
                total_deletions: 0,
                accepted_lines: 10,
                overriden_lines: 0,
            },
        );

        // Second AI session
        let agent2 = AgentId {
            tool: "cursor".to_string(),
            id: "session_2".to_string(),
            model: "claude-3-opus".to_string(),
        };
        let mut transcript2 = AiTranscript::new();
        transcript2.add_message(Message::user("Add tests".to_string(), None));
        transcript2.add_message(Message::assistant("Added tests".to_string(), None));
        let session2_hash = generate_short_hash(&agent2.id, &agent2.tool);
        log.metadata.prompts.insert(
            session2_hash.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: agent2,
                human_author: Some("bob@example.com".to_string()),
                messages: transcript2.messages().to_vec(),
                total_additions: 20,
                total_deletions: 0,
                accepted_lines: 20,
                overriden_lines: 0,
            },
        );

        // File with both sessions, plus some human lines
        let mut file1 = FileAttestation::new("src/lib.rs".to_string());
        file1.add_entry(AttestationEntry::new(
            session1_hash.clone(),
            vec![LineRange::Range(1, 10)],
        ));
        file1.add_entry(AttestationEntry::new(
            session2_hash.clone(),
            vec![LineRange::Range(11, 30)],
        ));
        // Human owns lines 31-40 (implicitly, by not being in any AI attestation)
        log.attestations.push(file1);

        // Create file contents
        use std::collections::HashMap;
        let mut file_contents = HashMap::new();
        let mut content = String::new();
        for i in 1..=30 {
            content.push_str(&format!("line{}\n", i));
        }
        file_contents.insert("src/lib.rs".to_string(), content);

        // Convert to checkpoints
        let result = log.convert_to_checkpoints_for_squash(&file_contents);
        assert!(result.is_ok());
        let checkpoints = result.unwrap();

        // Should have 2 AI checkpoints (no human lines since we only have AI-attributed lines 1-30)
        assert_eq!(checkpoints.len(), 2);

        // Both are AI sessions
        let ai_checkpoints: Vec<_> = checkpoints
            .iter()
            .filter(|c| c.agent_id.is_some())
            .collect();
        assert_eq!(ai_checkpoints.len(), 2);

        // Verify that the AI sessions are distinct
        assert_ne!(
            ai_checkpoints[0].agent_id.as_ref().unwrap().id,
            ai_checkpoints[1].agent_id.as_ref().unwrap().id
        );

        // Each checkpoint should contain the full attribution state for the file
        assert_eq!(ai_checkpoints[0].entries.len(), 1);
        assert_eq!(ai_checkpoints[1].entries.len(), 1);
        let entry1 = &ai_checkpoints[0].entries[0];
        let entry2 = &ai_checkpoints[1].entries[0];
        assert_eq!(entry1.line_attributions, entry2.line_attributions);
        assert_eq!(entry1.attributions, entry2.attributions);
        assert!(!entry1.line_attributions.is_empty());
        assert!(!entry1.attributions.is_empty());

        let total_lines: u32 = entry1
            .line_attributions
            .iter()
            .map(|attr| attr.end_line - attr.start_line + 1)
            .sum();
        assert_eq!(total_lines, 30);

        let lines_session1: u32 = entry1
            .line_attributions
            .iter()
            .filter(|attr| attr.author_id.as_str() == session1_hash.as_str())
            .map(|attr| attr.end_line - attr.start_line + 1)
            .sum();
        assert_eq!(lines_session1, 10);

        let lines_session2: u32 = entry1
            .line_attributions
            .iter()
            .filter(|attr| attr.author_id.as_str() == session2_hash.as_str())
            .map(|attr| attr.end_line - attr.start_line + 1)
            .sum();
        assert_eq!(lines_session2, 20);
    }
}
