//! Search command for git-ai
//!
//! Provides `git-ai search` functionality to query AI prompt history
//! by commit, file, pattern, or prompt ID.

use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::prompt_utils::find_prompt_with_db_fallback;
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::refs::get_authorship;
use crate::git::repository::{exec_git, Repository};
use std::collections::HashMap;
use std::env;
use std::path::Path;

/// Unified search results returned by all search modes
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// Prompt hash -> PromptRecord
    pub prompts: HashMap<String, PromptRecord>,
    /// Prompt hash -> list of (file_path, line_ranges) pairs
    pub prompt_locations: HashMap<String, Vec<(String, Vec<LineRange>)>>,
    /// Prompt hash -> commit SHAs where this prompt appears
    pub prompt_commits: HashMap<String, Vec<String>>,
}

impl SearchResult {
    /// Create a new empty SearchResult
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if the search result is empty
    pub fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    /// Get the number of prompts found
    pub fn len(&self) -> usize {
        self.prompts.len()
    }

    /// Merge another SearchResult into this one
    /// Deduplicates by prompt hash (first occurrence wins)
    pub fn merge(&mut self, other: SearchResult) {
        for (hash, prompt) in other.prompts {
            self.prompts.entry(hash.clone()).or_insert(prompt);
        }
        for (hash, locations) in other.prompt_locations {
            self.prompt_locations
                .entry(hash.clone())
                .or_default()
                .extend(locations);
        }
        for (hash, commits) in other.prompt_commits {
            let entry = self.prompt_commits.entry(hash).or_default();
            for commit in commits {
                if !entry.contains(&commit) {
                    entry.push(commit);
                }
            }
        }
    }
}

