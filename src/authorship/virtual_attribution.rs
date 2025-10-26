use crate::authorship::attribution_tracker::{
    Attribution, LineAttribution, line_attributions_to_attributions,
};
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct VirtualAttributions {
    repo: Repository,
    base_commit: String,
    // Maps file path -> (char attributions, line attributions)
    attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
    // Maps file path -> file content
    file_contents: HashMap<String, String>,
    // Prompt records mapping session ID -> PromptRecord
    prompts: BTreeMap<String, PromptRecord>,
    // Timestamp to use for attributions
    ts: u128,
}

impl VirtualAttributions {
    /// Create a new VirtualAttributions for the given base commit with initial pathspecs
    pub async fn new_for_base_commit(
        repo: Repository,
        base_commit: String,
        pathspecs: &[String],
    ) -> Result<Self, GitAiError> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Load prompts from base commit's authorship log
        let prompts = match crate::git::refs::get_reference_as_authorship_log_v3(&repo, &base_commit) {
            Ok(log) => log.metadata.prompts,
            Err(_) => BTreeMap::new(),
        };

        let mut virtual_attrs = VirtualAttributions {
            repo,
            base_commit,
            attributions: HashMap::new(),
            file_contents: HashMap::new(),
            prompts,
            ts,
        };

        // Process all pathspecs concurrently
        if !pathspecs.is_empty() {
            virtual_attrs.add_pathspecs_concurrent(pathspecs).await?;
        }

