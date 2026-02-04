use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::commands::blame::GitAiBlameOptions;
use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal;

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug)]
pub enum DiffSpec {
    SingleCommit(String),      // SHA
    TwoCommit(String, String), // start..end
}

pub enum DiffFormat {
    Json,
    GitCompatibleTerminal,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct DiffHunk {
    pub file_path: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub deleted_lines: Vec<u32>, // Absolute line numbers in OLD file
    pub added_lines: Vec<u32>,   // Absolute line numbers in NEW file
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct DiffLineKey {
    pub file: String,
    pub line: u32,
    pub side: LineSide,
}

/// JSON output format for git-ai diff --json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffJson {
    /// Per-file diff information with annotations
    pub files: BTreeMap<String, FileDiffJson>,
    /// Prompt records keyed by prompt hash
    pub prompts: BTreeMap<String, PromptRecord>,
}

/// Per-file diff information in JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiffJson {
    /// Annotations mapping prompt hash to line ranges
    /// Line ranges are serialized as JSON tuples: [start, end] or single number
    #[serde(serialize_with = "serialize_annotations")]
    pub annotations: BTreeMap<String, Vec<LineRange>>,
    /// The unified diff for this file
    pub diff: String,
    /// The base content of the file (before changes)
    pub base_content: String,
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum LineSide {
    Old, // For deleted lines
    New, // For added lines
}

#[derive(Debug, Clone)]
pub enum Attribution {
    Ai(String),    // Tool name: "cursor", "claude", etc.
    Human(String), // Username
    NoData,        // No authorship data available
}

// ============================================================================
// Main Entry Point
// ============================================================================

pub fn handle_diff(repo: &Repository, args: &[String]) -> Result<(), GitAiError> {
    if args.is_empty() {
        eprintln!("Error: diff requires a commit or commit range argument");
        eprintln!("Usage: git-ai diff <commit>");
        eprintln!("       git-ai diff <commit1>..<commit2>");
        std::process::exit(1);
    }

    let (spec, format) = parse_diff_args(args)?;
    let output = execute_diff(repo, spec, format)?;
    print!("{}", output);

    Ok(())
}

// ============================================================================
// Argument Parsing
// ============================================================================

pub fn parse_diff_args(args: &[String]) -> Result<(DiffSpec, DiffFormat), GitAiError> {
    let arg = &args[0];

    let format = if args.iter().any(|arg| arg == "--json") {
        DiffFormat::Json
    } else {
        DiffFormat::GitCompatibleTerminal
    };

    // Check for commit range (start..end)
    if arg.contains("..") {
        let parts: Vec<&str> = arg.split("..").collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Ok((
                DiffSpec::TwoCommit(parts[0].to_string(), parts[1].to_string()),
                format,
            ));
        } else {
            return Err(GitAiError::Generic(
                "Invalid commit range format. Expected: <commit>..<commit>".to_string(),
            ));
        }
    }

    // Single commit
    Ok((DiffSpec::SingleCommit(arg.to_string()), format))
}

// ============================================================================
// Core Execution Logic
// ============================================================================

pub fn execute_diff(
    repo: &Repository,
    spec: DiffSpec,
    format: DiffFormat,
) -> Result<String, GitAiError> {
    // Resolve commits to get from/to SHAs
    let (from_commit, to_commit) = match spec {
        DiffSpec::TwoCommit(start, end) => {
            // Resolve both commits
            let from = resolve_commit(repo, &start)?;
            let to = resolve_commit(repo, &end)?;
            (from, to)
        }
        DiffSpec::SingleCommit(commit) => {
            // Resolve the commit and its parent
            let to = resolve_commit(repo, &commit)?;
            let from = resolve_parent(repo, &to)?;
            (from, to)
        }
    };

    // Step 1: Get diff hunks with line numbers
    let hunks = get_diff_with_line_numbers(repo, &from_commit, &to_commit)?;

    // Step 2: Overlay AI attributions
    let attributions = overlay_diff_attributions(repo, &from_commit, &to_commit, &hunks)?;

    // Step 3: Format and output annotated diff
    let output = match format {
        DiffFormat::Json => {
            let diff_json = build_diff_json(repo, &from_commit, &to_commit, &hunks, &attributions)?;
            serde_json::to_string(&diff_json)
                .map_err(|e| GitAiError::Generic(format!("Failed to serialize JSON: {}", e)))?
        }
        DiffFormat::GitCompatibleTerminal => {
            format_annotated_diff(repo, &from_commit, &to_commit, &attributions)?
        }
    };

    Ok(output)
}

