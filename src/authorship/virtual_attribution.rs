use crate::authorship::attribution_tracker::{
    Attribution, LineAttribution, line_attributions_to_attributions,
};
use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::authorship::working_log::CheckpointKind;
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct VirtualAttributions {
    repo: Repository,
    base_commit: String,
    // Maps file path -> (char attributions, line attributions)
    pub attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
    // Maps file path -> file content
    file_contents: HashMap<String, String>,
    // Prompt records mapping prompt_id -> (commit_sha -> PromptRecord)
    // Same prompt can appear in multiple commits, allowing us to track and sort them
    pub prompts: BTreeMap<String, BTreeMap<String, PromptRecord>>,
    // Timestamp to use for attributions
    ts: u128,
    pub blame_start_commit: Option<String>,
}

impl VirtualAttributions {
    /// Create a new VirtualAttributions for the given base commit with initial pathspecs
    pub async fn new_for_base_commit(
        repo: Repository,
        base_commit: String,
        pathspecs: &[String],
        blame_start_commit: Option<String>,
    ) -> Result<Self, GitAiError> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let mut virtual_attrs = VirtualAttributions {
            repo,
            base_commit,
            attributions: HashMap::new(),
            file_contents: HashMap::new(),
            prompts: BTreeMap::new(),
            ts,
            blame_start_commit,
        };

        // Process all pathspecs concurrently
        if !pathspecs.is_empty() {
            virtual_attrs.add_pathspecs_concurrent(pathspecs).await?;
        }

        // After running blame, discover and load any missing prompts from blamed commits
        virtual_attrs.discover_and_load_foreign_prompts()?;