        Ok(virtual_attrs)
    }

    /// Add a single pathspec to the virtual attributions
    pub async fn add_pathspec(&mut self, pathspec: &str) -> Result<(), GitAiError> {
        self.add_pathspecs_concurrent(&[pathspec.to_string()]).await
    }

    /// Add multiple pathspecs concurrently
    async fn add_pathspecs_concurrent(&mut self, pathspecs: &[String]) -> Result<(), GitAiError> {
        const MAX_CONCURRENT: usize = 30;

        let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));
        let mut tasks = Vec::new();

        for pathspec in pathspecs {
            let pathspec = pathspec.clone();
            let repo = self.repo.clone();
            let base_commit = self.base_commit.clone();
            let ts = self.ts;
            let semaphore = Arc::clone(&semaphore);

            let task = smol::spawn(async move {
                // Acquire semaphore permit to limit concurrency
                let _permit = semaphore.acquire().await;

                // Wrap blocking git operations in smol::unblock
                smol::unblock(move || {
                    compute_attributions_for_file(&repo, &base_commit, &pathspec, ts)
                })
                .await
            });

            tasks.push(task);
        }

        // Await all tasks
        let results = futures::future::join_all(tasks).await;

        // Process results and store in HashMap
        for result in results {
            match result {
                Ok(Some((file_path, content, char_attrs, line_attrs))) => {
                    self.attributions
                        .insert(file_path.clone(), (char_attrs, line_attrs));
                    self.file_contents.insert(file_path, content);
                }
                Ok(None) => {
                    // File had no changes or couldn't be processed, skip
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Get both character and line attributions for a file
    pub fn get_attributions(
        &self,
        file_path: &str,
    ) -> Option<&(Vec<Attribution>, Vec<LineAttribution>)> {
        self.attributions.get(file_path)
    }

    /// Get just character-level attributions for a file
    pub fn get_char_attributions(&self, file_path: &str) -> Option<&Vec<Attribution>> {
        self.attributions
            .get(file_path)
            .map(|(char_attrs, _)| char_attrs)
    }

    /// Get just line-level attributions for a file
    pub fn get_line_attributions(&self, file_path: &str) -> Option<&Vec<LineAttribution>> {
        self.attributions
            .get(file_path)
            .map(|(_, line_attrs)| line_attrs)
    }

    /// List all tracked files
    pub fn files(&self) -> Vec<String> {
        self.attributions.keys().cloned().collect()
    }

    /// Get the base commit SHA
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Get the timestamp used for attributions
    pub fn timestamp(&self) -> u128 {
        self.ts
    }

    /// Get the prompts metadata
    pub fn prompts(&self) -> &BTreeMap<String, PromptRecord> {
        &self.prompts
    }

    /// Get the file content for a tracked file
    pub fn get_file_content(&self, file_path: &str) -> Option<&String> {
        self.file_contents.get(file_path)
    }

    /// Get a reference to the repository
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Alias for new_for_base_commit for clarity
    pub async fn from_commit(
        repo: Repository,
        commit_sha: String,
        pathspecs: &[String],
    ) -> Result<Self, GitAiError> {
        Self::new_for_base_commit(repo, commit_sha, pathspecs).await
    }

    /// Create VirtualAttributions from current repository state (HEAD + working log)
    pub async fn from_repo_state(
        repo: Repository,
        pathspecs: &[String],
    ) -> Result<Self, GitAiError> {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;
        use crate::git::refs::get_reference_as_authorship_log_v3;

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Step 1: Get HEAD SHA
        let head_ref = repo.head()?;
        let head_sha = head_ref.target()?;

        // Step 2: Get HEAD's authorship log
        let mut authorship_log = match get_reference_as_authorship_log_v3(&repo, &head_sha) {
            Ok(log) => log,
            Err(_) => {
                let mut log = AuthorshipLog::new();
                log.metadata.base_commit_sha = head_sha.clone();
                log
            }
        };

        // Step 3: Get working log for HEAD and apply checkpoints
        let working_log = repo.storage.working_log_for_base_commit(&head_sha);
        if let Ok(checkpoints) = working_log.read_all_checkpoints() {
            let mut session_additions = std::collections::HashMap::new();
            let mut session_deletions = std::collections::HashMap::new();

            for checkpoint in &checkpoints {
                authorship_log.apply_checkpoint(
                    checkpoint,
                    Some(&CheckpointKind::Human.to_str()),
                    &mut session_additions,
                    &mut session_deletions,
                );
            }

            authorship_log.finalize(&session_additions, &session_deletions);
        }

        // Step 4: Build attributions from working directory files
        // Prompts are already in the authorship_log we built above
        let mut virtual_attrs = VirtualAttributions {
            repo: repo.clone(),
            base_commit: head_sha,
            attributions: HashMap::new(),
            file_contents: HashMap::new(),
            prompts: authorship_log.metadata.prompts.clone(),
            ts,
        };

        // Process each pathspec - read from working directory
        // For now, we'll run the computation sequentially since we need the authorship log state
        for file_path in pathspecs {
            if let Ok(workdir) = repo.workdir() {
                let abs_path = workdir.join(file_path);

                // Read working directory content
                let file_content = if abs_path.exists() {
                    std::fs::read_to_string(&abs_path).unwrap_or_default()
                } else {
                    String::new()
                };

                // Get attribution from the authorship log
                let mut line_attributions = Vec::new();

                // Debug: Check if file exists in authorship log
                let file_attestation = authorship_log
                    .attestations
                    .iter()
                    .find(|a| a.file_path == *file_path);

                if let Some(file_attestation) = file_attestation {
                    // Convert authorship log entries to line attributions
                    for entry in &file_attestation.entries {
                        for line_range in &entry.line_ranges {
                            let (start, end) = match line_range {
                                crate::authorship::authorship_log::LineRange::Single(line) => {
                                    (*line, *line)
                                }
                                crate::authorship::authorship_log::LineRange::Range(start, end) => {
                                    (*start, *end)
                                }
                            };

                            line_attributions.push(LineAttribution {
                                start_line: start,
                                end_line: end,
                                author_id: entry.hash.clone(),
                                overridden: false,
                            });
                        }
                    }
                }

                // Convert to character attributions
                let char_attributions =
                    line_attributions_to_attributions(&line_attributions, &file_content, ts);

                virtual_attrs
                    .attributions
                    .insert(file_path.clone(), (char_attributions, line_attributions));
                virtual_attrs
                    .file_contents
                    .insert(file_path.clone(), file_content);
            }
        }

        Ok(virtual_attrs)
    }

    /// Create VirtualAttributions from working log checkpoints for a specific base commit
    /// Returns both the VirtualAttributions and the AuthorshipLog with applied checkpoints
    pub async fn from_working_log_for_commit(
        repo: Repository,
        base_commit: String,
        pathspecs: &[String],
        human_author: Option<String>,
    ) -> Result<
        (
            Self,
            crate::authorship::authorship_log_serialization::AuthorshipLog,
        ),
        GitAiError,
    > {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;
        use crate::git::refs::get_reference_as_authorship_log_v3;

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        // Step 1: Get base commit's authorship log (or create empty)
        let mut authorship_log = match get_reference_as_authorship_log_v3(&repo, &base_commit) {
            Ok(log) => log,
            Err(_) => {
                let mut log = AuthorshipLog::new();
                log.metadata.base_commit_sha = base_commit.clone();
                log
            }
        };

        // Step 2: Get working log for base commit and apply checkpoints
        let working_log = repo.storage.working_log_for_base_commit(&base_commit);
        if let Ok(checkpoints) = working_log.read_all_checkpoints() {
            let mut session_additions = std::collections::HashMap::new();
            let mut session_deletions = std::collections::HashMap::new();

            for checkpoint in &checkpoints {
                authorship_log.apply_checkpoint(
                    checkpoint,
                    human_author.as_deref(),
                    &mut session_additions,
                    &mut session_deletions,
                );
            }

            authorship_log.finalize(&session_additions, &session_deletions);
        }

        // Step 3: Build attributions from working directory files
        // Prompts are already in the authorship_log we built above
        let mut virtual_attrs = VirtualAttributions {
            repo: repo.clone(),
            base_commit,
            attributions: HashMap::new(),
            file_contents: HashMap::new(),
            prompts: authorship_log.metadata.prompts.clone(),
            ts,
        };

        // Process each pathspec - read from working directory
        for file_path in pathspecs {
            if let Ok(workdir) = repo.workdir() {
                let abs_path = workdir.join(file_path);

                // Read working directory content
                let file_content = if abs_path.exists() {
                    std::fs::read_to_string(&abs_path).unwrap_or_default()
                } else {
                    String::new()
                };

                // Get attribution from the authorship log
                let mut line_attributions = Vec::new();

                // Debug: Check if file exists in authorship log
                let file_attestation = authorship_log
                    .attestations
                    .iter()
                    .find(|a| a.file_path == *file_path);

                if let Some(file_attestation) = file_attestation {
                    // Convert authorship log entries to line attributions
                    for entry in &file_attestation.entries {
                        for line_range in &entry.line_ranges {
                            let (start, end) = match line_range {
                                crate::authorship::authorship_log::LineRange::Single(line) => {
                                    (*line, *line)
                                }
                                crate::authorship::authorship_log::LineRange::Range(start, end) => {
                                    (*start, *end)
                                }
                            };

                            line_attributions.push(LineAttribution {
                                start_line: start,
                                end_line: end,
                                author_id: entry.hash.clone(),
                                overridden: false,
                            });
                        }
                    }
                }

                // Convert to character attributions
                let char_attributions =
                    line_attributions_to_attributions(&line_attributions, &file_content, ts);

                virtual_attrs
                    .attributions
                    .insert(file_path.clone(), (char_attributions, line_attributions));
                virtual_attrs
                    .file_contents
                    .insert(file_path.clone(), file_content);
            }
        }

        Ok((virtual_attrs, authorship_log))
    }

    /// Create VirtualAttributions from raw components (used for transformations)
    pub fn from_raw_data(
        repo: Repository,
        base_commit: String,
        attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        file_contents: HashMap<String, String>,
        ts: u128,
    ) -> Self {
        VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts: BTreeMap::new(),
            ts,
        }
    }
    
    pub fn from_raw_data_with_prompts(
        repo: Repository,
        base_commit: String,
        attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        file_contents: HashMap<String, String>,
        prompts: BTreeMap<String, PromptRecord>,
        ts: u128,
    ) -> Self {
        VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts,
            ts,
        }
    }

    /// Convert this VirtualAttributions to an AuthorshipLog
    pub fn to_authorship_log(
        &self,
    ) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;

        let mut authorship_log = AuthorshipLog::new();
        authorship_log.metadata.base_commit_sha = self.base_commit.clone();
        authorship_log.metadata.prompts = self.prompts.clone();

        // Process each file
        for (file_path, (_, line_attrs)) in &self.attributions {
            if line_attrs.is_empty() {
                continue;
            }

            // Group line attributions by author
            let mut author_lines: HashMap<String, Vec<u32>> = HashMap::new();
            for line_attr in line_attrs {
                for line in line_attr.start_line..=line_attr.end_line {
                    author_lines
                        .entry(line_attr.author_id.clone())
                        .or_default()
                        .push(line);
                }
            }

            // Create attestation entries for each author
            for (author_id, mut lines) in author_lines {
                lines.sort();
                lines.dedup();

                if lines.is_empty() {
                    continue;
                }

                // Create line ranges
                let mut ranges = Vec::new();
                let mut range_start = lines[0];
                let mut range_end = lines[0];

                for &line in &lines[1..] {
                    if line == range_end + 1 {
                        range_end = line;
                    } else {
                        if range_start == range_end {
                            ranges.push(crate::authorship::authorship_log::LineRange::Single(
                                range_start,
                            ));
                        } else {
                            ranges.push(crate::authorship::authorship_log::LineRange::Range(
                                range_start,
                                range_end,
                            ));
                        }
                        range_start = line;
                        range_end = line;
                    }
                }

                // Add the last range
                if range_start == range_end {
                    ranges.push(crate::authorship::authorship_log::LineRange::Single(
                        range_start,
                    ));
                } else {
                    ranges.push(crate::authorship::authorship_log::LineRange::Range(
                        range_start,
                        range_end,
                    ));
                }

                // Create attestation entry
                let entry = crate::authorship::authorship_log_serialization::AttestationEntry::new(
                    author_id, ranges,
                );

                // Add to authorship log
                let file_attestation = authorship_log.get_or_create_file(file_path);
                file_attestation.add_entry(entry);
            }
        }

        Ok(authorship_log)
    }

    /// Convert to initial working log state (stub for now)
    pub fn to_initial_working_log_state(
        &self,
    ) -> Result<Vec<crate::authorship::working_log::Checkpoint>, GitAiError> {
        // TODO: Implement checkpoint conversion
        // This will require rethinking how checkpoints work
        todo!("Checkpoint conversion not yet implemented")
    }
}