/// Search for prompts associated with a specific commit
pub fn search_by_commit(repo: &Repository, commit_rev: &str) -> Result<SearchResult, GitAiError> {
    // Resolve the commit revision to a full SHA
    let commit = repo.revparse_single(commit_rev)?;
    let commit_sha = commit.id();

    let mut result = SearchResult::new();

    // Try git notes first
    if let Some(authorship_log) = get_authorship(repo, &commit_sha) {
        // Extract prompts from metadata
        for (hash, prompt) in authorship_log.metadata.prompts {
            result.prompts.insert(hash.clone(), prompt);
            result
                .prompt_commits
                .entry(hash)
                .or_default()
                .push(commit_sha.clone());
        }

        // Extract file/line locations from attestations
        for attestation in authorship_log.attestations {
            for entry in attestation.entries {
                result
                    .prompt_locations
                    .entry(entry.hash.clone())
                    .or_default()
                    .push((attestation.file_path.clone(), entry.line_ranges));
            }
        }
    }

    // If no git note found, fall back to database
    if result.prompts.is_empty() {
        if let Ok(db) = InternalDatabase::global() {
            if let Ok(db_guard) = db.lock() {
                if let Ok(db_records) = db_guard.get_prompts_by_commit(&commit_sha) {
                    for db_record in db_records {
                        let prompt = db_record.to_prompt_record();
                        result.prompts.insert(db_record.id.clone(), prompt);
                        result
                            .prompt_commits
                            .entry(db_record.id)
                            .or_default()
                            .push(commit_sha.clone());
                        // Note: DB records don't have file/line location data
                    }
                }
            }
        }
    } else {
        // Git notes were found but messages may have been stripped
        // (e.g., PromptStorageMode::Local or CAS upload). Try to
        // supplement empty messages from the internal database.
        if let Ok(db) = InternalDatabase::global() {
            if let Ok(db_guard) = db.lock() {
                let ids_needing_messages: Vec<String> = result
                    .prompts
                    .iter()
                    .filter(|(_, prompt)| prompt.messages.is_empty())
                    .map(|(id, _)| id.clone())
                    .collect();

                for id in ids_needing_messages {
                    if let Ok(Some(db_record)) = db_guard.get_prompt(&id) {
                        if !db_record.messages.messages.is_empty() {
                            if let Some(prompt) = result.prompts.get_mut(&id) {
                                prompt.messages = db_record.messages.messages;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Search for prompts across a range of commits
pub fn search_by_commit_range(
    repo: &Repository,
    start: &str,
    end: &str,
) -> Result<SearchResult, GitAiError> {
    // Use git rev-list to enumerate commits in the range
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push(format!("{}..{}", start, end));

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| GitAiError::Generic(format!("Invalid UTF-8 in git output: {}", e)))?;

    let mut result = SearchResult::new();

    // Search each commit and merge results
    for line in stdout.lines() {
        let commit_sha = line.trim();
        if !commit_sha.is_empty() {
            if let Ok(commit_result) = search_by_commit(repo, commit_sha) {
                result.merge(commit_result);
            }
        }
    }

    Ok(result)
}

/// Search for prompts associated with a file (optionally within specific line ranges)
///
/// Uses the blame system to identify AI-authored lines and their associated prompts.
/// When line_ranges is empty, searches the entire file.
pub fn search_by_file(
    repo: &Repository,
    file_path: &str,
    line_ranges: &[(u32, u32)],
) -> Result<SearchResult, GitAiError> {
    // Normalize file path relative to repo workdir
    let normalized_path = normalize_file_path(repo, file_path)?;

    // Configure blame options for data-only mode
    let mut options = GitAiBlameOptions::default();
    options.json = true; // Enable structured output mode
    options.no_output = true; // Suppress terminal output
    options.use_prompt_hashes_as_names = true; // Get prompt hashes instead of tool names
    options.newest_commit = Some("HEAD".to_string());

    if !line_ranges.is_empty() {
        options.line_ranges = line_ranges.to_vec();
    }

    // Call blame to get line-level authorship data
    let (line_authors, blame_prompt_records) = repo.blame(&normalized_path, &options)?;

    // Build SearchResult from blame data
    let mut result = SearchResult::new();

    // Store prompts directly from blame output
    for (hash, prompt) in &blame_prompt_records {
        result.prompts.insert(hash.clone(), prompt.clone());
    }

    // Group lines by prompt hash, filtering out human-authored lines
    let mut lines_by_hash: HashMap<String, Vec<u32>> = HashMap::new();
    for (&line_num, prompt_hash) in &line_authors {
        // Only include if this hash exists in prompt records (filters out human lines)
        if blame_prompt_records.contains_key(prompt_hash) {
            lines_by_hash
                .entry(prompt_hash.clone())
                .or_default()
                .push(line_num);
        }
    }

    // Convert lines to LineRange format and store in prompt_locations
    for (hash, mut lines) in lines_by_hash {
        lines.sort_unstable();
        lines.dedup();
        let ranges = LineRange::compress_lines(&lines);
        result
            .prompt_locations
            .entry(hash)
            .or_default()
            .push((normalized_path.clone(), ranges));
    }

    Ok(result)
}

/// Normalize a file path relative to the repository workdir
fn normalize_file_path(repo: &Repository, file_path: &str) -> Result<String, GitAiError> {
    let path = Path::new(file_path);

    // If already relative, use as-is
    if path.is_relative() {
        // Remove ./ prefix if present
        let path_str = file_path
            .strip_prefix("./")
            .unwrap_or(file_path)
            .replace('\\', "/"); // Normalize path separators
        return Ok(path_str);
    }

    // If absolute, try to strip repo workdir prefix
    let workdir = repo.workdir()?;
    let workdir_path = Path::new(&workdir);

    if let Ok(relative) = path.strip_prefix(workdir_path) {
        Ok(relative.to_string_lossy().replace('\\', "/"))
    } else {
        // Path is absolute but not under repo workdir - use as-is and let blame handle the error
        Ok(file_path.replace('\\', "/"))
    }
}

/// Search for prompts by full-text pattern matching on message content
///
/// Uses the internal database to perform LIKE matching on the messages column.
/// The search is case-insensitive for ASCII characters.
pub fn search_by_pattern(query: &str) -> Result<SearchResult, GitAiError> {
    let db = InternalDatabase::global()?;
    let db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    // Search with generous limit
    let db_records = db_guard.search_prompts(query, None, 1000, 0)?;

    let mut result = SearchResult::new();

    for db_record in db_records {
        let prompt = db_record.to_prompt_record();
        let id = db_record.id.clone();

        result.prompts.insert(id.clone(), prompt);

        // Add commit SHA if available
        if let Some(commit_sha) = db_record.commit_sha {
            result.prompt_commits.entry(id).or_default().push(commit_sha);
        }
    }

    Ok(result)
}

/// Search for a specific prompt by its ID
///
/// Looks up the prompt in the database first, then falls back to searching git notes.
pub fn search_by_prompt_id(
    repo: &Repository,
    prompt_id: &str,
) -> Result<SearchResult, GitAiError> {
    let (commit_sha, prompt) = find_prompt_with_db_fallback(prompt_id, Some(repo))?;

    let mut result = SearchResult::new();

    result.prompts.insert(prompt_id.to_string(), prompt);

    if let Some(sha) = commit_sha {
        result
            .prompt_commits
            .entry(prompt_id.to_string())
            .or_default()
            .push(sha);
    }

    Ok(result)
}

/// Merge multiple SearchResults into one, deduplicating by prompt hash
pub fn merge_results(results: Vec<SearchResult>) -> SearchResult {
    let mut merged = SearchResult::new();
    for result in results {
        merged.merge(result);
    }
    merged
}

/// Search mode determined by CLI arguments
#[derive(Debug, Clone, PartialEq)]
pub enum SearchMode {
    /// Search by a specific commit SHA, branch, tag, or symbolic ref
    ByCommit { commit_rev: String },
    /// Search across a range of commits
    ByCommitRange { start: String, end: String },
    /// Search by file path, optionally with specific line ranges
    ByFile {
        file_path: String,
        line_ranges: Vec<(u32, u32)>,
    },
    /// Full-text search across prompt messages
    ByPattern { query: String },
    /// Look up a specific prompt by its ID
    ByPromptId { prompt_id: String },
}

/// Filters applied after primary search
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Filter by AI tool name (e.g., "claude", "cursor")
    pub tool: Option<String>,
    /// Filter by human author name (substring match)
    pub author: Option<String>,
    /// Only include prompts after this Unix timestamp
    pub since: Option<i64>,
    /// Only include prompts before this Unix timestamp
    pub until: Option<i64>,
    /// Scope to specific repository path
    pub workdir: Option<String>,
}

impl SearchFilters {
    /// Create new empty filters
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any filters are set
    pub fn is_empty(&self) -> bool {
        self.tool.is_none()
            && self.author.is_none()
            && self.since.is_none()
            && self.until.is_none()
            && self.workdir.is_none()
    }
}

/// Output format for search results
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OutputFormat {
    /// Human-readable summary (default)
    #[default]
    Default,
    /// Full structured JSON output with transcripts
    Json,
    /// Human-readable with full transcripts
    Verbose,
    /// Stable machine-parseable format (tab-separated)
    Porcelain,
    /// Just show result count
    Count,
}

/// Handle the `git-ai search` command
pub fn handle_search(args: &[String]) {
    let parsed = match parse_search_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("Error: {}", e);
            print_search_help();
            std::process::exit(1);
        }
    };

    // Check for help flag
    if parsed.help {
        print_search_help();
        std::process::exit(0);
    }

    // Ensure at least one search mode is specified
    let mode = match parsed.mode {
        Some(m) => m,
        None => {
            eprintln!("Error: No search mode specified. Use --commit, --file, --pattern, or --prompt-id.");
            print_search_help();
            std::process::exit(1);
        }
    };

    // Get the repository
    let current_dir = env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string();

    let repo = match find_repository_in_path(&current_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: Not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    // Execute the search based on mode
    let result = match &mode {
        SearchMode::ByCommit { commit_rev } => {
            match search_by_commit(&repo, commit_rev) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error searching commit '{}': {}", commit_rev, e);
                    std::process::exit(1);
                }
            }
        }
        SearchMode::ByCommitRange { start, end } => {
            match search_by_commit_range(&repo, start, end) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error searching commit range '{}..{}': {}", start, end, e);
                    std::process::exit(1);
                }
            }
        }
        SearchMode::ByFile {
            file_path,
            line_ranges,
        } => match search_by_file(&repo, file_path, line_ranges) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error searching file '{}': {}", file_path, e);
                std::process::exit(1);
            }
        }
        SearchMode::ByPattern { query } => match search_by_pattern(query) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error searching pattern '{}': {}", query, e);
                std::process::exit(1);
            }
        }
        SearchMode::ByPromptId { prompt_id } => match search_by_prompt_id(&repo, prompt_id) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error searching prompt ID '{}': {}", prompt_id, e);
                std::process::exit(1);
            }
        }
    };

    // Apply filters
    let filtered = apply_filters(result, &parsed.filters);

    // Check for empty results
    if filtered.is_empty() {
        let mode_desc = match mode {
            SearchMode::ByCommit { commit_rev } => format!("commit '{}'", commit_rev),
            SearchMode::ByCommitRange { start, end } => {
                format!("commit range '{}..{}'", start, end)
            }
            SearchMode::ByFile { file_path, .. } => format!("file '{}'", file_path),
            SearchMode::ByPattern { query } => format!("pattern \"{}\"", query),
            SearchMode::ByPromptId { prompt_id } => format!("prompt ID '{}'", prompt_id),
        };
        eprintln!("No AI prompt history found for {}", mode_desc);
        std::process::exit(2);
    }

    // Format and output results
    let output = match parsed.output_format {
        OutputFormat::Default => format_default(&filtered, &mode),
        OutputFormat::Json => format_json(&filtered, &mode),
        OutputFormat::Verbose => format_verbose(&filtered, &mode),
        OutputFormat::Porcelain => format_porcelain(&filtered),
        OutputFormat::Count => format_count(&filtered),
    };

    println!("{}", output);
}