        Ok(virtual_attrs)
    }

    /// Discover and load prompts from blamed commits that aren't in our prompts map
    fn discover_and_load_foreign_prompts(&mut self) -> Result<(), GitAiError> {
        use std::collections::HashSet;

        // Collect all unique author_ids from attributions
        let mut all_author_ids: HashSet<String> = HashSet::new();
        for (_file_path, (char_attrs, _line_attrs)) in &self.attributions {
            for attr in char_attrs {
                all_author_ids.insert(attr.author_id.clone());
            }
        }

        // Find missing author_ids (not in prompts map)
        // An author_id is missing if it doesn't exist as a key in the outer prompts map
        let missing_ids: Vec<String> = all_author_ids
            .into_iter()
            .filter(|id| !self.prompts.contains_key(id))
            .collect();

        if missing_ids.is_empty() {
            return Ok(());
        }

        // Load prompts in parallel using the established MAX_CONCURRENT pattern
        let prompts = smol::block_on(async { self.load_prompts_concurrent(&missing_ids).await })?;

        // Insert loaded prompts into our map
        // Each prompt is associated with the commit it was found in
        for (id, commit_sha, prompt) in prompts {
            self.prompts
                .entry(id)
                .or_insert_with(BTreeMap::new)
                .insert(commit_sha, prompt);
        }

        Ok(())
    }

    /// Load multiple prompts concurrently using MAX_CONCURRENT limit
    async fn load_prompts_concurrent(
        &self,
        missing_ids: &[String],
    ) -> Result<Vec<(String, String, PromptRecord)>, GitAiError> {
        const MAX_CONCURRENT: usize = 30;

        let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));
        let mut tasks = Vec::new();

        for missing_id in missing_ids {
            let missing_id = missing_id.clone();
            let repo = self.repo.clone();
            let semaphore = Arc::clone(&semaphore);

            let task = smol::spawn(async move {
                // Acquire semaphore permit to limit concurrency
                let _permit = semaphore.acquire().await;

                // Wrap blocking git operations in smol::unblock
                smol::unblock(move || {
                    Self::find_prompt_in_history_static(&repo, &missing_id)
                        .map(|(commit_sha, prompt)| (missing_id.clone(), commit_sha, prompt))
                })
                .await
            });

            tasks.push(task);
        }

        // Await all tasks concurrently
        let results = futures::future::join_all(tasks).await;

        // Process results and collect successful prompts
        let mut prompts = Vec::new();
        for result in results {
            match result {
                Ok((id, commit_sha, prompt)) => prompts.push((id, commit_sha, prompt)),
                Err(_) => {
                    // Error finding prompt, skip it
                }
            }
        }

        Ok(prompts)
    }

    /// Static version of find_prompt_in_history for use in async context
    /// Returns (commit_sha, PromptRecord) for the most recent commit containing this prompt
    fn find_prompt_in_history_static(
        repo: &Repository,
        prompt_id: &str,
    ) -> Result<(String, crate::authorship::authorship_log::PromptRecord), GitAiError> {
        // Use git grep to search for the prompt ID in authorship notes
        let shas = crate::git::refs::grep_ai_notes(&repo, &format!("\"{}\"", prompt_id))
            .unwrap_or_default();

        // Check the most recent commit with this prompt ID
        if let Some(latest_sha) = shas.first() {
            if let Ok(log) = crate::git::refs::get_reference_as_authorship_log_v3(&repo, latest_sha)
            {
                if let Some(prompt) = log.metadata.prompts.get(prompt_id) {
                    return Ok((latest_sha.clone(), prompt.clone()));
                }
            }
        }

        Err(GitAiError::Generic(format!(
            "Prompt not found in history: {}",
            prompt_id
        )))
    }

    /// Add a single pathspec to the virtual attributions
    #[allow(dead_code)]
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
            let blame_start_commit = self.blame_start_commit.clone();
            let semaphore = Arc::clone(&semaphore);

            let task = smol::spawn(async move {
                // Acquire semaphore permit to limit concurrency
                let _permit = semaphore.acquire().await;

                // Wrap blocking git operations in smol::unblock
                smol::unblock(move || {
                    compute_attributions_for_file(
                        &repo,
                        &base_commit,
                        &pathspec,
                        ts,
                        blame_start_commit,
                    )
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
    #[allow(dead_code)]
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

    /// Get the prompts metadata (prompt_id -> commit_sha -> PromptRecord)
    pub fn prompts(&self) -> &BTreeMap<String, BTreeMap<String, PromptRecord>> {
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

    /// Create VirtualAttributions from current repository state (HEAD + working log)
    #[allow(dead_code)]
    pub async fn from_repo_state(
        repo: Repository,
        pathspecs: &[String],
    ) -> Result<Self, GitAiError> {
        // Step 1: Get HEAD SHA
        let head_ref = repo.head()?;
        let head_sha = head_ref.target()?;

        // Step 2: Use new_for_base_commit to establish authorship for all pathspecs
        // This will run blame to find all old authorship and discover prompts from history
        Self::new_for_base_commit(repo, head_sha, pathspecs, None).await
    }

    /// Create VirtualAttributions from just the working log (no blame)
    ///
    /// This is a fast path that skips the expensive blame operation.
    /// Use this when you only care about working log data and don't need historical blame.
    ///
    /// This function:
    /// 1. Loads INITIAL attributions (unstaged AI code from previous working state)
    /// 2. Applies working log checkpoints on top
    /// 3. Returns VirtualAttributions with just the working log data
    pub fn from_just_working_log(
        repo: Repository,
        base_commit: String,
        human_author: Option<String>,
    ) -> Result<Self, GitAiError> {
        let working_log = repo.storage.working_log_for_base_commit(&base_commit);
        let initial_attributions = working_log.read_initial_attributions();
        let checkpoints = working_log.read_all_checkpoints().unwrap_or_default();

        let mut attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)> =
            HashMap::new();
        let mut prompts = BTreeMap::new();
        let mut file_contents: HashMap<String, String> = HashMap::new();

        // Track additions and deletions per session_id for metrics
        let mut session_additions: HashMap<String, u32> = HashMap::new();
        let mut session_deletions: HashMap<String, u32> = HashMap::new();

        // Add prompts from INITIAL attributions
        // These are uncommitted prompts, so we use an empty string as the commit_sha
        for (prompt_id, prompt_record) in &initial_attributions.prompts {
            prompts
                .entry(prompt_id.clone())
                .or_insert_with(BTreeMap::new)
                .insert(String::new(), prompt_record.clone());
        }

        // Process INITIAL attributions
        for (file_path, line_attrs) in &initial_attributions.files {
            // Get the latest file content from working directory
            if let Ok(workdir) = repo.workdir() {
                let abs_path = workdir.join(file_path);
                let file_content = if abs_path.exists() {
                    std::fs::read_to_string(&abs_path).unwrap_or_default()
                } else {
                    String::new()
                };
                file_contents.insert(file_path.clone(), file_content.clone());

                // Convert line attributions to character attributions
                let char_attrs = line_attributions_to_attributions(&line_attrs, &file_content, 0);
                attributions.insert(file_path.clone(), (char_attrs, line_attrs.clone()));
            }
        }

        // Collect attributions from all checkpoints (later checkpoints override earlier ones)
        for checkpoint in &checkpoints {
            // Add prompts from checkpoint
            if let Some(agent_id) = &checkpoint.agent_id {
                let author_id =
                    crate::authorship::authorship_log_serialization::generate_short_hash(
                        &agent_id.id,
                        &agent_id.tool,
                    );
                // For working log checkpoints, use empty string as commit_sha since they're uncommitted
                prompts
                    .entry(author_id.clone())
                    .or_insert_with(BTreeMap::new)
                    .entry(String::new())
                    .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
                        agent_id: agent_id.clone(),
                        human_author: human_author.clone(),
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

                // Track additions and deletions from checkpoint line_stats
                *session_additions.entry(author_id.clone()).or_insert(0) +=
                    checkpoint.line_stats.additions;
                *session_deletions.entry(author_id.clone()).or_insert(0) +=
                    checkpoint.line_stats.deletions;
            }

            // Collect attributions from checkpoint entries
            for entry in &checkpoint.entries {
                // Get the latest file content from working directory
                if let Ok(workdir) = repo.workdir() {
                    let abs_path = workdir.join(&entry.file);
                    let file_content = if abs_path.exists() {
                        std::fs::read_to_string(&abs_path).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    file_contents.insert(entry.file.clone(), file_content);
                }

                // Use the line attributions from the checkpoint
                let line_attrs = entry.line_attributions.clone();
                let file_content = file_contents.get(&entry.file).cloned().unwrap_or_default();
                let char_attrs = line_attributions_to_attributions(&line_attrs, &file_content, 0);

                attributions.insert(entry.file.clone(), (char_attrs, line_attrs));
            }
        }

        // Calculate final metrics for each prompt
        Self::calculate_and_update_prompt_metrics(
            &mut prompts,
            &attributions,
            &session_additions,
            &session_deletions,
        );

        Ok(VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts,
            ts: 0,
            blame_start_commit: None,
        })
    }

    /// Create VirtualAttributions from working log checkpoints for a specific base commit
    ///
    /// This function:
    /// 1. Runs blame on the base commit to get ALL prompts from history (like new_for_base_commit)
    /// 2. Loads INITIAL attributions (unstaged AI code from previous working state)
    /// 3. Applies working log checkpoints on top
    /// 4. Returns VirtualAttributions with all attributions (both committed and uncommitted)
    pub async fn from_working_log_for_commit(
        repo: Repository,
        base_commit: String,
        pathspecs: &[String],
        human_author: Option<String>,
        blame_start_commit: Option<String>,
    ) -> Result<Self, GitAiError> {
        // Step 1: Build base VirtualAttributions using blame (gets ALL prompts from history)
        let blame_va = Self::new_for_base_commit(
            repo.clone(),
            base_commit.clone(),
            pathspecs,
            blame_start_commit,
        )
        .await?;

        // Step 2: Build VirtualAttributions from just working log
        let checkpoint_va =
            Self::from_just_working_log(repo.clone(), base_commit.clone(), human_author)?;

        // If checkpoint_va is empty, just return blame_va
        if checkpoint_va.attributions.is_empty() {
            return Ok(blame_va);
        }

        // Step 3: Merge blame and checkpoint attributions
        // Checkpoint attributions should override blame attributions for overlapping lines
        let final_state = checkpoint_va.file_contents.clone();
        let merged_va = merge_attributions_favoring_first(checkpoint_va, blame_va, final_state)?;

        Ok(merged_va)
    }

    /// Create VirtualAttributions from raw components (used for transformations)
    pub fn new(
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
            blame_start_commit: None,
        }
    }

    pub fn new_with_prompts(
        repo: Repository,
        base_commit: String,
        attributions: HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        file_contents: HashMap<String, String>,
        prompts: BTreeMap<String, BTreeMap<String, PromptRecord>>,
        ts: u128,
    ) -> Self {
        VirtualAttributions {
            repo,
            base_commit,
            attributions,
            file_contents,
            prompts,
            ts,
            blame_start_commit: None,
        }
    }

    /// Convert this VirtualAttributions to an AuthorshipLog
    pub fn to_authorship_log(
        &self,
    ) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;

        let mut authorship_log = AuthorshipLog::new();
        authorship_log.metadata.base_commit_sha = self.base_commit.clone();
        // Flatten the nested prompts map: take the most recent (first) prompt for each prompt_id
        authorship_log.metadata.prompts = self
            .prompts
            .iter()
            .filter_map(|(prompt_id, commits)| {
                // Get the first (most recent) commit's PromptRecord
                commits
                    .values()
                    .next()
                    .map(|record| (prompt_id.clone(), record.clone()))
            })
            .collect();

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
}

/// Helper function to collect committed line ranges from git diff
fn collect_committed_hunks(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<LineRange>>, GitAiError> {
    let mut committed_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();

    // Handle initial commit (no parent)
    if parent_sha == "initial" {
        // For initial commit, use git diff against the empty tree
        let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"; // Git's empty tree hash
        let added_lines = repo.diff_added_lines(empty_tree, commit_sha, pathspecs)?;

        for (file_path, lines) in added_lines {
            if !lines.is_empty() {
                committed_hunks.insert(file_path, LineRange::compress_lines(&lines));
            }
        }
        return Ok(committed_hunks);
    }

    // Use git diff to get added lines directly
    let added_lines = repo.diff_added_lines(parent_sha, commit_sha, pathspecs)?;

    for (file_path, lines) in added_lines {
        if !lines.is_empty() {
            committed_hunks.insert(file_path, LineRange::compress_lines(&lines));
        }
    }

    Ok(committed_hunks)
}

/// Helper function to collect unstaged line ranges (lines in working directory but not in commit)
/// Returns (unstaged_hunks, pure_insertion_hunks)
/// pure_insertion_hunks contains lines that were purely inserted (old_count=0), not modifications
fn collect_unstaged_hunks(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<
    (
        HashMap<String, Vec<LineRange>>,
        HashMap<String, Vec<LineRange>>,
    ),
    GitAiError,
> {
    let mut unstaged_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();
    let mut pure_insertion_hunks: HashMap<String, Vec<LineRange>> = HashMap::new();

    // Use git diff to get added lines in working directory vs commit, with insertion tracking
    let (added_lines, insertion_lines) =
        repo.diff_workdir_added_lines_with_insertions(commit_sha, pathspecs)?;

    for (file_path, lines) in added_lines {
        if !lines.is_empty() {
            unstaged_hunks.insert(file_path, LineRange::compress_lines(&lines));
        }
    }

    for (file_path, lines) in insertion_lines {
        if !lines.is_empty() {
            pure_insertion_hunks.insert(file_path, LineRange::compress_lines(&lines));
        }
    }

    // Check for untracked files in pathspecs that git diff didn't find
    // These are files that exist in the working directory but aren't tracked by git
    if let Some(paths) = pathspecs {
        if let Ok(workdir) = repo.workdir() {
            for pathspec in paths {
                // Skip if we already found this file in git diff
                if unstaged_hunks.contains_key(pathspec) {
                    continue;
                }

                // Check if file exists in the commit - if it does, it's tracked and git diff should handle it
                // Only process truly untracked files (files that don't exist in the commit tree)
                if file_exists_in_commit(repo, commit_sha, pathspec).unwrap_or(false) {
                    continue;
                }

                // Check if file exists in working directory
                let file_path = workdir.join(pathspec);
                if file_path.exists() && file_path.is_file() {
                    // Try to read the file
                    if let Ok(content) = std::fs::read_to_string(&file_path) {
                        // Count the lines - all lines are "unstaged" since the file is untracked
                        let line_count = content.lines().count() as u32;
                        if line_count > 0 {
                            // Create a range covering all lines (1-indexed)
                            let range = vec![LineRange::Range(1, line_count)];
                            unstaged_hunks.insert(pathspec.clone(), range.clone());
                            // Untracked files are pure insertions (the entire file is new)
                            pure_insertion_hunks.insert(pathspec.clone(), range);
                        }
                    }
                }
            }
        }
    }

    Ok((unstaged_hunks, pure_insertion_hunks))
}

impl VirtualAttributions {
    /// Split VirtualAttributions into committed and uncommitted buckets
    ///
    /// This method uses git diff to determine which line attributions belong in:
    /// - Bucket 1 (committed): Lines added in this commit → AuthorshipLog
    /// - Bucket 2 (uncommitted): Lines NOT added in this commit → InitialAttributions
    pub fn to_authorship_log_and_initial_working_log(
        &self,
        repo: &Repository,
        parent_sha: &str,
        commit_sha: &str,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<
        (
            crate::authorship::authorship_log_serialization::AuthorshipLog,
            crate::git::repo_storage::InitialAttributions,
        ),
        GitAiError,
    > {
        use crate::authorship::authorship_log_serialization::AuthorshipLog;
        use crate::git::repo_storage::InitialAttributions;
        use std::collections::{HashMap as StdHashMap, HashSet};

        let mut authorship_log = AuthorshipLog::new();
        authorship_log.metadata.base_commit_sha = self.base_commit.clone();
        // Flatten the nested prompts map: take the most recent (first) prompt for each prompt_id
        authorship_log.metadata.prompts = self
            .prompts
            .iter()
            .filter_map(|(prompt_id, commits)| {
                // Get the first (most recent) commit's PromptRecord
                commits
                    .values()
                    .next()
                    .map(|record| (prompt_id.clone(), record.clone()))
            })
            .collect();

        let mut initial_files: StdHashMap<String, Vec<LineAttribution>> = StdHashMap::new();
        let mut referenced_prompts: HashSet<String> = HashSet::new();

        // Get committed hunks (in commit coordinates) and unstaged hunks (in working directory coordinates)
        let committed_hunks = collect_committed_hunks(repo, parent_sha, commit_sha, pathspecs)?;
        let (mut unstaged_hunks, pure_insertion_hunks) =
            collect_unstaged_hunks(repo, commit_sha, pathspecs)?;

        // IMPORTANT: If a line appears in both committed_hunks and unstaged_hunks, it means:
        // - The line was committed in this commit (in commit coordinates)
        // - The line was then modified again in the working directory (in workdir coordinates)
        // Since both use the same line numbering after the commit (workdir coordinates = commit coordinates
        // for the committed state), we can directly compare line numbers.
        // We should treat these lines as committed, not unstaged, because the attribution belongs
        // to the commit even if there's a subsequent unstaged modification.
        //
        // HOWEVER: If a line is a PURE INSERTION (old_count=0), it means a new line was inserted
        // at that position, pushing existing lines down. In this case, the line number overlap
        // doesn't mean the same line - it's a different line at the same position!
        // We should NOT filter out pure insertions even if they overlap with committed line numbers.
        for (file_path, committed_ranges) in &committed_hunks {
            if let Some(unstaged_ranges) = unstaged_hunks.get_mut(file_path) {
                // Expand both to line numbers for comparison
                let committed_lines: std::collections::HashSet<u32> =
                    committed_ranges.iter().flat_map(|r| r.expand()).collect();

                // Get pure insertion lines for this file (these should NOT be filtered out)
                let pure_insertion_lines: std::collections::HashSet<u32> = pure_insertion_hunks
                    .get(file_path)
                    .map(|ranges| ranges.iter().flat_map(|r| r.expand()).collect())
                    .unwrap_or_default();

                // Filter out any unstaged lines that were also committed
                // (these are lines that were committed, then modified again in workdir)
                // BUT keep pure insertions even if they overlap with committed line numbers
                let mut filtered_unstaged_lines: Vec<u32> = unstaged_ranges
                    .iter()
                    .flat_map(|r| r.expand())
                    .filter(|line| {
                        // Keep the line if it's NOT in committed, OR if it's a pure insertion
                        !committed_lines.contains(line) || pure_insertion_lines.contains(line)
                    })
                    .collect();

                if filtered_unstaged_lines.is_empty() {
                    unstaged_ranges.clear();
                } else {
                    filtered_unstaged_lines.sort_unstable();
                    filtered_unstaged_lines.dedup();
                    *unstaged_ranges = LineRange::compress_lines(&filtered_unstaged_lines);
                }
            }
        }

        // Remove files with no unstaged hunks
        unstaged_hunks.retain(|_, ranges| !ranges.is_empty());

        // Process each file
        for (file_path, (_, line_attrs)) in &self.attributions {
            if line_attrs.is_empty() {
                continue;
            }

            // Get unstaged lines for this file (in working directory coordinates)
            let mut unstaged_lines: Vec<u32> = Vec::new();
            if let Some(unstaged_ranges) = unstaged_hunks.get(file_path) {
                for range in unstaged_ranges {
                    unstaged_lines.extend(range.expand());
                }
                unstaged_lines.sort_unstable();
            }

            // Split line attributions into committed and uncommitted
            // VirtualAttributions has line numbers in working directory coordinates,
            // so we need to convert to commit coordinates before comparing with committed hunks
            let mut committed_lines_map: StdHashMap<String, Vec<u32>> = StdHashMap::new();
            let mut uncommitted_lines_map: StdHashMap<String, Vec<u32>> = StdHashMap::new();

            // Get the committed hunks for this file (if any) - these are in commit coordinates
            let file_committed_hunks = committed_hunks.get(file_path);

            for line_attr in line_attrs {
                // Check each line individually
                for workdir_line_num in line_attr.start_line..=line_attr.end_line {
                    // Check if this line is unstaged (in working directory but not in commit)
                    let is_unstaged = unstaged_lines.binary_search(&workdir_line_num).is_ok();

                    if is_unstaged {
                        // Line is unstaged, mark as uncommitted
                        uncommitted_lines_map
                            .entry(line_attr.author_id.clone())
                            .or_default()
                            .push(workdir_line_num);
                        referenced_prompts.insert(line_attr.author_id.clone());
                    } else {
                        // Convert working directory line number to commit line number
                        // by subtracting the count of unstaged lines before this line
                        let adjustment = unstaged_lines
                            .iter()
                            .filter(|&&l| l < workdir_line_num)
                            .count() as u32;
                        let commit_line_num = workdir_line_num - adjustment;

                        // Check if this commit line number is in any committed hunk
                        let is_committed = if let Some(hunks) = file_committed_hunks {
                            hunks.iter().any(|hunk| hunk.contains(commit_line_num))
                        } else {
                            false
                        };

                        if is_committed {
                            // Line was committed in this commit (use commit coordinates)
                            committed_lines_map
                                .entry(line_attr.author_id.clone())
                                .or_default()
                                .push(commit_line_num);
                        } else {
                        }
                        // Note: Lines that are neither unstaged nor in committed_hunks are lines that
                        // already existed in the parent commit. They are discarded (not added to uncommitted).
                    }
                }
            }

            // Add committed attributions to authorship log
            if !committed_lines_map.is_empty() {
                // Create attestation entries from committed lines
                for (author_id, mut lines) in committed_lines_map {
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

                    let entry =
                        crate::authorship::authorship_log_serialization::AttestationEntry::new(
                            author_id, ranges,
                        );

                    let file_attestation = authorship_log.get_or_create_file(file_path);
                    file_attestation.add_entry(entry);
                }
            }

            // Add uncommitted attributions to INITIAL
            if !uncommitted_lines_map.is_empty() {
                // Convert the map into line attributions
                let mut uncommitted_line_attrs = Vec::new();
                for (author_id, mut lines) in uncommitted_lines_map {
                    lines.sort();
                    lines.dedup();

                    if lines.is_empty() {
                        continue;
                    }

                    // Create ranges from individual lines
                    let mut range_start = lines[0];
                    let mut range_end = lines[0];

                    for &line in &lines[1..] {
                        if line == range_end + 1 {
                            range_end = line;
                        } else {
                            // End current range and start new one
                            uncommitted_line_attrs.push(LineAttribution {
                                start_line: range_start,
                                end_line: range_end,
                                author_id: author_id.clone(),
                                overrode: None,
                            });
                            range_start = line;
                            range_end = line;
                        }
                    }

                    // Add the last range
                    uncommitted_line_attrs.push(LineAttribution {
                        start_line: range_start,
                        end_line: range_end,
                        author_id: author_id.clone(),
                        overrode: None,
                    });
                }

                initial_files.insert(file_path.clone(), uncommitted_line_attrs);
            }
        }

        // Build prompts map for INITIAL (only prompts referenced by uncommitted lines)
        let mut initial_prompts = StdHashMap::new();
        for prompt_id in referenced_prompts {
            if let Some(commits) = self.prompts.get(&prompt_id) {
                // Get the most recent (first) prompt for this prompt_id
                if let Some(prompt) = commits.values().next() {
                    initial_prompts.insert(prompt_id, prompt.clone());
                }
            }
        }

        let initial_attributions = InitialAttributions {
            files: initial_files,
            prompts: initial_prompts,
        };

        Ok((authorship_log, initial_attributions))
    }

    /// Merge prompts from multiple sources, picking the newest PromptRecord for each prompt_id
    ///
    /// This function collects all PromptRecords for each unique prompt_id across all sources,
    /// sorts them by age (oldest to newest), and returns the newest version of each prompt.
    pub fn merge_prompts_picking_newest(
        prompt_sources: &[&BTreeMap<String, BTreeMap<String, PromptRecord>>],
    ) -> BTreeMap<String, BTreeMap<String, PromptRecord>> {
        let mut merged_prompts = BTreeMap::new();

        // Collect all unique prompt_ids across all sources
        let mut all_prompt_ids: HashSet<String> = HashSet::new();
        for source in prompt_sources {
            all_prompt_ids.extend(source.keys().cloned());
        }

        for prompt_id in all_prompt_ids {
            // Collect all PromptRecords for this prompt_id from all sources
            let mut all_records = Vec::new();

            for source in prompt_sources {
                if let Some(commits) = source.get(&prompt_id) {
                    for (_commit_sha, prompt_record) in commits {
                        all_records.push(prompt_record.clone());
                    }
                }
            }

            // Sort records oldest to newest using the Ord implementation
            all_records.sort();

            // Take the last (newest) record
            if let Some(newest_record) = all_records.last() {
                let mut prompt_commits = BTreeMap::new();

                // Use commit sha from first source that has this prompt, or "merged" if not found
                let commit_sha = prompt_sources
                    .iter()
                    .find_map(|source| {
                        source
                            .get(&prompt_id)
                            .and_then(|commits| commits.keys().last().cloned())
                    })
                    .unwrap_or_else(|| "merged".to_string());

                prompt_commits.insert(commit_sha, newest_record.clone());
                merged_prompts.insert(prompt_id.clone(), prompt_commits);
            }
        }

        merged_prompts
    }

    /// Calculate and update prompt metrics (accepted_lines, overridden_lines, total_additions, total_deletions)
    pub fn calculate_and_update_prompt_metrics(
        prompts: &mut BTreeMap<String, BTreeMap<String, PromptRecord>>,
        attributions: &HashMap<String, (Vec<Attribution>, Vec<LineAttribution>)>,
        session_additions: &HashMap<String, u32>,
        session_deletions: &HashMap<String, u32>,
    ) {
        use std::collections::HashSet;

        // Collect all line attributions
        let all_line_attributions: Vec<&LineAttribution> = attributions
            .values()
            .flat_map(|(_, line_attrs)| line_attrs.iter())
            .collect();

        // Calculate accepted_lines: count lines in final attributions per session
        let mut session_accepted_lines: HashMap<String, u32> = HashMap::new();
        for (_file_path, (_char_attrs, line_attrs)) in attributions {
            for line_attr in line_attrs {
                let line_count = line_attr.end_line - line_attr.start_line + 1;
                *session_accepted_lines
                    .entry(line_attr.author_id.clone())
                    .or_insert(0) += line_count;
            }
        }

        // Calculate overridden_lines: count lines where overrode field matches session_id
        let mut session_overridden_lines: HashMap<String, u32> = HashMap::new();
        for line_attr in &all_line_attributions {
            if let Some(overrode_id) = &line_attr.overrode {
                let mut overridden_lines: HashSet<u32> = HashSet::new();
                for line in line_attr.start_line..=line_attr.end_line {
                    overridden_lines.insert(line);
                }
                *session_overridden_lines
                    .entry(overrode_id.clone())
                    .or_insert(0) += overridden_lines.len() as u32;
            }
        }

        // Update all prompt records with calculated metrics
        for (session_id, commits) in prompts.iter_mut() {
            for prompt_record in commits.values_mut() {
                prompt_record.total_additions = *session_additions.get(session_id).unwrap_or(&0);
                prompt_record.total_deletions = *session_deletions.get(session_id).unwrap_or(&0);
                prompt_record.accepted_lines =
                    *session_accepted_lines.get(session_id).unwrap_or(&0);
                prompt_record.overriden_lines =
                    *session_overridden_lines.get(session_id).unwrap_or(&0);
            }
        }
    }

    /// Filter prompts and attributions to only include those from specific commits
    /// This is useful for range analysis where we only want to count AI contributions
    /// from commits within the range, not from before
    pub fn filter_to_commits(&mut self, commit_shas: &HashSet<String>) {
        // Capture original AI prompt IDs before filtering
        let original_prompt_ids: HashSet<String> = self.prompts.keys().cloned().collect();

        // Filter prompts to only include those from the specified commits
        let mut filtered_prompts = BTreeMap::new();

        for (prompt_id, commits_map) in &self.prompts {
            let filtered_commits: BTreeMap<String, PromptRecord> = commits_map
                .iter()
                .filter(|(commit_sha, _)| commit_shas.contains(*commit_sha))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            if !filtered_commits.is_empty() {
                filtered_prompts.insert(prompt_id.clone(), filtered_commits);
            }
        }

        self.prompts = filtered_prompts;

        // Get set of valid prompt IDs after filtering
        let valid_prompt_ids: HashSet<String> = self.prompts.keys().cloned().collect();

        // Remove attributions that reference filtered-out prompts
        for (_file_path, (char_attrs, _line_attrs)) in self.attributions.iter_mut() {
            char_attrs.retain(|attr| {
                // Keep human attributions (not in original prompts at all)
                // OR keep AI attributions that are still valid after filtering
                !original_prompt_ids.contains(&attr.author_id)
                    || valid_prompt_ids.contains(&attr.author_id)
            });
        }

        // Recalculate line attributions for all files
        for (file_path, (char_attrs, line_attrs)) in self.attributions.iter_mut() {
            let file_content = self
                .file_contents
                .get(file_path)
                .cloned()
                .unwrap_or_default();
            *line_attrs = crate::authorship::attribution_tracker::attributions_to_line_attributions(
                char_attrs,
                &file_content,
            );
        }
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

    // Merge prompts from both VAs, picking the newest version of each prompt
    let merged_prompts =
        VirtualAttributions::merge_prompts_picking_newest(&[&primary.prompts, &secondary.prompts]);

    let mut merged = VirtualAttributions {
        repo,
        base_commit,
        attributions: HashMap::new(),
        file_contents: HashMap::new(),
        prompts: merged_prompts,
        ts,
        blame_start_commit: None,
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

    // Save total_additions and total_deletions from the newest PromptRecord
    let mut saved_totals: HashMap<String, (u32, u32)> = HashMap::new();
    for (prompt_id, commits) in &merged.prompts {
        for prompt_record in commits.values() {
            saved_totals.insert(
                prompt_id.clone(),
                (prompt_record.total_additions, prompt_record.total_deletions),
            );
        }
    }

    // Calculate and update prompt metrics (will set accepted_lines and overridden_lines)
    VirtualAttributions::calculate_and_update_prompt_metrics(
        &mut merged.prompts,
        &merged.attributions,
        &HashMap::new(), // Empty - will result in total_additions = 0
        &HashMap::new(), // Empty - will result in total_deletions = 0
    );

    // Restore the saved total_additions and total_deletions
    for (prompt_id, commits) in merged.prompts.iter_mut() {
        if let Some(&(additions, deletions)) = saved_totals.get(prompt_id) {
            for prompt_record in commits.values_mut() {
                prompt_record.total_additions = additions;
                prompt_record.total_deletions = deletions;
            }
        }
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
    blame_start_commit: Option<String>,
) -> Result<Option<(String, String, Vec<Attribution>, Vec<LineAttribution>)>, GitAiError> {
    // Set up blame options
    let mut ai_blame_opts = GitAiBlameOptions::default();
    ai_blame_opts.no_output = true;
    ai_blame_opts.return_human_authors_as_human = true;
    ai_blame_opts.use_prompt_hashes_as_names = true;
    ai_blame_opts.newest_commit = Some(base_commit.to_string());
    ai_blame_opts.oldest_commit = blame_start_commit.clone();

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
                    overrode: None,
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

/// Check if a file exists in a commit's tree
fn file_exists_in_commit(
    repo: &Repository,
    commit_sha: &str,
    file_path: &str,
) -> Result<bool, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;
    Ok(tree.get_path(std::path::Path::new(file_path)).is_ok())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_virtual_attributions() {
        let repo = crate::git::find_repository_in_path(".").unwrap();

        let virtual_attributions = smol::block_on(async {
            VirtualAttributions::new_for_base_commit(
                repo,
                "5753483e6a8d0024dacfc6eaab8b8f5b2f2301c5".to_string(),
                &["src/utils.rs".to_string()],
                None,
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
                    "  [{}] lines {}..{} (count={}) -> author: '{}', overrode: {}",
                    i,
                    attr.start_line,
                    attr.end_line,
                    attr.line_count(),
                    attr.author_id,
                    format!("{:?}", attr.overrode)
                );
            }
        }

        assert!(!virtual_attributions.files().is_empty());
    }
}