/// Merge two VirtualAttributions, favoring the primary for overlaps
pub fn merge_attributions_favoring_first(
    primary: VirtualAttributions,
    secondary: VirtualAttributions,
    final_state: HashMap<String, String>,
) -> Result<VirtualAttributions, GitAiError> {
    use crate::authorship::attribution_tracker::AttributionTracker;

    let tracker = AttributionTracker::new();
    let ts = primary.ts;
    let repo = primary.repo.clone();
    let base_commit = primary.base_commit.clone();

    // Merge prompts from both VAs
    let mut merged_prompts = primary.prompts.clone();
    for (key, value) in &secondary.prompts {
        merged_prompts.entry(key.clone()).or_insert_with(|| value.clone());
    }

    let mut merged = VirtualAttributions {
        repo,
        base_commit,
        attributions: HashMap::new(),
        file_contents: HashMap::new(),
        prompts: merged_prompts,
        ts,
    };

    // Get union of all files
    let mut all_files: std::collections::HashSet<String> =
        primary.attributions.keys().cloned().collect();
    all_files.extend(secondary.attributions.keys().cloned());
    all_files.extend(final_state.keys().cloned());

    for file_path in all_files {
        let final_content = match final_state.get(&file_path) {
            Some(content) => content,
            None => continue, // Skip files not in final state
        };

        // Get attributions from both sources
        let primary_attrs = primary.get_char_attributions(&file_path);
        let secondary_attrs = secondary.get_char_attributions(&file_path);

        // Get source content from both
        let primary_content = primary.get_file_content(&file_path);
        let secondary_content = secondary.get_file_content(&file_path);

        // Transform both to final state
        let transformed_primary =
            if let (Some(attrs), Some(content)) = (primary_attrs, primary_content) {
                transform_attributions_to_final(&tracker, content, attrs, final_content, ts)?
            } else {
                Vec::new()
            };

        let transformed_secondary =
            if let (Some(attrs), Some(content)) = (secondary_attrs, secondary_content) {
                transform_attributions_to_final(&tracker, content, attrs, final_content, ts)?
            } else {
                Vec::new()
            };

        // Merge: primary wins overlaps, secondary fills gaps
        let merged_char_attrs = merge_char_attributions(
            &transformed_primary,
            &transformed_secondary,
            final_content.len(),
        );

        // Convert to line attributions
        let merged_line_attrs =
            crate::authorship::attribution_tracker::attributions_to_line_attributions(
                &merged_char_attrs,
                final_content,
            );

        merged
            .attributions
            .insert(file_path.clone(), (merged_char_attrs, merged_line_attrs));
        merged
            .file_contents
            .insert(file_path, final_content.clone());
    }

    Ok(merged)
}