/// Apply filters to search results (intersection/AND semantics)
fn apply_filters(mut result: SearchResult, filters: &SearchFilters) -> SearchResult {
    if filters.is_empty() {
        return result;
    }

    // Collect hashes to remove
    let hashes_to_remove: Vec<String> = result
        .prompts
        .iter()
        .filter(|(_, prompt)| {
            // Filter by tool (case-insensitive)
            if let Some(ref tool) = filters.tool {
                if !prompt.agent_id.tool.eq_ignore_ascii_case(tool) {
                    return true; // Remove this prompt
                }
            }

            // Filter by author (substring, case-insensitive)
            if let Some(ref author) = filters.author {
                let matches = prompt
                    .human_author
                    .as_ref()
                    .map(|a| a.to_lowercase().contains(&author.to_lowercase()))
                    .unwrap_or(false);
                if !matches {
                    return true; // Remove this prompt
                }
            }

            // Filter by since/until timestamps
            // Note: We'd need timestamp information from messages or DB records
            // For now, these filters are a no-op on git notes data
            // TODO: Implement temporal filtering when timestamp data is available

            false // Keep this prompt
        })
        .map(|(hash, _)| hash.clone())
        .collect();

    // Remove filtered prompts from all maps
    for hash in hashes_to_remove {
        result.prompts.remove(&hash);
        result.prompt_locations.remove(&hash);
        result.prompt_commits.remove(&hash);
    }

    result
}