// ============================================================================
// Commit Resolution
// ============================================================================

fn resolve_commit(repo: &Repository, rev: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push(rev.to_string());

    let output = exec_git(&args)?;
    let sha = String::from_utf8(output.stdout)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse rev-parse output: {}", e)))?
        .trim()
        .to_string();

    if sha.is_empty() {
        return Err(GitAiError::Generic(format!(
            "Could not resolve commit: {}",
            rev
        )));
    }

    Ok(sha)
}

fn resolve_parent(repo: &Repository, commit: &str) -> Result<String, GitAiError> {
    let parent_rev = format!("{}^", commit);

    // Try to resolve parent
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push(parent_rev);

    let output = exec_git(&args);

    match output {
        Ok(out) => {
            let sha = String::from_utf8(out.stdout)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse parent SHA: {}", e)))?
                .trim()
                .to_string();

            if sha.is_empty() {
                // No parent, this is initial commit - use empty tree
                Ok("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string())
            } else {
                Ok(sha)
            }
        }
        Err(_) => {
            // No parent, this is initial commit - use empty tree hash
            Ok("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string())
        }
    }
}

// ============================================================================
// Diff Retrieval with Line Numbers
// ============================================================================

pub fn get_diff_with_line_numbers(
    repo: &Repository,
    from: &str,
    to: &str,
) -> Result<Vec<DiffHunk>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("-U0".to_string()); // No context lines, just changes
    args.push("--no-color".to_string());
    args.push(from.to_string());
    args.push(to.to_string());

    let output = exec_git(&args)?;
    let diff_text = String::from_utf8(output.stdout)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse diff output: {}", e)))?;

    parse_diff_hunks(&diff_text)
}

fn parse_diff_hunks(diff_text: &str) -> Result<Vec<DiffHunk>, GitAiError> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();

    for line in diff_text.lines() {
        // Git outputs paths in two formats:
        // 1. Unquoted: +++ b/path/to/file.txt
        // 2. Quoted (for non-ASCII): +++ "b/path/to/file.txt" (with octal escapes inside)
        if let Some(raw_path) = line.strip_prefix("+++ b/") {
            // Unquoted path (ASCII only)
            // Note: Git adds trailing tab after filenames with spaces, so we trim_end
            current_file = crate::utils::unescape_git_path(raw_path.trim_end());
        } else if line.starts_with("+++ \"") {
            // Quoted path (non-ASCII chars) - unescape the entire quoted portion after "+++ "
            if let Some(quoted_suffix) = line.strip_prefix("+++ ") {
                let unescaped = crate::utils::unescape_git_path(quoted_suffix);
                // Now unescaped is "b/中文.txt", strip the "b/" prefix
                current_file = if let Some(stripped) = unescaped.strip_prefix("b/") {
                    stripped.to_string()
                } else {
                    unescaped
                };
            }
        } else if line.starts_with("@@ ") {
            // Hunk header
            if let Some(hunk) = parse_hunk_line(line, &current_file)? {
                hunks.push(hunk);
            }
        }
    }

    Ok(hunks)
}

fn parse_hunk_line(line: &str, file_path: &str) -> Result<Option<DiffHunk>, GitAiError> {
    // Parse hunk header format: @@ -old_start,old_count +new_start,new_count @@
    // Also handles: @@ -old_start +new_start,new_count @@ (single line deletion)
    // Also handles: @@ -old_start,old_count +new_start @@ (single line addition)

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return Ok(None);
    }

    let old_part = parts[1]; // e.g., "-10,3" or "-10"
    let new_part = parts[2]; // e.g., "+15,5" or "+15"

    // Parse old part
    let (old_start, old_count) = if let Some(old_str) = old_part.strip_prefix('-') {
        if let Some((start_str, count_str)) = old_str.split_once(',') {
            let start: u32 = start_str.parse().unwrap_or(0);
            let count: u32 = count_str.parse().unwrap_or(0);
            (start, count)
        } else {
            let start: u32 = old_str.parse().unwrap_or(0);
            (start, 1)
        }
    } else {
        (0, 0)
    };

    // Parse new part
    let (new_start, new_count) = if let Some(new_str) = new_part.strip_prefix('+') {
        if let Some((start_str, count_str)) = new_str.split_once(',') {
            let start: u32 = start_str.parse().unwrap_or(0);
            let count: u32 = count_str.parse().unwrap_or(0);
            (start, count)
        } else {
            let start: u32 = new_str.parse().unwrap_or(0);
            (start, 1)
        }
    } else {
        (0, 0)
    };

    // Build line number lists
    let deleted_lines: Vec<u32> = if old_count > 0 {
        (old_start..old_start + old_count).collect()
    } else {
        Vec::new()
    };

    let added_lines: Vec<u32> = if new_count > 0 {
        (new_start..new_start + new_count).collect()
    } else {
        Vec::new()
    };

    Ok(Some(DiffHunk {
        file_path: file_path.to_string(),
        old_start,
        old_count,
        new_start,
        new_count,
        deleted_lines,
        added_lines,
    }))
}