/// Transform attributions from old content to new content
fn transform_attributions_to_final(
    tracker: &crate::authorship::attribution_tracker::AttributionTracker,
    old_content: &str,
    old_attributions: &[Attribution],
    new_content: &str,
    ts: u128,
) -> Result<Vec<Attribution>, GitAiError> {
    // Use a dummy author for new insertions (we'll discard them anyway)
    let dummy_author = "__DUMMY__";

    let transformed = tracker.update_attributions(
        old_content,
        new_content,
        old_attributions,
        dummy_author,
        ts,
    )?;

    // Filter out dummy attributions (new insertions)
    let filtered: Vec<Attribution> = transformed
        .into_iter()
        .filter(|attr| attr.author_id != dummy_author)
        .collect();

    Ok(filtered)
}

/// Merge character-level attributions, with primary winning overlaps
fn merge_char_attributions(
    primary: &[Attribution],
    secondary: &[Attribution],
    content_len: usize,
) -> Vec<Attribution> {
    // Create coverage map for primary
    let mut covered = vec![false; content_len];
    for attr in primary {
        for i in attr.start..attr.end.min(content_len) {
            covered[i] = true;
        }
    }

    let mut result = Vec::new();

    // Add all primary attributions
    result.extend(primary.iter().cloned());

    // Add secondary attributions only where primary doesn't cover
    for attr in secondary {
        let mut uncovered_ranges = Vec::new();
        let mut range_start: Option<usize> = None;

        for i in attr.start..attr.end.min(content_len) {
            if !covered[i] {
                if range_start.is_none() {
                    range_start = Some(i);
                }
            } else {
                if let Some(start) = range_start {
                    uncovered_ranges.push((start, i));
                    range_start = None;
                }
            }
        }

        // Handle final range
        if let Some(start) = range_start {
            uncovered_ranges.push((start, attr.end.min(content_len)));
        }

        // Create attributions for uncovered ranges
        for (start, end) in uncovered_ranges {
            if start < end {
                result.push(Attribution::new(
                    start,
                    end,
                    attr.author_id.clone(),
                    attr.ts,
                ));
            }
        }
    }

    // Sort by start position
    result.sort_by_key(|a| (a.start, a.end));
    result
}