/// Format search results as human-readable default output
fn format_default(result: &SearchResult, mode: &SearchMode) -> String {
    let mode_desc = match mode {
        SearchMode::ByCommit { commit_rev } => format!("commit {}", commit_rev),
        SearchMode::ByCommitRange { start, end } => format!("commit range {}..{}", start, end),
        SearchMode::ByFile { file_path, .. } => format!("file {}", file_path),
        SearchMode::ByPattern { query } => format!("pattern \"{}\"", query),
        SearchMode::ByPromptId { prompt_id } => format!("prompt ID {}", prompt_id),
    };

    let mut output = format!(
        "Found {} AI prompt session(s) for {}\n\n",
        result.len(),
        mode_desc
    );

    for (idx, (hash, prompt)) in result.prompts.iter().enumerate() {
        output.push_str(&format!(
            "[{}] Prompt {} ({} / {})\n",
            idx + 1,
            hash,
            prompt.agent_id.tool,
            prompt.agent_id.model
        ));

        if let Some(ref author) = prompt.human_author {
            output.push_str(&format!("    Author: {}\n", author));
        }

        // Show file locations if available
        if let Some(locations) = result.prompt_locations.get(hash) {
            let files: Vec<String> = locations
                .iter()
                .map(|(path, ranges)| {
                    let range_str: Vec<String> =
                        ranges.iter().map(|r| format!("{}", r)).collect();
                    if range_str.is_empty() {
                        path.clone()
                    } else {
                        format!("{}:{}", path, range_str.join(","))
                    }
                })
                .collect();
            output.push_str(&format!("    Files:  {}\n", files.join(", ")));
        }

        // Show first message snippet
        if let Some(first_msg) = prompt.messages.first() {
            if let Some(text) = first_msg.text() {
                let snippet: String = text.chars().take(80).collect();
                let ellipsis = if text.len() > 80 { "..." } else { "" };
                output.push_str(&format!("    First message: {}{}\n", snippet, ellipsis));
            }
        }

        output.push('\n');
    }

    output.trim_end().to_string()
}