// ============================================================================
// Attribution Overlay
// ============================================================================

pub fn overlay_diff_attributions(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
    hunks: &[DiffHunk],
) -> Result<HashMap<DiffLineKey, Attribution>, GitAiError> {
    let mut attributions = HashMap::new();

    // Group added lines by file
    let mut lines_by_file: HashMap<String, Vec<u32>> = HashMap::new();
    for hunk in hunks {
        if !hunk.added_lines.is_empty() {
            lines_by_file
                .entry(hunk.file_path.clone())
                .or_default()
                .extend(&hunk.added_lines);
        }
    }

    // For each file, call blame with the appropriate line ranges
    for (file_path, mut lines) in lines_by_file {
        // Sort and convert to contiguous ranges for efficient -L format
        lines.sort_unstable();
        lines.dedup();
        let line_ranges = lines_to_ranges(&lines);

        if line_ranges.is_empty() {
            continue;
        }

        // Build blame options
        let mut options = GitAiBlameOptions::default();
        #[allow(clippy::field_reassign_with_default)]
        {
        options.oldest_commit = Some(from_commit.to_string());
        options.newest_commit = Some(to_commit.to_string());
        options.line_ranges = line_ranges;
        options.no_output = true;
        }

        // Call blame to get attributions
        let blame_result = repo.blame(&file_path, &options);

        match blame_result {
            Ok((line_authors, prompt_records)) => {
                // Map blame results to Attribution enum
                for line in &lines {
                    if let Some(author) = line_authors.get(line) {
                        // Check if this author is an AI tool by looking up in prompt_records
                        let attribution = if prompt_records
                            .values()
                            .any(|pr| &pr.agent_id.tool == author)
                        {
                            Attribution::Ai(author.clone())
                        } else {
                            Attribution::Human(author.clone())
                        };

                        let key = DiffLineKey {
                            file: file_path.clone(),
                            line: *line,
                            side: LineSide::New,
                        };
                        attributions.insert(key, attribution);
                    } else {
                        // No blame data for this line
                        let key = DiffLineKey {
                            file: file_path.clone(),
                            line: *line,
                            side: LineSide::New,
                        };
                        attributions.insert(key, Attribution::NoData);
                    }
                }
            }
            Err(_) => {
                // Blame failed, mark all lines as NoData
                for line in &lines {
                    let key = DiffLineKey {
                        file: file_path.clone(),
                        line: *line,
                        side: LineSide::New,
                    };
                    attributions.insert(key, Attribution::NoData);
                }
            }
        }
    }

    Ok(attributions)
}

/// Convert a sorted list of line numbers to contiguous ranges
/// e.g., [1, 2, 3, 5, 6, 10] -> [(1, 3), (5, 6), (10, 10)]
fn lines_to_ranges(lines: &[u32]) -> Vec<(u32, u32)> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = lines[0];
    let mut end = lines[0];

    for &line in &lines[1..] {
        if line == end + 1 {
            // Contiguous, extend the range
            end = line;
        } else {
            // Gap found, save current range and start new one
            ranges.push((start, end));
            start = line;
            end = line;
        }
    }

    // Don't forget the last range
    ranges.push((start, end));

    ranges
}

// ============================================================================
// JSON Output Building
// ============================================================================

