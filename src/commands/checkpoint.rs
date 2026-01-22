use crate::authorship::attribution_tracker::{
    Attribution, AttributionTracker, INITIAL_ATTRIBUTION_TS, LineAttribution,
};
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::imara_diff_utils::{LineChangeTag, compute_line_changes};
use crate::authorship::working_log::CheckpointKind;
use crate::authorship::working_log::{Checkpoint, WorkingLogEntry};
use crate::commands::blame::{GitAiBlameOptions, OLDEST_AI_BLAME_DATE};
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::repo_storage::{PersistedWorkingLog, RepoStorage};
use crate::git::repository::Repository;
use crate::git::status::{EntryKind, StatusCode};
use crate::utils::{debug_log, normalize_to_posix};
use futures::stream::{self, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Per-file line statistics (in-memory only, not persisted)
#[derive(Debug, Clone, Default)]
struct FileLineStats {
    additions: u32,
    deletions: u32,
    additions_sloc: u32,
    deletions_sloc: u32,
}

pub fn run(
    repo: &Repository,
    author: &str,
    kind: CheckpointKind,
    show_working_log: bool,
    reset: bool,
    quiet: bool,
    agent_run_result: Option<AgentRunResult>,
    is_pre_commit: bool,
) -> Result<(usize, usize, usize), GitAiError> {
    let checkpoint_start = Instant::now();
    debug_log(&format!("[BENCHMARK] Starting checkpoint run"));

    // Robustly handle zero-commit repos
    let base_commit = match repo.head() {
        Ok(head) => match head.target() {
            Ok(oid) => oid,
            Err(_) => "initial".to_string(),
        },
        Err(_) => "initial".to_string(),
    };

    // Cannot run checkpoint on bare repositories
    if repo.workdir().is_err() {
        eprintln!("Cannot run checkpoint on bare repositories");
        return Err(GitAiError::Generic(
            "Cannot run checkpoint on bare repositories".to_string(),
        ));
    }

    // Initialize the new storage system
    let storage_start = Instant::now();
    let repo_storage = RepoStorage::for_repo_path(repo.path(), &repo.workdir()?);
    let mut working_log = repo_storage.working_log_for_base_commit(&base_commit);
    debug_log(&format!(
        "[BENCHMARK] Storage initialization took {:?}",
        storage_start.elapsed()
    ));

    // Early exit for human only
    if is_pre_commit {
        let has_no_ai_edits = working_log
            .all_ai_touched_files()
            .map(|files| files.is_empty())
            .unwrap_or(true);

        // Also check for INITIAL attributions - these are AI attributions from previous
        // commits that weren't staged (e.g., after an amend). We must process these.
        let has_initial_attributions = !working_log.read_initial_attributions().files.is_empty();

        // we can only skip the work here if inter_commit_move is not enabled.
        // otherwise we might miss an AI attribution that was moved by a user ie: copy / pasting
        if has_no_ai_edits
            && !has_initial_attributions
            && !Config::get().get_feature_flags().inter_commit_move
        {
            debug_log("No AI edits,in pre-commit checkpoint, skipping");
            return Ok((0, 0, 0));
        }
    }

    // Set dirty files if available
    if let Some(dirty_files) = agent_run_result
        .as_ref()
        .and_then(|result| result.dirty_files.clone())
    {
        working_log.set_dirty_files(Some(dirty_files));
    }

    // Get the current timestamp in milliseconds since the Unix epoch
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    // Extract edited filepaths from agent_run_result if available
    // For human checkpoints, use will_edit_filepaths to narrow git status scope
    // For AI checkpoints, use edited_filepaths
    // Filter out paths outside the repository to prevent git call crashes
    let pathspec_start = Instant::now();
    let mut filtered_pathspec: Option<Vec<String>> = None;
    let pathspec_filter = agent_run_result.as_ref().and_then(|result| {
        let paths = if result.checkpoint_kind == CheckpointKind::Human {
            result.will_edit_filepaths.as_ref()
        } else {
            result.edited_filepaths.as_ref()
        };

        paths.and_then(|p| {
            let repo_workdir = repo.workdir().ok()?;

            let filtered: Vec<String> = p
                .iter()
                .filter_map(|path| {
                    let path_buf = if std::path::Path::new(path).is_absolute() {
                        // Absolute path - check directly
                        std::path::PathBuf::from(path)
                    } else {
                        // Relative path - join with workdir
                        repo_workdir.join(path)
                    };

                    // Use centralized path comparison (handles Windows canonical paths correctly)
                    if repo.path_is_in_workdir(&path_buf) {
                        // Convert to relative path for git operations
                        if std::path::Path::new(path).is_absolute() {
                            if let Ok(relative) = path_buf.strip_prefix(&repo_workdir) {
                                // Normalize path separators to forward slashes for git
                                Some(normalize_to_posix(&relative.to_string_lossy()))
                            } else {
                                // Fallback: try with canonical paths
                                let canonical_workdir = repo_workdir.canonicalize().ok()?;
                                let canonical_path = path_buf.canonicalize().ok()?;
                                if let Ok(relative) =
                                    canonical_path.strip_prefix(&canonical_workdir)
                                {
                                    // Normalize path separators to forward slashes for git
                                    Some(normalize_to_posix(&relative.to_string_lossy()))
                                } else {
                                    None
                                }
                            }
                        } else {
                            // Normalize path separators to forward slashes for git
                            Some(normalize_to_posix(path))
                        }
                    } else {
                        None
                    }
                })
                .collect();

            if filtered.is_empty() {
                None
            } else {
                filtered_pathspec = Some(filtered);
                filtered_pathspec.as_ref()
            }
        })
    });
    debug_log(&format!(
        "[BENCHMARK] Pathspec filtering took {:?}",
        pathspec_start.elapsed()
    ));

    let files_start = Instant::now();
    let files = get_all_tracked_files(
        repo,
        &base_commit,
        &working_log,
        pathspec_filter,
        is_pre_commit,
    )?;
    debug_log(&format!(
        "[BENCHMARK] get_all_tracked_files found {} files, took {:?}",
        files.len(),
        files_start.elapsed()
    ));

    let read_checkpoints_start = Instant::now();
    let mut checkpoints = if reset {
        // If reset flag is set, start with an empty working log
        working_log.reset_working_log()?;
        Vec::new()
    } else {
        working_log.read_all_checkpoints()?
    };
    debug_log(&format!(
        "[BENCHMARK] Reading {} checkpoints took {:?}",
        checkpoints.len(),
        read_checkpoints_start.elapsed()
    ));

    if show_working_log {
        if checkpoints.is_empty() {
            debug_log("No working log entries found.");
        } else {
            debug_log("Working Log Entries:");
            debug_log(&format!("{}", "=".repeat(80)));
            for (i, checkpoint) in checkpoints.iter().enumerate() {
                debug_log(&format!("Checkpoint {}", i + 1));
                debug_log(&format!("  Diff: {}", checkpoint.diff));
                debug_log(&format!("  Author: {}", checkpoint.author));
                debug_log(&format!(
                    "  Agent ID: {}",
                    checkpoint
                        .agent_id
                        .as_ref()
                        .map(|id| id.tool.clone())
                        .unwrap_or_default()
                ));

                // Display first user message from transcript if available
                if let Some(transcript) = &checkpoint.transcript {
                    if let Some(first_message) = transcript.messages().first() {
                        if let crate::authorship::transcript::Message::User { text, .. } =
                            first_message
                        {
                            let agent_info = checkpoint
                                .agent_id
                                .as_ref()
                                .map(|id| format!(" (Agent: {})", id.tool))
                                .unwrap_or_default();
                            let message_count = transcript.messages().len();
                            debug_log(&format!(
                                "  First message{} ({} messages): {}",
                                agent_info, message_count, text
                            ));
                        }
                    }
                }

                debug_log("  Entries:");
                for entry in &checkpoint.entries {
                    debug_log(&format!("    File: {}", entry.file));
                    debug_log(&format!("    Blob SHA: {}", entry.blob_sha));
                    debug_log(&format!(
                        "    Line Attributions: {:?}",
                        entry.line_attributions
                    ));
                    debug_log(&format!("    Attributions: {:?}", entry.attributions));
                }
                debug_log("");
            }
        }
        return Ok((0, files.len(), checkpoints.len()));
    }

    // Save current file states and get content hashes
    let save_states_start = Instant::now();
    let file_content_hashes = save_current_file_states(&working_log, &files)?;
    debug_log(&format!(
        "[BENCHMARK] save_current_file_states for {} files took {:?}",
        files.len(),
        save_states_start.elapsed()
    ));

    // Order file hashes by key and create a hash of the ordered hashes
    let hash_compute_start = Instant::now();
    let mut ordered_hashes: Vec<_> = file_content_hashes.iter().collect();
    ordered_hashes.sort_by_key(|(file_path, _)| *file_path);

    let mut combined_hasher = Sha256::new();
    for (file_path, hash) in ordered_hashes {
        combined_hasher.update(file_path.as_bytes());
        combined_hasher.update(hash.as_bytes());
    }
    let combined_hash = format!("{:x}", combined_hasher.finalize());
    debug_log(&format!(
        "[BENCHMARK] Hash computation took {:?}",
        hash_compute_start.elapsed()
    ));

    // Note: foreign prompts from INITIAL file are read in post_commit.rs
    // when converting working log -> authorship log

    // Get checkpoint entries using unified function that handles both initial and subsequent checkpoints
    let entries_start = Instant::now();
    let (entries, file_stats) = smol::block_on(get_checkpoint_entries(
        kind,
        repo,
        &working_log,
        &files,
        &file_content_hashes,
        &checkpoints,
        agent_run_result.as_ref(),
        ts,
    ))?;
    debug_log(&format!(
        "[BENCHMARK] get_checkpoint_entries generated {} entries, took {:?}",
        entries.len(),
        entries_start.elapsed()
    ));

    // Skip adding checkpoint if there are no changes
    if !entries.is_empty() {
        let checkpoint_create_start = Instant::now();
        let mut checkpoint = Checkpoint::new(
            kind.clone(),
            combined_hash.clone(),
            author.to_string(),
            entries.clone(),
        );

        // Aggregate line stats from in-memory stats (computed during entry creation)
        checkpoint.line_stats = compute_line_stats(&file_stats)?;

        // Set transcript and agent_id if provided and not a human checkpoint
        if kind != CheckpointKind::Human
            && let Some(agent_run) = &agent_run_result
        {
            checkpoint.transcript = Some(agent_run.transcript.clone().unwrap_or_default());
            checkpoint.agent_id = Some(agent_run.agent_id.clone());
            checkpoint.agent_metadata = agent_run.agent_metadata.clone();
        }
        debug_log(&format!(
            "[BENCHMARK] Checkpoint creation took {:?}",
            checkpoint_create_start.elapsed()
        ));

        // Upsert prompt to database (non-fatal if it fails)
        if kind != CheckpointKind::Human && checkpoint.agent_id.is_some() {
            if checkpoint.transcript.is_some() {
                if let Err(e) = upsert_checkpoint_prompt_to_db(
                    &checkpoint,
                    working_log.repo_workdir.to_string_lossy().to_string(),
                    None, // commit_sha is None at checkpoint stage
                ) {
                    debug_log(&format!(
                        "[Warning] Failed to upsert prompt to database: {}",
                        e
                    ));
                    crate::observability::log_error(
                        &e,
                        Some(serde_json::json!({
                            "operation": "checkpoint_prompt_upsert",
                            "agent_tool": checkpoint.agent_id.as_ref().map(|a| a.tool.as_str())
                        })),
                    );
                }
            }
        }

        // Append checkpoint to the working log
        let append_start = Instant::now();
        working_log.append_checkpoint(&checkpoint)?;
        debug_log(&format!(
            "[BENCHMARK] Appending checkpoint to working log took {:?}",
            append_start.elapsed()
        ));
        checkpoints.push(checkpoint.clone());

        // Record agent usage metric for AI checkpoints
        if kind != CheckpointKind::Human {
            if let Some(agent_id) = &checkpoint.agent_id {
                let prompt_id = generate_short_hash(&agent_id.id, &agent_id.tool);

                let values = crate::metrics::AgentUsageValues::new();
                let mut attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                    .tool(&agent_id.tool)
                    .model(&agent_id.model)
                    .prompt_id(prompt_id)
                    .external_prompt_id(&agent_id.id);

                // Get repo URL from default remote
                if let Ok(Some(remote_name)) = repo.get_default_remote() {
                    if let Ok(remotes) = repo.remotes_with_urls() {
                        if let Some((_, url)) = remotes.into_iter().find(|(n, _)| n == &remote_name) {
                            if let Ok(normalized) = crate::repo_url::normalize_repo_url(&url) {
                                attrs = attrs.repo_url(normalized);
                            }
                        }
                    }
                }

                crate::metrics::record(values, attrs);
            }
        }
    }

    let agent_tool = if kind != CheckpointKind::Human
        && let Some(agent_run_result) = &agent_run_result
    {
        Some(agent_run_result.agent_id.tool.as_str())
    } else {
        None
    };

    // Print summary with new format
    if reset {
        debug_log("Working log reset. Starting fresh checkpoint.");
    }

    let label = if entries.len() > 1 {
        "checkpoint"
    } else {
        "commit"
    };

    if !quiet {
        let log_author = agent_tool.unwrap_or(author);
        // Only count files that actually have checkpoint entries to avoid confusion.
        // Files that were previously checkpointed but have no new changes won't have entries.
        let files_with_entries = entries.len();
        let total_uncommitted_files = files.len();

        if files_with_entries == total_uncommitted_files {
            // All files with changes got entries
            eprintln!(
                "{} {} changed {} file(s) that have changed since the last {}",
                kind.to_str(),
                log_author,
                files_with_entries,
                label
            );
        } else {
            // Some files were already checkpointed
            eprintln!(
                "{} {} changed {} of the {} file(s) that have changed since the last {} ({} already checkpointed)",
                kind.to_str(),
                log_author,
                files_with_entries,
                total_uncommitted_files,
                label,
                total_uncommitted_files - files_with_entries
            );
        }
    }

    // Return the requested values: (entries_len, files_len, working_log_len)
    debug_log(&format!(
        "[BENCHMARK] Total checkpoint run took {:?}",
        checkpoint_start.elapsed()
    ));
    Ok((entries.len(), files.len(), checkpoints.len()))
}

// Gets tracked changes AND
fn get_status_of_files(
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    edited_filepaths: HashSet<String>,
    skip_untracked: bool,
) -> Result<Vec<String>, GitAiError> {
    let mut files = Vec::new();

    // Use porcelain v2 format to get status

    let edited_filepaths_option = if edited_filepaths.is_empty() {
        None
    } else {
        Some(&edited_filepaths)
    };

    let status_start = Instant::now();
    let statuses = repo.status(edited_filepaths_option, skip_untracked)?;
    debug_log(&format!(
        "[BENCHMARK]   git status call took {:?}",
        status_start.elapsed()
    ));

    for entry in statuses {
        // Skip ignored files
        if entry.kind == EntryKind::Ignored {
            continue;
        }

        // Skip unmerged/conflicted files - we'll track them once the conflict is resolved
        if entry.kind == EntryKind::Unmerged {
            continue;
        }

        // Include files that have any change (staged or unstaged) or are untracked
        let has_change = entry.staged != StatusCode::Unmodified
            || entry.unstaged != StatusCode::Unmodified
            || entry.kind == EntryKind::Untracked;

        if has_change {
            // For deleted files, check if they were text files in HEAD
            let is_deleted =
                entry.staged == StatusCode::Deleted || entry.unstaged == StatusCode::Deleted;

            let is_text = if is_deleted {
                is_text_file_in_head(repo, &entry.path)
            } else {
                is_text_file(working_log, &entry.path)
            };

            if is_text {
                files.push(entry.path.clone());
            }
        }
    }

    Ok(files)
}

/// Get all files that should be tracked, including those from previous checkpoints and INITIAL attributions
///
fn get_all_tracked_files(
    repo: &Repository,
    _base_commit: &str,
    working_log: &PersistedWorkingLog,
    edited_filepaths: Option<&Vec<String>>,
    is_pre_commit: bool,
) -> Result<Vec<String>, GitAiError> {
    let mut files: HashSet<String> = edited_filepaths
        .map(|paths| paths.iter().cloned().collect())
        .unwrap_or_default();

    let initial_read_start = Instant::now();
    for file in working_log.read_initial_attributions().files.keys() {
        // Normalize path separators to forward slashes
        let normalized_path = normalize_to_posix(file);
        if is_text_file(working_log, &normalized_path) {
            files.insert(normalized_path);
        }
    }
    debug_log(&format!(
        "[BENCHMARK]   Reading INITIAL attributions in get_all_tracked_files took {:?}",
        initial_read_start.elapsed()
    ));

    let checkpoints_read_start = Instant::now();
    if let Ok(working_log_data) = working_log.read_all_checkpoints() {
        for checkpoint in &working_log_data {
            for entry in &checkpoint.entries {
                // Normalize path separators to forward slashes
                let normalized_path = normalize_to_posix(&entry.file);
                if !files.contains(&normalized_path) {
                    // Check if it's a text file before adding
                    if is_text_file(working_log, &normalized_path) {
                        files.insert(normalized_path);
                    }
                }
            }
        }
    }
    debug_log(&format!(
        "[BENCHMARK]   Reading checkpoints in get_all_tracked_files took {:?}",
        checkpoints_read_start.elapsed()
    ));

    let has_ai_checkpoints = if let Ok(working_log_data) = working_log.read_all_checkpoints() {
        working_log_data.iter().any(|checkpoint| {
            checkpoint.kind == CheckpointKind::AiAgent || checkpoint.kind == CheckpointKind::AiTab
        })
    } else {
        false
    };

    let status_files_start = Instant::now();
    let mut results_for_tracked_files = if is_pre_commit && !has_ai_checkpoints {
        get_status_of_files(repo, working_log, files, true)?
    } else {
        get_status_of_files(repo, working_log, files, false)?
    };
    debug_log(&format!(
        "[BENCHMARK]   get_status_of_files in get_all_tracked_files took {:?}",
        status_files_start.elapsed()
    ));

    // Ensure to always include all dirty files
    if let Some(ref dirty_files) = working_log.dirty_files {
        for file_path in dirty_files.keys() {
            // Normalize path separators to forward slashes
            let normalized_path = normalize_to_posix(file_path);
            // Only add if not already in the files list
            if !results_for_tracked_files.contains(&normalized_path) {
                // Check if it's a text file before adding
                if is_text_file(working_log, &normalized_path) {
                    results_for_tracked_files.push(normalized_path);
                }
            }
        }
    }

    Ok(results_for_tracked_files)
}

fn save_current_file_states(
    working_log: &PersistedWorkingLog,
    files: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let read_start = Instant::now();

    // Extract only the data we need (no cloning the entire working_log)
    let blobs_dir = working_log.dir.join("blobs");
    let repo_workdir = working_log.repo_workdir.clone();
    let dirty_files = working_log.dirty_files.clone();

    // Process files concurrently with a semaphore limiting to 8 at a time
    let file_content_hashes = smol::block_on(async {
        let semaphore = Arc::new(smol::lock::Semaphore::new(8));
        let blobs_dir = Arc::new(blobs_dir);
        let repo_workdir = Arc::new(repo_workdir);
        let dirty_files = Arc::new(dirty_files);

        let futures = files.iter().map(|file_path| {
            let file_path = file_path.clone();
            let blobs_dir = Arc::clone(&blobs_dir);
            let repo_workdir = Arc::clone(&repo_workdir);
            let dirty_files = Arc::clone(&dirty_files);
            let semaphore = Arc::clone(&semaphore);

            async move {
                // Acquire semaphore permit
                let _permit = semaphore.acquire().await;

                // Read file content - check dirty_files first, then filesystem
                let content = if let Some(ref dirty_map) = *dirty_files {
                    dirty_map.get(&file_path).cloned()
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    // Construct absolute path
                    let abs_path = if std::path::Path::new(&file_path).is_absolute() {
                        file_path.clone()
                    } else {
                        repo_workdir.join(&file_path).to_string_lossy().to_string()
                    };
                    // Read from filesystem
                    std::fs::read_to_string(&abs_path).unwrap_or_default()
                });

                // Create SHA256 hash of the content
                let mut hasher = Sha256::new();
                hasher.update(content.as_bytes());
                let sha = format!("{:x}", hasher.finalize());

                // Ensure blobs directory exists
                std::fs::create_dir_all(&*blobs_dir)?;

                // Write content to blob file
                let blob_path = blobs_dir.join(&sha);
                std::fs::write(blob_path, content)?;

                Ok::<(String, String), GitAiError>((file_path, sha))
            }
        });

        // Collect results from all concurrent operations
        let results: Vec<Result<(String, String), GitAiError>> =
            stream::iter(futures).buffer_unordered(8).collect().await;

        // Convert results into HashMap
        let mut file_content_hashes = HashMap::new();
        for result in results {
            let (file_path, content_hash) = result?;
            file_content_hashes.insert(file_path, content_hash);
        }

        Ok::<HashMap<String, String>, GitAiError>(file_content_hashes)
    })?;

    Ok(file_content_hashes)
}