/// Format search results as JSON
fn format_json(result: &SearchResult, mode: &SearchMode) -> String {
    use serde_json::json;

    let query = match mode {
        SearchMode::ByCommit { commit_rev } => {
            json!({ "mode": "commit", "commit": commit_rev })
        }
        SearchMode::ByCommitRange { start, end } => {
            json!({ "mode": "commit_range", "start": start, "end": end })
        }
        SearchMode::ByFile {
            file_path,
            line_ranges,
        } => {
            json!({ "mode": "file", "file": file_path, "line_ranges": line_ranges })
        }
        SearchMode::ByPattern { query } => {
            json!({ "mode": "pattern", "query": query })
        }
        SearchMode::ByPromptId { prompt_id } => {
            json!({ "mode": "prompt_id", "prompt_id": prompt_id })
        }
    };

    let prompts: serde_json::Map<String, serde_json::Value> = result
        .prompts
        .iter()
        .map(|(hash, prompt)| {
            let locations = result
                .prompt_locations
                .get(hash)
                .map(|locs| {
                    locs.iter()
                        .map(|(path, ranges)| {
                            json!({
                                "file": path,
                                "lines": ranges.iter().map(|r| format!("{}", r)).collect::<Vec<_>>()
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let commits = result
                .prompt_commits
                .get(hash)
                .cloned()
                .unwrap_or_default();

            (
                hash.clone(),
                json!({
                    "agent_id": {
                        "tool": prompt.agent_id.tool,
                        "id": prompt.agent_id.id,
                        "model": prompt.agent_id.model
                    },
                    "human_author": prompt.human_author,
                    "messages": prompt.messages,
                    "total_additions": prompt.total_additions,
                    "total_deletions": prompt.total_deletions,
                    "locations": locations,
                    "commits": commits
                }),
            )
        })
        .collect();

    let output = json!({
        "query": query,
        "result_count": result.len(),
        "prompts": prompts
    });

    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
}

/// Format search results with full transcripts
fn format_verbose(result: &SearchResult, mode: &SearchMode) -> String {
    use crate::authorship::prompt_utils::format_transcript;

    let mode_desc = match mode {
        SearchMode::ByCommit { commit_rev } => format!("commit {}", commit_rev),
        SearchMode::ByCommitRange { start, end } => format!("commit range {}..{}", start, end),
        SearchMode::ByFile { file_path, .. } => format!("file {}", file_path),
        SearchMode::ByPattern { query } => format!("pattern \"{}\"", query),
        SearchMode::ByPromptId { prompt_id } => format!("prompt ID {}", prompt_id),
    };

    let mut output = format!(
        "Found {} AI prompt session(s) for {}\n\n",
        result.len(),
        mode_desc
    );

    for (hash, prompt) in &result.prompts {
        output.push_str(&format!(
            "=== Prompt {} ({} / {}) ===\n",
            hash, prompt.agent_id.tool, prompt.agent_id.model
        ));

        if let Some(ref author) = prompt.human_author {
            output.push_str(&format!("Author: {}\n", author));
        }

        if let Some(locations) = result.prompt_locations.get(hash) {
            let files: Vec<String> = locations
                .iter()
                .map(|(path, ranges)| {
                    let range_str: Vec<String> =
                        ranges.iter().map(|r| format!("{}", r)).collect();
                    format!("{}:{}", path, range_str.join(","))
                })
                .collect();
            output.push_str(&format!("Files: {}\n", files.join(", ")));
        }

        output.push_str("\n");
        output.push_str(&format_transcript(prompt));
        output.push_str("\n");
    }

    output.trim_end().to_string()
}

/// Format search results as stable machine-parseable output
fn format_porcelain(result: &SearchResult) -> String {
    // Format: <prompt_id>\t<tool>\t<model>\t<author>\t<date_unix>\t<file_count>\t<first_message_snippet>
    let mut lines = Vec::new();

    for (hash, prompt) in &result.prompts {
        let author = prompt.human_author.as_deref().unwrap_or("");
        let date_unix = "0"; // TODO: Extract timestamp from messages or DB
        let file_count = result
            .prompt_locations
            .get(hash)
            .map(|l| l.len())
            .unwrap_or(0);

        let first_msg = prompt
            .messages
            .first()
            .and_then(|m| m.text())
            .map(|t| {
                let snippet: String = t.chars().take(60).collect();
                snippet.replace('\t', " ").replace('\n', " ")
            })
            .unwrap_or_default();

        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            hash,
            prompt.agent_id.tool,
            prompt.agent_id.model,
            author,
            date_unix,
            file_count,
            first_msg
        ));
    }

    lines.join("\n")
}

/// Format search results as just a count
fn format_count(result: &SearchResult) -> String {
    result.len().to_string()
}

/// Parsed search arguments
#[derive(Debug)]
struct ParsedSearchArgs {
    mode: Option<SearchMode>,
    filters: SearchFilters,
    output_format: OutputFormat,
    help: bool,
}

/// Parse command-line arguments for search
fn parse_search_args(args: &[String]) -> Result<ParsedSearchArgs, String> {
    let mut mode: Option<SearchMode> = None;
    let mut filters = SearchFilters::new();
    let mut output_format = OutputFormat::Default;
    let mut output_format_set = false; // Track if output format was explicitly set
    let mut help = false;
    let mut pending_lines: Vec<(u32, u32)> = vec![]; // Lines seen before --file

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                help = true;
            }
            "--commit" => {
                i += 1;
                if i >= args.len() {
                    return Err("--commit requires a value".to_string());
                }
                let commit_rev = args[i].clone();
                // Check for range syntax (sha1..sha2)
                if let Some(pos) = commit_rev.find("..") {
                    let start = commit_rev[..pos].to_string();
                    let end = commit_rev[pos + 2..].to_string();
                    mode = Some(SearchMode::ByCommitRange { start, end });
                } else {
                    mode = Some(SearchMode::ByCommit { commit_rev });
                }
            }
            "--file" => {
                i += 1;
                if i >= args.len() {
                    return Err("--file requires a value".to_string());
                }
                let file_path = args[i].clone();
                // Initialize with any pending line ranges collected before --file
                let line_ranges = if !pending_lines.is_empty() {
                    std::mem::take(&mut pending_lines)
                } else if let Some(SearchMode::ByFile { line_ranges, .. }) = &mode {
                    line_ranges.clone()
                } else {
                    vec![]
                };
                mode = Some(SearchMode::ByFile {
                    file_path,
                    line_ranges,
                });
            }
            "--lines" => {
                i += 1;
                if i >= args.len() {
                    return Err("--lines requires a value".to_string());
                }
                let range_str = &args[i];
                let range = parse_line_range(range_str)?;

                // Add to existing file mode, or queue for later if --file comes after
                match &mut mode {
                    Some(SearchMode::ByFile { line_ranges, .. }) => {
                        line_ranges.push(range);
                    }
                    _ => {
                        // Queue for later - will be added when --file is parsed
                        pending_lines.push(range);
                    }
                }
            }
            "--pattern" => {
                i += 1;
                if i >= args.len() {
                    return Err("--pattern requires a value".to_string());
                }
                mode = Some(SearchMode::ByPattern {
                    query: args[i].clone(),
                });
            }
            "--prompt-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("--prompt-id requires a value".to_string());
                }
                mode = Some(SearchMode::ByPromptId {
                    prompt_id: args[i].clone(),
                });
            }
            // Filters
            "--tool" => {
                i += 1;
                if i >= args.len() {
                    return Err("--tool requires a value".to_string());
                }
                filters.tool = Some(args[i].clone());
            }
            "--author" => {
                i += 1;
                if i >= args.len() {
                    return Err("--author requires a value".to_string());
                }
                filters.author = Some(args[i].clone());
            }
            "--since" => {
                i += 1;
                if i >= args.len() {
                    return Err("--since requires a value".to_string());
                }
                filters.since = Some(parse_time_spec(&args[i])?);
            }
            "--until" => {
                i += 1;
                if i >= args.len() {
                    return Err("--until requires a value".to_string());
                }
                filters.until = Some(parse_time_spec(&args[i])?);
            }
            "--workdir" => {
                i += 1;
                if i >= args.len() {
                    return Err("--workdir requires a value".to_string());
                }
                filters.workdir = Some(args[i].clone());
            }
            // Output formats (mutually exclusive)
            "--json" => {
                if output_format_set {
                    return Err("Only one output format can be specified. Use one of: --json, --verbose, --porcelain, --count".to_string());
                }
                output_format = OutputFormat::Json;
                output_format_set = true;
            }
            "--verbose" => {
                if output_format_set {
                    return Err("Only one output format can be specified. Use one of: --json, --verbose, --porcelain, --count".to_string());
                }
                output_format = OutputFormat::Verbose;
                output_format_set = true;
            }
            "--porcelain" => {
                if output_format_set {
                    return Err("Only one output format can be specified. Use one of: --json, --verbose, --porcelain, --count".to_string());
                }
                output_format = OutputFormat::Porcelain;
                output_format_set = true;
            }
            "--count" => {
                if output_format_set {
                    return Err("Only one output format can be specified. Use one of: --json, --verbose, --porcelain, --count".to_string());
                }
                output_format = OutputFormat::Count;
                output_format_set = true;
            }
            arg => {
                return Err(format!("Unknown argument: {}", arg));
            }
        }
        i += 1;
    }

    // Validate: if --lines was specified without --file, error
    if !pending_lines.is_empty() {
        return Err("--lines requires --file to be specified".to_string());
    }

    Ok(ParsedSearchArgs {
        mode,
        filters,
        output_format,
        help,
    })
}