/// Build the DiffJson structure for --json output
fn build_diff_json(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
    hunks: &[DiffHunk],
    _attributions: &HashMap<DiffLineKey, Attribution>,
) -> Result<DiffJson, GitAiError> {
    let mut files: BTreeMap<String, FileDiffJson> = BTreeMap::new();
    let mut all_prompts: BTreeMap<String, PromptRecord> = BTreeMap::new();

    // Get the full diff output and split by file
    let file_diffs = get_diff_split_by_file(repo, from_commit, to_commit)?;

    // Get unique files from hunks
    let mut unique_files: Vec<String> = hunks.iter().map(|h| h.file_path.clone()).collect();
    unique_files.sort();
    unique_files.dedup();

    // For each file, collect annotations, diff, and base content
    for file_path in &unique_files {
        // Get annotations for this file (lines attributed to AI prompts)
        let file_annotations =
            collect_file_annotations(repo, from_commit, to_commit, file_path, hunks)?;

        // Merge prompt records into the global map
        for (hash, prompt_record) in &file_annotations.1 {
            all_prompts.insert(hash.clone(), prompt_record.clone());
        }

        // Get the diff for this file
        let diff = file_diffs.get(file_path).cloned().unwrap_or_default();

        // Get base content (file content at from_commit)
        let base_content = match repo.get_file_content(file_path, from_commit) {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
            Err(_) => String::new(), // File didn't exist in from_commit (new file)
        };

        files.insert(
            file_path.clone(),
            FileDiffJson {
                annotations: file_annotations.0,
                diff,
                base_content,
            },
        );
    }

    Ok(DiffJson {
        files,
        prompts: all_prompts,
    })
}

/// Get the unified diff split by file path
fn get_diff_split_by_file(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
) -> Result<HashMap<String, String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--no-color".to_string());
    args.push(from_commit.to_string());
    args.push(to_commit.to_string());

    let output = exec_git(&args)?;
    let diff_text = String::from_utf8(output.stdout)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse diff output: {}", e)))?;

    let mut file_diffs: HashMap<String, String> = HashMap::new();
    let mut current_file = String::new();
    let mut current_diff = String::new();

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            // Save previous file's diff if any
            if !current_file.is_empty() && !current_diff.is_empty() {
                file_diffs.insert(current_file.clone(), current_diff.clone());
            }
            current_diff = format!("{}\n", line);
            current_file.clear();
        } else if let Some(raw_path) = line.strip_prefix("+++ b/") {
            // Unquoted path (ASCII only)
            // Note: Git adds trailing tab after filenames with spaces, so we trim_end
            current_file = crate::utils::unescape_git_path(raw_path.trim_end());
            current_diff.push_str(line);
            current_diff.push('\n');
        } else if line.starts_with("+++ \"") {
            // Quoted path (non-ASCII chars) - unescape the entire quoted portion after "+++ "
            if let Some(quoted_suffix) = line.strip_prefix("+++ ") {
                let unescaped = crate::utils::unescape_git_path(quoted_suffix);
                // Now unescaped is "b/中文.txt", strip the "b/" prefix
                current_file = if let Some(stripped) = unescaped.strip_prefix("b/") {
                    stripped.to_string()
                } else {
                    unescaped
                };
            }
            current_diff.push_str(line);
            current_diff.push('\n');
        } else {
            current_diff.push_str(line);
            current_diff.push('\n');
        }
    }

    // Don't forget the last file
    if !current_file.is_empty() && !current_diff.is_empty() {
        file_diffs.insert(current_file, current_diff);
    }

    Ok(file_diffs)
}

/// Collect annotations for a specific file, returning (annotations_map, prompt_records_map)
#[allow(clippy::type_complexity)]
fn collect_file_annotations(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
    file_path: &str,
    hunks: &[DiffHunk],
) -> Result<
    (
        BTreeMap<String, Vec<LineRange>>,
        HashMap<String, PromptRecord>,
    ),
    GitAiError,