fn get_checkpoint_entry_for_file(
    file_path: String,
    kind: CheckpointKind,
    repo: Repository,
    working_log: PersistedWorkingLog,
    previous_checkpoints: Arc<Vec<Checkpoint>>,
    file_content_hash: String,
    author_id: Arc<String>,
    head_commit_sha: Arc<Option<String>>,
    head_tree_id: Arc<Option<String>>,
    initial_attributions: Arc<HashMap<String, Vec<LineAttribution>>>,
    ts: u128,
) -> Result<Option<(WorkingLogEntry, FileLineStats)>, GitAiError> {
    let feature_flag_inter_commit_move = Config::get().get_feature_flags().inter_commit_move;

    let file_start = Instant::now();
    let current_content = working_log
        .read_current_file_content(&file_path)
        .unwrap_or_default();

    // Try to get previous state from checkpoints first
    let from_checkpoint = previous_checkpoints.iter().rev().find_map(|checkpoint| {
        checkpoint
            .entries
            .iter()
            .find(|e| e.file == file_path)
            .map(|entry| {
                (
                    working_log
                        .get_file_version(&entry.blob_sha)
                        .unwrap_or_default(),
                    entry.attributions.clone(),
                )
            })
    });

    // Get INITIAL attributions for this file (needed early for the skip check)
    let initial_attrs_for_file = initial_attributions
        .get(&file_path)
        .cloned()
        .unwrap_or_default();

    let is_from_checkpoint = from_checkpoint.is_some();
    let (previous_content, prev_attributions) = if let Some((content, attrs)) = from_checkpoint {
        // File exists in a previous checkpoint - use that
        (content, attrs)
    } else {
        // File doesn't exist in any previous checkpoint - need to initialize from git + INITIAL
        // Get previous content from HEAD tree
        let previous_content = if let Some(tree_id) = head_tree_id.as_ref().as_ref() {
            let head_tree = repo.find_tree(tree_id.clone()).ok();
            if let Some(tree) = head_tree {
                match tree.get_path(std::path::Path::new(&file_path)) {
                    Ok(entry) => {
                        if let Ok(blob) = repo.find_blob(entry.id()) {
                            let blob_content = blob.content().unwrap_or_default();
                            String::from_utf8_lossy(&blob_content).to_string()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Skip if no changes, UNLESS we have INITIAL attributions for this file
        // (in which case we need to create an entry to record those attributions)
        if current_content == previous_content && initial_attrs_for_file.is_empty() {
            return Ok(None);
        }

        // Build a set of lines covered by INITIAL attributions
        let mut initial_covered_lines: HashSet<u32> = HashSet::new();
        for attr in &initial_attrs_for_file {
            for line in attr.start_line..=attr.end_line {
                initial_covered_lines.insert(line);
            }
        }

        // Start with INITIAL attributions (they win)
        let mut prev_line_attributions = initial_attrs_for_file.clone();
        let mut blamed_lines: HashSet<u32> = HashSet::new();

        // Get blame for lines not in INITIAL
        let blame_start = Instant::now();
        let mut ai_blame_opts = GitAiBlameOptions::default();
        ai_blame_opts.no_output = true;
        ai_blame_opts.return_human_authors_as_human = true;
        ai_blame_opts.use_prompt_hashes_as_names = true;
        ai_blame_opts.newest_commit = head_commit_sha.as_ref().clone();
        ai_blame_opts.oldest_date = Some(OLDEST_AI_BLAME_DATE.clone());
        let ai_blame = if feature_flag_inter_commit_move {
            repo.blame(&file_path, &ai_blame_opts).ok()
        } else {
            // When skipping blame, default all lines to "human"
            let total_lines = previous_content.lines().count() as u32;
            let mut line_authors: HashMap<u32, String> = HashMap::new();
            for line_num in 1..=total_lines {
                line_authors.insert(line_num, CheckpointKind::Human.to_str());
            }
            let prompt_records: HashMap<String, PromptRecord> = HashMap::new();
            Some((line_authors, prompt_records))
        };

        debug_log(&format!(
            "[BENCHMARK] Blame for {} took {:?}",
            file_path,
            blame_start.elapsed()
        ));

        // Add blame results for lines NOT covered by INITIAL
        if let Some((blames, _)) = ai_blame {
            for (line, author) in blames {
                blamed_lines.insert(line);
                // Skip if INITIAL already has this line
                if initial_covered_lines.contains(&line) {
                    continue;
                }

                // Skip human-authored lines - they should remain human
                if author == CheckpointKind::Human.to_str() {
                    continue;
                }

                prev_line_attributions.push(LineAttribution {
                    start_line: line,
                    end_line: line,
                    author_id: author.clone(),
                    overrode: None,
                });
            }
        }

        // For AI checkpoints, attribute any lines NOT in INITIAL and NOT returned by ai_blame
        if kind != CheckpointKind::Human {
            let total_lines = current_content.lines().count() as u32;
            for line_num in 1..=total_lines {
                if !initial_covered_lines.contains(&line_num) && !blamed_lines.contains(&line_num) {
                    prev_line_attributions.push(LineAttribution {
                        start_line: line_num,
                        end_line: line_num,
                        author_id: author_id.as_ref().clone(),
                        overrode: None,
                    });
                }
            }
        }

        // For INITIAL attributions, we need to use current_content (not previous_content)
        // because INITIAL line numbers refer to the current state of the file
        let content_for_line_conversion = if !initial_attrs_for_file.is_empty() {
            &current_content
        } else {
            &previous_content
        };

        // Convert any line attributions to character attributions
        let prev_attributions =
            crate::authorship::attribution_tracker::line_attributions_to_attributions(
                &prev_line_attributions,
                content_for_line_conversion,
                INITIAL_ATTRIBUTION_TS,
            );

        // When we have INITIAL attributions, they describe the current state of the file.
        // We need to pass current_content as previous_content so the attributions are preserved.
        // The tracker will see no changes and preserve the INITIAL attributions.
        let adjusted_previous = if !initial_attrs_for_file.is_empty() {
            current_content.clone()
        } else {
            previous_content
        };

        (adjusted_previous, prev_attributions)
    };

    // Skip if no changes (but we already checked this earlier, accounting for INITIAL attributions)
    // For files from previous checkpoints, check if content has changed
    if is_from_checkpoint && current_content == previous_content {
        return Ok(None);
    }

    let (entry, stats) = make_entry_for_file(
        &file_path,
        &file_content_hash,
        author_id.as_ref(),
        &previous_content,
        &prev_attributions,
        &current_content,
        ts,
    )?;
    debug_log(&format!(
        "[BENCHMARK] Processing file {} took {:?}",
        file_path,
        file_start.elapsed()
    ));
    Ok(Some((entry, stats)))
}

async fn get_checkpoint_entries(
    kind: CheckpointKind,
    repo: &Repository,
    working_log: &PersistedWorkingLog,
    files: &[String],
    file_content_hashes: &HashMap<String, String>,
    previous_checkpoints: &[Checkpoint],
    agent_run_result: Option<&AgentRunResult>,
    ts: u128,
) -> Result<(Vec<WorkingLogEntry>, Vec<FileLineStats>), GitAiError> {
    let entries_fn_start = Instant::now();

    // Read INITIAL attributions from working log (empty if file doesn't exist)
    let initial_read_start = Instant::now();
    let initial_data = working_log.read_initial_attributions();
    let initial_attributions = initial_data.files;
    debug_log(&format!(
        "[BENCHMARK] Reading initial attributions took {:?}",
        initial_read_start.elapsed()
    ));

    // Determine author_id based on checkpoint kind and agent_id
    let author_id = if kind != CheckpointKind::Human {
        // For AI checkpoints, use session hash
        agent_run_result
            .map(|result| {
                crate::authorship::authorship_log_serialization::generate_short_hash(
                    &result.agent_id.id,
                    &result.agent_id.tool,
                )
            })
            .unwrap_or_else(|| kind.to_str())
    } else {
        // For human checkpoints, use checkpoint kind string
        kind.to_str()
    };

    // Get HEAD commit info for git operations
    let head_commit = repo
        .head()
        .ok()
        .and_then(|h| h.target().ok())
        .and_then(|oid| repo.find_commit(oid).ok());
    let head_commit_sha = head_commit.as_ref().map(|c| c.id().to_string());
    let head_tree_id = head_commit
        .as_ref()
        .and_then(|c| c.tree().ok())
        .map(|t| t.id().to_string());

    const MAX_CONCURRENT: usize = 30;

    // Create a semaphore to limit concurrent tasks
    let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));

    // Move checkpoint data to Arc once, outside the loop to avoid repeated allocations
    let previous_checkpoints = Arc::new(previous_checkpoints.to_vec());

    // Move other repeated allocations outside the loop
    let author_id = Arc::new(author_id);
    let head_commit_sha = Arc::new(head_commit_sha);
    let head_tree_id = Arc::new(head_tree_id);
    let initial_attributions = Arc::new(initial_attributions);

    // Spawn tasks for each file
    let spawn_start = Instant::now();
    let mut tasks = Vec::new();

    for file_path in files {
        let file_path = file_path.clone();
        let repo = repo.clone();
        let working_log = working_log.clone();
        let previous_checkpoints = Arc::clone(&previous_checkpoints);
        let author_id = Arc::clone(&author_id);
        let head_commit_sha = Arc::clone(&head_commit_sha);
        let head_tree_id = Arc::clone(&head_tree_id);
        let blob_sha = file_content_hashes
            .get(&file_path)
            .cloned()
            .unwrap_or_default();
        let initial_attributions = Arc::clone(&initial_attributions);
        let semaphore = Arc::clone(&semaphore);
        let kind = kind.clone();

        let task = smol::spawn(async move {
            // Acquire semaphore permit to limit concurrency
            let _permit = semaphore.acquire().await;

            // Wrap all the blocking git operations in smol::unblock
            smol::unblock(move || {
                get_checkpoint_entry_for_file(
                    file_path,
                    kind,
                    repo,
                    working_log,
                    previous_checkpoints,
                    blob_sha,
                    author_id.clone(),
                    head_commit_sha.clone(),
                    head_tree_id.clone(),
                    initial_attributions.clone(),
                    ts,
                )
            })
            .await
        });

        tasks.push(task);
    }
    debug_log(&format!(
        "[BENCHMARK] Spawning {} tasks took {:?}",
        tasks.len(),
        spawn_start.elapsed()
    ));

    // Await all tasks concurrently
    let await_start = Instant::now();
    let results = futures::future::join_all(tasks).await;
    debug_log(&format!(
        "[BENCHMARK] Awaiting {} tasks took {:?}",
        results.len(),
        await_start.elapsed()
    ));

    // Process results
    let process_start = Instant::now();
    let results_count = results.len();
    let mut entries = Vec::new();
    let mut file_stats = Vec::new();
    for result in results {
        match result {
            Ok(Some((entry, stats))) => {
                entries.push(entry);
                file_stats.push(stats);
            }
            Ok(None) => {} // File had no changes
            Err(e) => return Err(e),
        }
    }
    debug_log(&format!(
        "[BENCHMARK] Processing {} results took {:?}",
        results_count,
        process_start.elapsed()
    ));
    debug_log(&format!(
        "[BENCHMARK] get_checkpoint_entries function total took {:?}",
        entries_fn_start.elapsed()
    ));

    Ok((entries, file_stats))
}

fn make_entry_for_file(
    file_path: &str,
    blob_sha: &str,
    author_id: &str,
    previous_content: &str,
    previous_attributions: &Vec<Attribution>,
    content: &str,
    ts: u128,
) -> Result<(WorkingLogEntry, FileLineStats), GitAiError> {
    let tracker = AttributionTracker::new();

    let fill_start = Instant::now();
    let filled_in_prev_attributions = tracker.attribute_unattributed_ranges(
        previous_content,
        previous_attributions,
        &CheckpointKind::Human.to_str(),
        ts - 1,
    );
    debug_log(&format!(
        "[BENCHMARK]   attribute_unattributed_ranges for {} took {:?}",
        file_path,
        fill_start.elapsed()
    ));

    let update_start = Instant::now();
    let new_attributions = tracker.update_attributions(
        previous_content,
        content,
        &filled_in_prev_attributions,
        author_id,
        ts,
    )?;
    debug_log(&format!(
        "[BENCHMARK]   update_attributions for {} took {:?}",
        file_path,
        update_start.elapsed()
    ));

    // TODO Consider discarding any "uncontentious" attributions for the human author. Any human attributions that do not share a line with any other author's attributions can be discarded.
    // let filtered_attributions = crate::authorship::attribution_tracker::discard_uncontentious_attributions_for_author(&new_attributions, &CheckpointKind::Human.to_str());

    let line_attr_start = Instant::now();
    let line_attributions =
        crate::authorship::attribution_tracker::attributions_to_line_attributions(
            &new_attributions,
            content,
        );
    debug_log(&format!(
        "[BENCHMARK]   attributions_to_line_attributions for {} took {:?}",
        file_path,
        line_attr_start.elapsed()
    ));

    // Compute line stats while we already have both contents in memory
    let stats_start = Instant::now();
    let line_stats = compute_file_line_stats(previous_content, content);
    debug_log(&format!(
        "[BENCHMARK]   compute_file_line_stats for {} took {:?}",
        file_path,
        stats_start.elapsed()
    ));

    let entry = WorkingLogEntry::new(
        file_path.to_string(),
        blob_sha.to_string(),
        new_attributions,
        line_attributions,
    );

    Ok((entry, line_stats))
}

/// Compute line statistics for a single file by diffing previous and current content
fn compute_file_line_stats(previous_content: &str, current_content: &str) -> FileLineStats {
    let mut stats = FileLineStats::default();

    // Use imara_diff to count line changes (matches git's diff algorithm)
    let changes = compute_line_changes(previous_content, current_content);
    for change in changes {
        match change.tag() {
            LineChangeTag::Insert => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.additions += change.value().lines().count() as u32;
                stats.additions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Delete => {
                let non_whitespace_lines = change
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count() as u32;
                stats.deletions += change.value().lines().count() as u32;
                stats.deletions_sloc += non_whitespace_lines;
            }
            LineChangeTag::Equal => {}
        }
    }

    stats
}

/// Aggregate line statistics from individual file stats
/// This avoids redundant diff computation since stats are already computed during entry creation
fn compute_line_stats(
    file_stats: &[FileLineStats],
) -> Result<crate::authorship::working_log::CheckpointLineStats, GitAiError> {
    let mut stats = crate::authorship::working_log::CheckpointLineStats::default();

    // Aggregate line stats from all files
    for file_stat in file_stats {
        stats.additions += file_stat.additions;
        stats.deletions += file_stat.deletions;
        stats.additions_sloc += file_stat.additions_sloc;
        stats.deletions_sloc += file_stat.deletions_sloc;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;

    #[test]
    fn test_checkpoint_with_staged_changes() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make changes to the file
        file.append("New line added by user\n").unwrap();

        // Note: TmpFile.append() automatically stages changes (see write_to_disk in test_utils)
        // So at this point, the file has staged changes

        // Run checkpoint - it should track the changes even though they're staged
        let (entries_len, files_len, _checkpoints_len) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        // The bug: when changes are staged, entries_len is 0 instead of 1
        assert_eq!(files_len, 1, "Should have 1 file with changes");
        assert_eq!(
            entries_len, 1,
            "Should have 1 file entry in checkpoint (staged changes should be tracked)"
        );
    }

    #[test]
    fn test_checkpoint_with_staged_changes_after_previous_checkpoint() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make first changes and checkpoint
        file.append("First change\n").unwrap();
        let (entries_len_1, files_len_1, _) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        assert_eq!(
            files_len_1, 1,
            "First checkpoint: should have 1 file with changes"
        );
        assert_eq!(
            entries_len_1, 1,
            "First checkpoint: should have 1 file entry"
        );

        // Make second changes - these are already staged by append()
        file.append("Second change\n").unwrap();

        // Run checkpoint again - it should track the staged changes even after a previous checkpoint
        let (entries_len_2, files_len_2, _) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        assert_eq!(
            files_len_2, 1,
            "Second checkpoint: should have 1 file with changes"
        );
        assert_eq!(
            entries_len_2, 1,
            "Second checkpoint: should have 1 file entry in checkpoint (staged changes should be tracked)"
        );
    }

    #[test]
    fn test_checkpoint_with_only_staged_no_unstaged_changes() {
        use std::fs;

        // Create a repo with an initial commit
        let (tmp_repo, file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the file path
        let file_path = file.path();
        let filename = file.filename();

        // Manually modify the file (bypassing TmpFile's automatic staging)
        let mut content = fs::read_to_string(&file_path).unwrap();
        content.push_str("New line for staging test\n");
        fs::write(&file_path, &content).unwrap();

        // Now manually stage it using git (this is what "git add" does)
        tmp_repo.stage_file(filename).unwrap();

        // At this point: HEAD has old content, index has new content, workdir has new content
        // And unstaged should be "Unmodified" because workdir == index

        // Now run checkpoint
        let (entries_len, files_len, _checkpoints_len) =
            tmp_repo.trigger_checkpoint_with_author("Aidan").unwrap();

        // This should work: we should see 1 file with 1 entry
        assert_eq!(files_len, 1, "Should detect 1 file with staged changes");
        assert_eq!(
            entries_len, 1,
            "Should track the staged changes in checkpoint"
        );
    }

    #[test]
    fn test_checkpoint_skips_conflicted_files() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the current branch name (whatever the default is)
        let base_branch = tmp_repo.current_branch().unwrap();

        // Create a branch and make different changes on each branch to create a conflict
        tmp_repo.create_branch("feature-branch").unwrap();

        // On feature branch, modify the file
        file.append("Feature branch change\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("FeatureUser")
            .unwrap();
        tmp_repo.commit_with_message("Feature commit").unwrap();

        // Switch back to base branch and make conflicting changes
        tmp_repo.switch_branch(&base_branch).unwrap();
        file.append("Main branch change\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("MainUser").unwrap();
        tmp_repo.commit_with_message("Main commit").unwrap();

        // Attempt to merge feature-branch into base branch - this should create a conflict
        let has_conflicts = tmp_repo.merge_with_conflicts("feature-branch").unwrap();
        assert!(has_conflicts, "Should have merge conflicts");

        // Try to checkpoint while there are conflicts
        let (entries_len, files_len, _) = tmp_repo.trigger_checkpoint_with_author("Human").unwrap();

        // Checkpoint should skip conflicted files
        assert_eq!(
            files_len, 0,
            "Should have 0 files (conflicted file should be skipped)"
        );
        assert_eq!(
            entries_len, 0,
            "Should have 0 entries (conflicted file should be skipped)"
        );
    }

    #[test]
    fn test_checkpoint_with_paths_outside_repo() {
        use crate::authorship::transcript::AiTranscript;
        use crate::authorship::working_log::AgentId;
        use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;

        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Make changes to the file
        file.append("New line added\n").unwrap();

        // Create agent run result with paths outside the repo
        let agent_run_result = AgentRunResult {
            agent_id: AgentId {
                tool: "test_tool".to_string(),
                id: "test_session".to_string(),
                model: "test_model".to_string(),
            },
            agent_metadata: None,
            transcript: Some(AiTranscript { messages: vec![] }),
            checkpoint_kind: CheckpointKind::AiAgent,
            repo_working_dir: None,
            edited_filepaths: Some(vec![
                "/tmp/outside_file.txt".to_string(),
                "../outside_parent.txt".to_string(),
                file.filename().to_string(), // This one is valid
            ]),
            will_edit_filepaths: None,
            dirty_files: None,
        };

        // Run checkpoint - should not crash even with paths outside repo
        let result =
            tmp_repo.trigger_checkpoint_with_agent_result("test_user", Some(agent_run_result));

        // Should succeed without crashing
        assert!(
            result.is_ok(),
            "Checkpoint should succeed even with paths outside repo: {:?}",
            result.err()
        );

        let (entries_len, files_len, _) = result.unwrap();
        // Should only process the valid file
        assert_eq!(files_len, 1, "Should process 1 valid file");
        assert_eq!(entries_len, 1, "Should create 1 entry");
    }

    #[test]
    fn test_checkpoint_works_after_conflict_resolution_maintains_authorship() {
        // Create a repo with an initial commit
        let (tmp_repo, mut file, _) = TmpRepo::new_with_base_commit().unwrap();

        // Get the current branch name (whatever the default is)
        let base_branch = tmp_repo.current_branch().unwrap();

        // Checkpoint initial state to track the base authorship
        let file_path = file.path();
        let initial_content = std::fs::read_to_string(&file_path).unwrap();
        println!("Initial content:\n{}", initial_content);

        // Create a branch and make changes
        tmp_repo.create_branch("feature-branch").unwrap();
        file.append("Feature line 1\n").unwrap();
        file.append("Feature line 2\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("AI_Agent").unwrap();
        tmp_repo.commit_with_message("Feature commit").unwrap();

        // Switch back to base branch and make conflicting changes
        tmp_repo.switch_branch(&base_branch).unwrap();
        file.append("Main line 1\n").unwrap();
        file.append("Main line 2\n").unwrap();
        tmp_repo.trigger_checkpoint_with_author("Human").unwrap();
        tmp_repo.commit_with_message("Main commit").unwrap();

        // Attempt to merge feature-branch into base branch - this should create a conflict
        let has_conflicts = tmp_repo.merge_with_conflicts("feature-branch").unwrap();
        assert!(has_conflicts, "Should have merge conflicts");

        // While there are conflicts, checkpoint should skip the file
        let (entries_len_conflict, files_len_conflict, _) =
            tmp_repo.trigger_checkpoint_with_author("Human").unwrap();
        assert_eq!(
            files_len_conflict, 0,
            "Should skip conflicted files during conflict"
        );
        assert_eq!(
            entries_len_conflict, 0,
            "Should not create entries for conflicted files"
        );

        // Resolve the conflict by choosing "ours" (base branch)
        tmp_repo.resolve_conflict(file.filename(), "ours").unwrap();

        // Verify content to ensure the resolution was applied correctly
        let resolved_content = std::fs::read_to_string(&file_path).unwrap();
        println!("Resolved content after resolution:\n{}", resolved_content);
        assert!(
            resolved_content.contains("Main line 1"),
            "Should contain base branch content (we chose 'ours')"
        );
        assert!(
            resolved_content.contains("Main line 2"),
            "Should contain base branch content (we chose 'ours')"
        );
        assert!(
            !resolved_content.contains("Feature line 1"),
            "Should not contain feature branch content (we chose 'ours')"
        );

        // After resolution, make additional changes to test that checkpointing works again
        file.append("Post-resolution line 1\n").unwrap();
        file.append("Post-resolution line 2\n").unwrap();

        // Now checkpoint should work and track the new changes
        let (entries_len_after, files_len_after, _) =
            tmp_repo.trigger_checkpoint_with_author("Human").unwrap();

        println!(
            "After resolution and new changes: entries_len={}, files_len={}",
            entries_len_after, files_len_after
        );

        // The file should be tracked with the new changes
        assert_eq!(
            files_len_after, 1,
            "Should detect 1 file with new changes after conflict resolution"
        );
        assert_eq!(
            entries_len_after, 1,
            "Should create 1 entry for new changes after conflict resolution"
        );
    }

    #[test]
    fn test_compute_line_stats_ignores_whitespace_only_lines() {
        let (tmp_repo, _lines_file, _alphabet_file) = TmpRepo::new_with_base_commit().unwrap();

        let repo =
            crate::git::repository::find_repository_in_path(tmp_repo.path().to_str().unwrap())
                .expect("Repository should exist");

        let base_commit = repo
            .head()
            .ok()
            .and_then(|head| head.target().ok())
            .unwrap_or_else(|| "initial".to_string());
        let working_log = repo.storage.working_log_for_base_commit(&base_commit);

        let mut test_file = tmp_repo
            .write_file("whitespace.txt", "Seed line\n", true)
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Setup")
            .expect("Setup checkpoint should succeed");

        test_file
            .append("\n\n   \nVisible line one\n\n\t\nVisible line two\n  \n")
            .unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Aidan")
            .expect("First checkpoint should succeed");

        let after_add_stats = working_log
            .read_all_checkpoints()
            .expect("Should read checkpoints after addition");
        let after_add_last = after_add_stats
            .last()
            .expect("At least one checkpoint expected")
            .line_stats
            .clone();

        assert_eq!(
            after_add_last.additions, 8,
            "Additions includes empty lines"
        );
        assert_eq!(after_add_last.deletions, 0, "No deletions expected yet");
        assert_eq!(
            after_add_last.additions_sloc, 2,
            "Only visible lines counted"
        );
        assert_eq!(
            after_add_last.deletions_sloc, 0,
            "No deletions expected yet"
        );

        let cleaned_content = std::fs::read_to_string(test_file.path()).unwrap();
        let cleaned_lines: Vec<&str> = cleaned_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let cleaned_body = format!("{}\n", cleaned_lines.join("\n"));
        test_file.update(&cleaned_body).unwrap();

        tmp_repo
            .trigger_checkpoint_with_author("Aidan")
            .expect("Second checkpoint should succeed");

        let after_delete_stats = working_log
            .read_all_checkpoints()
            .expect("Should read checkpoints after deletion");
        let latest_stats = after_delete_stats
            .last()
            .expect("At least one checkpoint expected")
            .line_stats
            .clone();

        assert_eq!(
            latest_stats.additions, 0,
            "No additions in cleanup checkpoint"
        );
        assert_eq!(latest_stats.deletions, 6, "Deletions includes empty lines");
        assert_eq!(
            latest_stats.additions_sloc, 0,
            "No additions in cleanup checkpoint"
        );
        assert_eq!(
            latest_stats.deletions_sloc, 0,
            "Whitespace deletions ignored"
        );
    }
}

fn is_text_file(working_log: &PersistedWorkingLog, path: &str) -> bool {
    // Normalize path for dirty_files lookup
    let normalized_path = normalize_to_posix(path);
    let skip_metadata_check = working_log
        .dirty_files
        .as_ref()
        .map(|m| m.contains_key(&normalized_path))
        .unwrap_or(false);

    if !skip_metadata_check {
        if let Ok(metadata) = std::fs::metadata(working_log.to_repo_absolute_path(&normalized_path))
        {
            if !metadata.is_file() {
                return false;
            }
        } else {
            return false; // If metadata can't be read, treat as non-text
        }
    }

    working_log
        .read_current_file_content(&normalized_path)
        .map(|content| !content.chars().any(|c| c == '\0'))
        .unwrap_or(false)
}

fn is_text_file_in_head(repo: &Repository, path: &str) -> bool {
    // For deleted files, check if they were text files in HEAD
    let head_commit = match repo
        .head()
        .ok()
        .and_then(|h| h.target().ok())
        .and_then(|oid| repo.find_commit(oid).ok())
    {
        Some(commit) => commit,
        None => return false,
    };

    let head_tree = match head_commit.tree().ok() {
        Some(tree) => tree,
        None => return false,
    };

    match head_tree.get_path(std::path::Path::new(path)) {
        Ok(entry) => {
            if let Ok(blob) = repo.find_blob(entry.id()) {
                // Consider a file text if it contains no null bytes
                let blob_content = match blob.content() {
                    Ok(content) => content,
                    Err(_) => return false,
                };
                !blob_content.contains(&0)
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Upsert a checkpoint prompt to the internal database
fn upsert_checkpoint_prompt_to_db(
    checkpoint: &Checkpoint,
    workdir: String,
    commit_sha: Option<String>,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};

    let record = PromptDbRecord::from_checkpoint(checkpoint, Some(workdir), commit_sha)
        .ok_or_else(|| {
            GitAiError::Generic("Failed to create prompt record from checkpoint".to_string())
        })?;

    let db = InternalDatabase::global()?;
    let mut db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    db_guard.upsert_prompt(&record)?;

    Ok(())
}