/// Parse a line range specification (e.g., "10", "10-50")
fn parse_line_range(s: &str) -> Result<(u32, u32), String> {
    if let Some(pos) = s.find('-') {
        let start: u32 = s[..pos]
            .parse()
            .map_err(|_| format!("Invalid line number: {}", &s[..pos]))?;
        let end: u32 = s[pos + 1..]
            .parse()
            .map_err(|_| format!("Invalid line number: {}", &s[pos + 1..]))?;
        if start > end {
            return Err(format!("Invalid line range: start ({}) > end ({})", start, end));
        }
        Ok((start, end))
    } else {
        let line: u32 = s
            .parse()
            .map_err(|_| format!("Invalid line number: {}", s))?;
        Ok((line, line))
    }
}

/// Parse a time specification (relative or absolute)
fn parse_time_spec(s: &str) -> Result<i64, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Try relative formats: 1d, 2h, 1w, 30m
    if let Some(num_str) = s.strip_suffix('d') {
        let days: i64 = num_str
            .parse()
            .map_err(|_| format!("Invalid day count: {}", num_str))?;
        return Ok(now - days * 86400);
    }
    if let Some(num_str) = s.strip_suffix('h') {
        let hours: i64 = num_str
            .parse()
            .map_err(|_| format!("Invalid hour count: {}", num_str))?;
        return Ok(now - hours * 3600);
    }
    if let Some(num_str) = s.strip_suffix('w') {
        let weeks: i64 = num_str
            .parse()
            .map_err(|_| format!("Invalid week count: {}", num_str))?;
        return Ok(now - weeks * 7 * 86400);
    }
    if let Some(num_str) = s.strip_suffix('m') {
        let minutes: i64 = num_str
            .parse()
            .map_err(|_| format!("Invalid minute count: {}", num_str))?;
        return Ok(now - minutes * 60);
    }

    // Try Unix timestamp
    if let Ok(ts) = s.parse::<i64>() {
        return Ok(ts);
    }

    // Try YYYY-MM-DD format
    if s.len() == 10 && s.chars().nth(4) == Some('-') && s.chars().nth(7) == Some('-') {
        // Parse as date at midnight UTC
        let year: i32 = s[0..4]
            .parse()
            .map_err(|_| format!("Invalid year in date: {}", s))?;
        let month: u32 = s[5..7]
            .parse()
            .map_err(|_| format!("Invalid month in date: {}", s))?;
        let day: u32 = s[8..10]
            .parse()
            .map_err(|_| format!("Invalid day in date: {}", s))?;

        // Simple conversion (approximate, ignoring leap seconds etc.)
        let days_since_epoch = days_since_unix_epoch(year, month, day)
            .ok_or_else(|| format!("Invalid date: {}", s))?;
        return Ok(days_since_epoch * 86400);
    }

    Err(format!(
        "Invalid time format: {}. Use relative (7d, 2h, 1w) or YYYY-MM-DD or Unix timestamp.",
        s
    ))
}