> {
    let mut annotations: BTreeMap<String, Vec<LineRange>> = BTreeMap::new();
    let mut prompt_records: HashMap<String, PromptRecord> = HashMap::new();

    // Collect all added lines for this file
    let mut added_lines: Vec<u32> = Vec::new();
    for hunk in hunks {
        if hunk.file_path == file_path {
            added_lines.extend(&hunk.added_lines);
        }
    }

    if added_lines.is_empty() {
        return Ok((annotations, prompt_records));
    }

    added_lines.sort_unstable();
    added_lines.dedup();
    let line_ranges = lines_to_ranges(&added_lines);

    if line_ranges.is_empty() {
        return Ok((annotations, prompt_records));
    }

    // Build blame options - use prompt hashes as names to get the actual hash per line
    let mut options = GitAiBlameOptions::default();
    #[allow(clippy::field_reassign_with_default)]
    {
    options.oldest_commit = Some(from_commit.to_string());
    options.newest_commit = Some(to_commit.to_string());
    options.line_ranges = line_ranges;
    options.no_output = true;
    options.use_prompt_hashes_as_names = true; // Key: get prompt hash instead of tool name
    }

    // Call blame to get attributions
    let blame_result = repo.blame(file_path, &options);

    match blame_result {
        Ok((line_authors, blame_prompt_records)) => {
            // Group lines by prompt hash
            // With use_prompt_hashes_as_names=true, line_authors values are the prompt hashes
            let mut lines_by_hash: HashMap<String, Vec<u32>> = HashMap::new();

            for &line in &added_lines {
                if let Some(prompt_hash) = line_authors.get(&line) {
                    // Only include if this hash is in the prompt_records (i.e., it's an AI line)
                    if blame_prompt_records.contains_key(prompt_hash) {
                        lines_by_hash
                            .entry(prompt_hash.clone())
                            .or_default()
                            .push(line);
                    }
                }
            }

            // Convert lines to LineRange format and store in annotations
            for (hash, mut lines) in lines_by_hash {
                lines.sort_unstable();
                lines.dedup();
                let ranges = LineRange::compress_lines(&lines);
                annotations.insert(hash, ranges);
            }

            // Store prompt records
            prompt_records = blame_prompt_records;
        }
        Err(_) => {
            // Blame failed, no annotations for this file
        }
    }

    Ok((annotations, prompt_records))
}

// ============================================================================
// Output Formatting
// ============================================================================

#[allow(clippy::if_same_then_else)]
pub fn format_annotated_diff(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
    attributions: &HashMap<DiffLineKey, Attribution>,
) -> Result<String, GitAiError> {
    // Execute git diff with normal context
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--no-color".to_string());
    args.push(from_commit.to_string());
    args.push(to_commit.to_string());

    let output = exec_git(&args)?;
    let diff_text = String::from_utf8(output.stdout)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse diff output: {}", e)))?;

    // Check if we should use colors
    let use_color = std::io::stdout().is_terminal();

    // Parse and annotate diff
    let mut result = String::new();
    let mut current_file = String::new();
    let mut old_line_num = 0u32;
    let mut new_line_num = 0u32;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            // Diff header
            result.push_str(&format_line(line, LineType::DiffHeader, use_color, None));
            current_file.clear();
            old_line_num = 0;
            new_line_num = 0;
        } else if line.starts_with("index ") {
            result.push_str(&format_line(line, LineType::DiffHeader, use_color, None));
        } else if line.starts_with("--- ") {
            result.push_str(&format_line(line, LineType::DiffHeader, use_color, None));
        } else if let Some(raw_path) = line.strip_prefix("+++ b/") {
            // Unquoted path (ASCII only)
            // Note: Git adds trailing tab after filenames with spaces, so we trim_end
            current_file = crate::utils::unescape_git_path(raw_path.trim_end());
            result.push_str(&format_line(line, LineType::DiffHeader, use_color, None));
        } else if line.starts_with("+++ \"") {
            // Quoted path (non-ASCII chars) - unescape the entire quoted portion after "+++ "
            if let Some(quoted_suffix) = line.strip_prefix("+++ ") {
                let unescaped = crate::utils::unescape_git_path(quoted_suffix);
                // Now unescaped is "b/中文.txt", strip the "b/" prefix
                current_file = if let Some(stripped) = unescaped.strip_prefix("b/") {
                    stripped.to_string()
                } else {
                    unescaped
                };
            }
            result.push_str(&format_line(line, LineType::DiffHeader, use_color, None));
        } else if line.starts_with("@@ ") {
            // Hunk header - update line counters
            if let Some((old_start, new_start)) = parse_hunk_header_for_line_nums(line) {
                old_line_num = old_start;
                new_line_num = new_start;
            }
            result.push_str(&format_line(line, LineType::HunkHeader, use_color, None));
        } else if line.starts_with('-') && !line.starts_with("---") {
            // Deleted line
            let key = DiffLineKey {
                file: current_file.clone(),
                line: old_line_num,
                side: LineSide::Old,
            };
            let attribution = attributions.get(&key);
            result.push_str(&format_line(
                line,
                LineType::Deletion,
                use_color,
                attribution,
            ));
            old_line_num += 1;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            // Added line
            let key = DiffLineKey {
                file: current_file.clone(),
                line: new_line_num,
                side: LineSide::New,
            };
            let attribution = attributions.get(&key);
            result.push_str(&format_line(
                line,
                LineType::Addition,
                use_color,
                attribution,
            ));
            new_line_num += 1;
        } else if line.starts_with(' ') {
            // Context line
            result.push_str(&format_line(line, LineType::Context, use_color, None));
            old_line_num += 1;
            new_line_num += 1;
        } else if line.starts_with("Binary files") {
            // Binary file marker
            result.push_str(&format_line(line, LineType::Binary, use_color, None));
        } else {
            // Other lines (e.g., "\ No newline at end of file")
            result.push_str(&format_line(line, LineType::Context, use_color, None));
        }
    }

    Ok(result)
}