/// Compute attributions for a single file at a specific commit
fn compute_attributions_for_file(
    repo: &Repository,
    base_commit: &str,
    file_path: &str,
    ts: u128,
) -> Result<Option<(String, String, Vec<Attribution>, Vec<LineAttribution>)>, GitAiError> {
    // Set up blame options
    let mut ai_blame_opts = GitAiBlameOptions::default();
    ai_blame_opts.no_output = true;
    ai_blame_opts.return_human_authors_as_human = true;
    ai_blame_opts.use_prompt_hashes_as_names = true;
    ai_blame_opts.newest_commit = Some(base_commit.to_string());

    // Run blame at the base commit
    let ai_blame = repo.blame(file_path, &ai_blame_opts);

    match ai_blame {
        Ok((blames, _)) => {
            // Convert blame results to line attributions
            let mut line_attributions = Vec::new();
            for (line, author) in blames {
                // Skip human-only lines as they don't need tracking
                if author == CheckpointKind::Human.to_str() {
                    continue;
                }
                line_attributions.push(LineAttribution {
                    start_line: line,
                    end_line: line,
                    author_id: author.clone(),
                    overridden: false,
                });
            }

            // Get the file content at this commit to convert to character attributions
            // We need to read the file content that blame operated on
            let file_content = get_file_content_at_commit(repo, base_commit, file_path)?;

            // Convert line attributions to character attributions
            let char_attributions =
                line_attributions_to_attributions(&line_attributions, &file_content, ts);

            Ok(Some((
                file_path.to_string(),
                file_content,
                char_attributions,
                line_attributions,
            )))
        }
        Err(_) => {
            // File doesn't exist at this commit or can't be blamed, skip it
            Ok(None)
        }
    }
}