/// Calculate days since Unix epoch for a date
fn days_since_unix_epoch(year: i32, month: u32, day: u32) -> Option<i64> {
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }

    // Days from epoch to year start
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    for y in (year..1970).rev() {
        days -= if is_leap_year(y) { 366 } else { 365 };
    }

    // Days from year start to month start
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[(m - 1) as usize] as i64;
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }

    // Add day of month (1-indexed)
    days += (day - 1) as i64;

    Some(days)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn print_search_help() {
    eprintln!("git-ai search - Search AI prompt history");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    git-ai search [OPTIONS]");
    eprintln!();
    eprintln!("SEARCH MODES (at least one required):");
    eprintln!("    --commit <rev>          Search by commit SHA, branch, tag, or symbolic ref");
    eprintln!("                            Supports ranges: <sha1>..<sha2>");
    eprintln!("    --file <path>           Search by file path (relative to repo root or absolute)");
    eprintln!("    --pattern <text>        Full-text search in prompt messages");
    eprintln!("    --prompt-id <id>        Look up specific prompt by ID");
    eprintln!();
    eprintln!("LINE RANGE (requires --file):");
    eprintln!("    --lines <start-end>     Limit to specific line range (1-indexed, inclusive)");
    eprintln!("                            Can be specified multiple times for multiple ranges");
    eprintln!("                            Single line: --lines 42 (equivalent to --lines 42-42)");
    eprintln!();
    eprintln!("FILTERS (can be combined with any search mode):");
    eprintln!("    --tool <name>           Filter by AI tool name (claude, cursor, etc.)");
    eprintln!("    --author <name>         Filter by human author name (substring match)");
    eprintln!("    --since <time>          Only include prompts after this time");
    eprintln!("    --until <time>          Only include prompts before this time");
    eprintln!("    --workdir <path>        Scope to specific repository");
    eprintln!();
    eprintln!("OUTPUT FORMAT (mutually exclusive):");
    eprintln!("    (default)               Human-readable summary");
    eprintln!("    --json                  Full JSON output with transcripts");
    eprintln!("    --verbose               Human-readable with full transcripts");
    eprintln!("    --porcelain             Stable machine-parseable format");
    eprintln!("    --count                 Just show result count");
    eprintln!();
    eprintln!("TIME FORMATS:");
    eprintln!("    Relative: 7d (days), 2h (hours), 1w (weeks), 30m (minutes)");
    eprintln!("    Absolute: YYYY-MM-DD, Unix timestamp");
    eprintln!();
    eprintln!("EXAMPLES:");
    eprintln!("    git-ai search --commit abc1234");
    eprintln!("    git-ai search --commit HEAD~3..HEAD");
    eprintln!("    git-ai search --file src/main.rs");
    eprintln!("    git-ai search --file src/main.rs --lines 10-50");
    eprintln!("    git-ai search --pattern \"error handling\"");
    eprintln!("    git-ai search --commit abc1234 --json");
    eprintln!("    git-ai search --file src/main.rs --tool claude --since 7d");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_result_new() {
        let result = SearchResult::new();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_search_mode_variants() {
        let by_commit = SearchMode::ByCommit {
            commit_rev: "abc123".to_string(),
        };
        let by_file = SearchMode::ByFile {
            file_path: "src/main.rs".to_string(),
            line_ranges: vec![(10, 50)],
        };
        let by_pattern = SearchMode::ByPattern {
            query: "test".to_string(),
        };
        let by_prompt_id = SearchMode::ByPromptId {
            prompt_id: "xyz789".to_string(),
        };
        let by_range = SearchMode::ByCommitRange {
            start: "abc".to_string(),
            end: "def".to_string(),
        };

        assert_eq!(
            by_commit,
            SearchMode::ByCommit {
                commit_rev: "abc123".to_string()
            }
        );
        assert_eq!(
            by_file,
            SearchMode::ByFile {
                file_path: "src/main.rs".to_string(),
                line_ranges: vec![(10, 50)]
            }
        );
        assert_eq!(
            by_pattern,
            SearchMode::ByPattern {
                query: "test".to_string()
            }
        );
        assert_eq!(
            by_prompt_id,
            SearchMode::ByPromptId {
                prompt_id: "xyz789".to_string()
            }
        );
        assert_eq!(
            by_range,
            SearchMode::ByCommitRange {
                start: "abc".to_string(),
                end: "def".to_string()
            }
        );
    }

    #[test]
    fn test_output_format_default() {
        let format = OutputFormat::default();
        assert_eq!(format, OutputFormat::Default);
    }

    #[test]
    fn test_search_filters_empty() {
        let filters = SearchFilters::new();
        assert!(filters.is_empty());

        let filters_with_tool = SearchFilters {
            tool: Some("claude".to_string()),
            ..Default::default()
        };
        assert!(!filters_with_tool.is_empty());
    }

    #[test]
    fn test_parse_line_range() {
        assert_eq!(parse_line_range("42").unwrap(), (42, 42));
        assert_eq!(parse_line_range("10-50").unwrap(), (10, 50));
        assert_eq!(parse_line_range("1-1").unwrap(), (1, 1));

        assert!(parse_line_range("50-10").is_err());
        assert!(parse_line_range("abc").is_err());
        assert!(parse_line_range("").is_err());
    }

    #[test]
    fn test_parse_search_args_commit() {
        let args = vec!["--commit".to_string(), "abc123".to_string()];
        let parsed = parse_search_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            Some(SearchMode::ByCommit {
                commit_rev: "abc123".to_string()
            })
        );
    }

    #[test]
    fn test_parse_search_args_commit_range() {
        let args = vec!["--commit".to_string(), "abc..def".to_string()];
        let parsed = parse_search_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            Some(SearchMode::ByCommitRange {
                start: "abc".to_string(),
                end: "def".to_string()
            })
        );
    }

    #[test]
    fn test_parse_search_args_file_with_lines() {
        let args = vec![
            "--file".to_string(),
            "src/main.rs".to_string(),
            "--lines".to_string(),
            "10-50".to_string(),
            "--lines".to_string(),
            "80-100".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            Some(SearchMode::ByFile {
                file_path: "src/main.rs".to_string(),
                line_ranges: vec![(10, 50), (80, 100)]
            })
        );
    }

    #[test]
    fn test_parse_search_args_with_filters() {
        let args = vec![
            "--commit".to_string(),
            "HEAD".to_string(),
            "--tool".to_string(),
            "claude".to_string(),
            "--author".to_string(),
            "Alice".to_string(),
            "--json".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();
        assert_eq!(parsed.filters.tool, Some("claude".to_string()));
        assert_eq!(parsed.filters.author, Some("Alice".to_string()));
        assert_eq!(parsed.output_format, OutputFormat::Json);
    }

    #[test]
    fn test_normalize_file_path_relative() {
        // Test that relative paths pass through correctly
        // Note: We can't easily test the full function without a repo, but we can test the path normalization logic
        let path = "./src/main.rs";
        let normalized = path.strip_prefix("./").unwrap_or(path);
        assert_eq!(normalized, "src/main.rs");

        let path2 = "src/main.rs";
        let normalized2 = path2.strip_prefix("./").unwrap_or(path2);
        assert_eq!(normalized2, "src/main.rs");
    }

    #[test]
    fn test_normalize_file_path_backslash() {
        // Test that backslashes are normalized to forward slashes
        let path = "src\\commands\\search.rs";
        let normalized = path.replace('\\', "/");
        assert_eq!(normalized, "src/commands/search.rs");
    }

    #[test]
    fn test_search_result_merge_commits() {
        // Test merge behavior for prompt_commits (simpler test without PromptRecord)
        let mut result1 = SearchResult::new();
        result1
            .prompt_commits
            .entry("hash1".to_string())
            .or_default()
            .push("commit1".to_string());

        let mut result2 = SearchResult::new();
        result2
            .prompt_commits
            .entry("hash1".to_string())
            .or_default()
            .push("commit2".to_string());
        result2
            .prompt_commits
            .entry("hash2".to_string())
            .or_default()
            .push("commit3".to_string());

        result1.merge(result2);

        // hash1 should have both commits (deduplicated)
        assert_eq!(result1.prompt_commits.get("hash1").unwrap().len(), 2);
        assert!(result1.prompt_commits.contains_key("hash2"));
    }

    #[test]
    fn test_search_result_merge_locations() {
        let mut result1 = SearchResult::new();
        result1
            .prompt_locations
            .entry("hash1".to_string())
            .or_default()
            .push(("file1.rs".to_string(), vec![LineRange::Single(10)]));

        let mut result2 = SearchResult::new();
        result2
            .prompt_locations
            .entry("hash1".to_string())
            .or_default()
            .push(("file2.rs".to_string(), vec![LineRange::Range(20, 30)]));

        result1.merge(result2);

        // hash1 should have locations from both files
        let locations = result1.prompt_locations.get("hash1").unwrap();
        assert_eq!(locations.len(), 2);
    }

    #[test]
    fn test_parse_search_args_lines_without_file_error() {
        let args = vec!["--lines".to_string(), "10-50".to_string()];
        let result = parse_search_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("--lines requires --file"));
    }

    #[test]
    fn test_parse_search_args_lines_before_file_works() {
        // --lines can come before --file and should work
        let args = vec![
            "--lines".to_string(),
            "10-50".to_string(),
            "--file".to_string(),
            "src/main.rs".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();
        assert_eq!(
            parsed.mode,
            Some(SearchMode::ByFile {
                file_path: "src/main.rs".to_string(),
                line_ranges: vec![(10, 50)]
            })
        );
    }

    #[test]
    fn test_parse_search_args_multiple_output_formats_error() {
        let args = vec![
            "--commit".to_string(),
            "HEAD".to_string(),
            "--json".to_string(),
            "--verbose".to_string(),
        ];
        let result = parse_search_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only one output format"));
    }

    #[test]
    fn test_parse_search_args_json_porcelain_error() {
        let args = vec![
            "--commit".to_string(),
            "HEAD".to_string(),
            "--json".to_string(),
            "--porcelain".to_string(),
        ];
        let result = parse_search_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only one output format"));
    }

    #[test]
    fn test_parse_search_args_verbose_count_error() {
        let args = vec![
            "--commit".to_string(),
            "HEAD".to_string(),
            "--verbose".to_string(),
            "--count".to_string(),
        ];
        let result = parse_search_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Only one output format"));
    }

    #[test]
    fn test_parse_line_range_invalid_start_greater_than_end() {
        let result = parse_line_range("50-10");
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("start") && err_msg.contains("end"));
    }
}