fn parse_hunk_header_for_line_nums(line: &str) -> Option<(u32, u32)> {
    // Parse @@ -old_start,old_count +new_start,new_count @@
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    let old_part = parts[1];
    let new_part = parts[2];

    // Extract old_start
    let old_start = if let Some(old_str) = old_part.strip_prefix('-') {
        if let Some((start_str, _)) = old_str.split_once(',') {
            start_str.parse::<u32>().ok()?
        } else {
            old_str.parse::<u32>().ok()?
        }
    } else {
        return None;
    };

    // Extract new_start
    let new_start = if let Some(new_str) = new_part.strip_prefix('+') {
        if let Some((start_str, _)) = new_str.split_once(',') {
            start_str.parse::<u32>().ok()?
        } else {
            new_str.parse::<u32>().ok()?
        }
    } else {
        return None;
    };

    Some((old_start, new_start))
}

#[derive(Debug)]
enum LineType {
    DiffHeader,
    HunkHeader,
    Addition,
    Deletion,
    Context,
    Binary,
}

fn format_line(
    line: &str,
    line_type: LineType,
    use_color: bool,
    attribution: Option<&Attribution>,
) -> String {
    let annotation = if let Some(attr) = attribution {
        format_attribution(attr)
    } else {
        String::new()
    };

    if use_color {
        match line_type {
            LineType::DiffHeader => {
                format!("\x1b[1m{}\x1b[0m\n", line) // Bold
            }
            LineType::HunkHeader => {
                format!("\x1b[36m{}\x1b[0m\n", line) // Cyan
            }
            LineType::Addition => {
                if annotation.is_empty() {
                    format!("\x1b[32m{}\x1b[0m\n", line) // Green
                } else {
                    format!("\x1b[32m{}\x1b[0m  \x1b[2m{}\x1b[0m\n", line, annotation) // Green + dim annotation
                }
            }
            LineType::Deletion => {
                if annotation.is_empty() {
                    format!("\x1b[31m{}\x1b[0m\n", line) // Red
                } else {
                    format!("\x1b[31m{}\x1b[0m  \x1b[2m{}\x1b[0m\n", line, annotation) // Red + dim annotation
                }
            }
            LineType::Context | LineType::Binary => {
                format!("{}\n", line)
            }
        }
    } else {
        // No color
        if annotation.is_empty() {
            format!("{}\n", line)
        } else {
            format!("{}  {}\n", line, annotation)
        }
    }
}

fn format_attribution(attribution: &Attribution) -> String {
    match attribution {
        Attribution::Ai(tool) => format!("🤖{}", tool),
        Attribution::Human(username) => format!("👤{}", username),
        Attribution::NoData => "[no-data]".to_string(),
    }
}

/// Custom serializer for annotations that converts LineRange to JSON tuples
fn serialize_annotations<S>(
    annotations: &BTreeMap<String, Vec<LineRange>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(annotations.len()))?;
    for (key, ranges) in annotations {
        let json_ranges: Vec<serde_json::Value> = ranges
            .iter()
            .map(|range| match range {
                LineRange::Single(line) => serde_json::Value::Number((*line).into()),
                LineRange::Range(start, end) => serde_json::Value::Array(vec![
                    serde_json::Value::Number((*start).into()),
                    serde_json::Value::Number((*end).into()),
                ]),
            })
            .collect();
        map.serialize_entry(key, &json_ranges)?;
    }
    map.end()
}

// ============================================================================
// Filtered Diff for Bundle Sharing
// ============================================================================

/// Options for getting a diff with optional filtering
#[derive(Default)]
pub struct DiffOptions {
    /// If provided, only include files with attributions from these prompts
    pub prompt_ids: Option<Vec<String>>,
    /// Whether to filter files to only those with attributions from prompt_ids
    pub filter_to_attributed_files: bool,
}