/// Get file content at a specific commit
fn get_file_content_at_commit(
    repo: &Repository,
    commit_sha: &str,
    file_path: &str,
) -> Result<String, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;

    match tree.get_path(std::path::Path::new(file_path)) {
        Ok(entry) => {
            if let Ok(blob) = repo.find_blob(entry.id()) {
                let blob_content = blob.content().unwrap_or_default();
                Ok(String::from_utf8_lossy(&blob_content).to_string())
            } else {
                Ok(String::new())
            }
        }
        Err(_) => Ok(String::new()),
    }
}

mod tests {

    use crate::git::find_repository_in_path;

    use super::*;

    #[test]
    fn test_virtual_attributions() {
        let repo = find_repository_in_path(".").unwrap();

        let virtual_attributions = smol::block_on(async {
            VirtualAttributions::new_for_base_commit(
                repo,
                "5753483e6a8d0024dacfc6eaab8b8f5b2f2301c5".to_string(),
                &["src/utils.rs".to_string()],
            )
            .await
        })
        .unwrap();

        println!(
            "virtual_attributions files: {:?}",
            virtual_attributions.files()
        );
        println!("base_commit: {}", virtual_attributions.base_commit());
        println!("timestamp: {}", virtual_attributions.timestamp());

        if let Some((char_attrs, line_attrs)) =
            virtual_attributions.get_attributions("src/utils.rs")
        {
            println!("\n=== src/utils.rs Attribution Info ===");
            println!("Character-level attributions: {} ranges", char_attrs.len());
            for (i, attr) in char_attrs.iter().enumerate() {
                println!(
                    "  [{}] chars {}..{} (len={}) -> author: '{}', ts: {}",
                    i,
                    attr.start,
                    attr.end,
                    attr.end - attr.start,
                    attr.author_id,
                    attr.ts
                );
            }

            println!("\nLine-level attributions: {} ranges", line_attrs.len());
            for (i, attr) in line_attrs.iter().enumerate() {
                println!(
                    "  [{}] lines {}..{} (count={}) -> author: '{}', overridden: {}",
                    i,
                    attr.start_line,
                    attr.end_line,
                    attr.line_count(),
                    attr.author_id,
                    attr.overridden
                );
            }
        }

        assert!(!virtual_attributions.files().is_empty());
    }
}