/// Get diff JSON for a single commit with optional filtering by prompt attributions
///
/// This function is designed for bundle sharing:
/// - If `options.filter_to_attributed_files` is true, only includes files that have
///   attributions from the specified `prompt_ids`
/// - If `options.prompt_ids` is Some, filters the returned prompts to only those IDs
pub fn get_diff_json_filtered(
    repo: &Repository,
    commit_sha: &str,
    options: DiffOptions,
) -> Result<DiffJson, GitAiError> {
    // Resolve the commit to get from/to SHAs (parent -> commit)
    let to_commit = resolve_commit(repo, commit_sha)?;
    let from_commit = resolve_parent(repo, &to_commit)?;

    // Get diff hunks with line numbers
    let hunks = get_diff_with_line_numbers(repo, &from_commit, &to_commit)?;

    // Get attributions for overlay (not used directly, but needed for build_diff_json)
    let attributions = overlay_diff_attributions(repo, &from_commit, &to_commit, &hunks)?;

    // Build the full DiffJson structure
    let mut diff_json = build_diff_json(repo, &from_commit, &to_commit, &hunks, &attributions)?;

    // Apply filtering if requested
    if options.filter_to_attributed_files
        && let Some(ref prompt_ids) = options.prompt_ids {
            let prompt_id_set: std::collections::HashSet<&String> = prompt_ids.iter().collect();

            // Filter files to only those with attributions from the specified prompts
            diff_json.files.retain(|_file_path, file_diff| {
                // Check if any annotation key matches a prompt_id
                file_diff.annotations.keys().any(|key| prompt_id_set.contains(key))
            });
        }

    // Filter prompts to only those specified (if any)
    if let Some(ref prompt_ids) = options.prompt_ids {
        let prompt_id_set: std::collections::HashSet<&String> = prompt_ids.iter().collect();
        diff_json.prompts.retain(|key, _| prompt_id_set.contains(key));
    }

    Ok(diff_json)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_args_single_commit() {
        let args = vec!["abc123".to_string()];
        let (spec, _format) = parse_diff_args(&args).unwrap();

        match spec {
            DiffSpec::SingleCommit(sha) => {
                assert_eq!(sha, "abc123");
            }
            _ => panic!("Expected SingleCommit"),
        }
    }

    #[test]
    fn test_parse_diff_args_commit_range() {
        let args = vec!["abc123..def456".to_string()];
        let (spec, _format) = parse_diff_args(&args).unwrap();

        match spec {
            DiffSpec::TwoCommit(start, end) => {
                assert_eq!(start, "abc123");
                assert_eq!(end, "def456");
            }
            _ => panic!("Expected TwoCommit"),
        }
    }

    #[test]
    fn test_parse_diff_args_invalid_range() {
        let args = vec!["..".to_string()];
        let result = parse_diff_args(&args);
        assert!(result.is_err());

        let args = vec!["abc..".to_string()];
        let result = parse_diff_args(&args);
        assert!(result.is_err());

        let args = vec!["..def".to_string()];
        let result = parse_diff_args(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_hunk_line_basic() {
        let line = "@@ -10,3 +15,5 @@ fn main() {";
        let result = parse_hunk_line(line, "test.rs").unwrap().unwrap();

        assert_eq!(result.file_path, "test.rs");
        assert_eq!(result.old_start, 10);
        assert_eq!(result.old_count, 3);
        assert_eq!(result.new_start, 15);
        assert_eq!(result.new_count, 5);
        assert_eq!(result.deleted_lines, vec![10, 11, 12]);
        assert_eq!(result.added_lines, vec![15, 16, 17, 18, 19]);
    }

    #[test]
    fn test_parse_hunk_line_single_line_deletion() {
        let line = "@@ -10 +10,2 @@ fn main() {";
        let result = parse_hunk_line(line, "test.rs").unwrap().unwrap();

        assert_eq!(result.old_start, 10);
        assert_eq!(result.old_count, 1);
        assert_eq!(result.new_start, 10);
        assert_eq!(result.new_count, 2);
        assert_eq!(result.deleted_lines, vec![10]);
        assert_eq!(result.added_lines, vec![10, 11]);
    }

    #[test]
    fn test_parse_hunk_line_single_line_addition() {
        let line = "@@ -10,2 +10 @@ fn main() {";
        let result = parse_hunk_line(line, "test.rs").unwrap().unwrap();

        assert_eq!(result.old_start, 10);
        assert_eq!(result.old_count, 2);
        assert_eq!(result.new_start, 10);
        assert_eq!(result.new_count, 1);
        assert_eq!(result.deleted_lines, vec![10, 11]);
        assert_eq!(result.added_lines, vec![10]);
    }

    #[test]
    fn test_parse_hunk_line_pure_addition() {
        let line = "@@ -0,0 +1,3 @@ fn main() {";
        let result = parse_hunk_line(line, "test.rs").unwrap().unwrap();

        assert_eq!(result.old_start, 0);
        assert_eq!(result.old_count, 0);
        assert_eq!(result.new_start, 1);
        assert_eq!(result.new_count, 3);
        assert_eq!(result.deleted_lines.len(), 0);
        assert_eq!(result.added_lines, vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_hunk_line_pure_deletion() {
        let line = "@@ -5,3 +0,0 @@ fn main() {";
        let result = parse_hunk_line(line, "test.rs").unwrap().unwrap();

        assert_eq!(result.old_start, 5);
        assert_eq!(result.old_count, 3);
        assert_eq!(result.new_start, 0);
        assert_eq!(result.new_count, 0);
        assert_eq!(result.deleted_lines, vec![5, 6, 7]);
        assert_eq!(result.added_lines.len(), 0);
    }

    #[test]
    fn test_parse_hunk_header_for_line_nums() {
        let line = "@@ -10,5 +20,3 @@ context";
        let result = parse_hunk_header_for_line_nums(line).unwrap();
        assert_eq!(result, (10, 20));
    }

    #[test]
    fn test_parse_hunk_header_for_line_nums_single_line() {
        let line = "@@ -10 +20,3 @@ context";
        let result = parse_hunk_header_for_line_nums(line).unwrap();
        assert_eq!(result, (10, 20));

        let line = "@@ -10,5 +20 @@ context";
        let result = parse_hunk_header_for_line_nums(line).unwrap();
        assert_eq!(result, (10, 20));
    }

    #[test]
    fn test_parse_hunk_header_for_line_nums_invalid() {
        let line = "not a hunk header";
        let result = parse_hunk_header_for_line_nums(line);
        assert!(result.is_none());

        let line = "@@ invalid @@";
        let result = parse_hunk_header_for_line_nums(line);
        assert!(result.is_none());
    }

    #[test]
    fn test_format_attribution_ai() {
        let attr = Attribution::Ai("cursor".to_string());
        assert_eq!(format_attribution(&attr), "🤖cursor");

        let attr = Attribution::Ai("claude".to_string());
        assert_eq!(format_attribution(&attr), "🤖claude");
    }

    #[test]
    fn test_format_attribution_human() {
        let attr = Attribution::Human("alice".to_string());
        assert_eq!(format_attribution(&attr), "👤alice");

        let attr = Attribution::Human("bob@example.com".to_string());
        assert_eq!(format_attribution(&attr), "👤bob@example.com");
    }

    #[test]
    fn test_format_attribution_no_data() {
        let attr = Attribution::NoData;
        assert_eq!(format_attribution(&attr), "[no-data]");
    }

    #[test]
    fn test_diff_line_key_equality() {
        let key1 = DiffLineKey {
            file: "test.rs".to_string(),
            line: 10,
            side: LineSide::Old,
        };

        let key2 = DiffLineKey {
            file: "test.rs".to_string(),
            line: 10,
            side: LineSide::Old,
        };

        let key3 = DiffLineKey {
            file: "test.rs".to_string(),
            line: 10,
            side: LineSide::New,
        };

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_parse_diff_hunks_multiple_files() {
        let diff_text = r#"diff --git a/file1.rs b/file1.rs
index abc123..def456 100644
--- a/file1.rs
+++ b/file1.rs
@@ -10,2 +10,3 @@ fn main() {
diff --git a/file2.rs b/file2.rs
index 111222..333444 100644
--- a/file2.rs
+++ b/file2.rs
@@ -5,1 +5,2 @@ fn test() {
"#;

        let result = parse_diff_hunks(diff_text).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].file_path, "file1.rs");
        assert_eq!(result[1].file_path, "file2.rs");
    }

    #[test]
    fn test_parse_diff_hunks_empty() {
        let diff_text = "";
        let result = parse_diff_hunks(diff_text).unwrap();
        assert_eq!(result.len(), 0);
    }
}
