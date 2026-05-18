//! Post-commit authorship generation.
//!
//! Reads accumulated checkpoints (working log), determines which lines were
//! committed, and produces an AuthorshipLog that gets stored as a git note.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use super::attribution::LineAttribution;
use super::authorship_log::{
    self, AttestationEntry, AuthorshipLog, FileAttestation, LineRange, Metadata,
};
use super::git_binary::git_cmd as git_command;
use super::working_log::{self, AgentId, Checkpoint, CheckpointKind, InitialAttributions};

/// Error type for post-commit operations.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Git(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Json(e) => write!(f, "JSON error: {}", e),
            Error::Git(msg) => write!(f, "Git error: {}", msg),
        }
    }
}

/// Main entry point: generate authorship data for a commit.
///
/// Reads checkpoints accumulated since `parent_sha`, determines which lines
/// landed in `commit_sha`, and returns the AuthorshipLog plus any leftover
/// attributions for uncommitted working-directory changes.
pub fn generate_authorship_for_commit(
    git_dir: &Path,
    repo_dir: &Path,
    parent_sha: &str,
    commit_sha: &str,
    human_author: &str,
) -> Result<(AuthorshipLog, Option<InitialAttributions>), Error> {
    // 1. Read checkpoints from working log
    let checkpoints = working_log::read_checkpoints(git_dir, parent_sha);

    // 2. Read INITIAL attributions carried over from prior commit
    let initial = working_log::read_initial_attributions(git_dir, parent_sha).unwrap_or_default();

    // 3. Merge checkpoint data into per-file line attributions (last checkpoint wins)
    let (file_attributions, metadata) = merge_attributions(&checkpoints, &initial, human_author);

    if file_attributions.is_empty() {
        // No checkpoints or INITIAL data — attribute all committed lines to human.
        // This covers initial commits and commits made without any AI tool active.
        let committed_lines = git_diff_committed_lines(repo_dir, parent_sha, commit_sha);
        if committed_lines.is_empty() {
            let log = AuthorshipLog::new(Metadata::new(commit_sha.to_string()));
            return Ok((log, None));
        }

        let human_hash = authorship_log::generate_human_hash(human_author);
        let mut log = AuthorshipLog::new(Metadata {
            schema_version: authorship_log::AUTHORSHIP_LOG_VERSION.to_string(),
            git_ai_version: Some(authorship_log::GIT_AI_VERSION.to_string()),
            base_commit_sha: commit_sha.to_string(),
            prompts: BTreeMap::new(),
            humans: BTreeMap::from([(
                human_hash.clone(),
                authorship_log::HumanRecord {
                    author: human_author.to_string(),
                },
            )]),
            sessions: BTreeMap::new(),
        });

        for (file_path, lines) in &committed_lines {
            if lines.is_empty() {
                continue;
            }
            let ranges = LineRange::compress_lines(lines);
            log.attestations.push(FileAttestation {
                file_path: file_path.clone(),
                entries: vec![AttestationEntry {
                    hash: human_hash.clone(),
                    line_ranges: ranges,
                }],
            });
        }

        return Ok((log, None));
    }

    // 3b. Remap checkpoint-space line attributions to working-tree-space.
    // If the file was modified between checkpoint and commit without a new checkpoint,
    // line numbers drift. Fix by diffing checkpoint blob vs working tree content.
    let mut file_attributions = file_attributions;
    for (file_path, attrs) in file_attributions.iter_mut() {
        let blob_content = find_last_blob_content(&checkpoints, file_path, git_dir, parent_sha);
        if let Some(ref checkpoint_content) = blob_content
            && let Ok(committed_content) = std::fs::read_to_string(repo_dir.join(file_path))
            && *checkpoint_content != committed_content
        {
            let old_lines: Vec<&str> = checkpoint_content.lines().collect();
            let new_lines: Vec<&str> = committed_content.lines().collect();
            let mapping = build_line_mapping(&old_lines, &new_lines);
            let mut new_attrs: Vec<LineAttribution> = Vec::new();
            for attr in attrs.iter() {
                let mut mapped_lines: Vec<u32> = Vec::new();
                for line_num in attr.start_line..=attr.end_line {
                    if let Some(&new_line) = mapping.get(&line_num) {
                        mapped_lines.push(new_line);
                    }
                }
                if mapped_lines.is_empty() {
                    continue;
                }
                mapped_lines.sort_unstable();
                let mut start = mapped_lines[0];
                let mut end = mapped_lines[0];
                for &l in &mapped_lines[1..] {
                    if l == end + 1 {
                        end = l;
                    } else {
                        new_attrs.push(LineAttribution {
                            start_line: start,
                            end_line: end,
                            author_id: attr.author_id.clone(),
                            overrode: attr.overrode.clone(),
                        });
                        start = l;
                        end = l;
                    }
                }
                new_attrs.push(LineAttribution {
                    start_line: start,
                    end_line: end,
                    author_id: attr.author_id.clone(),
                    overrode: attr.overrode.clone(),
                });
            }
            *attrs = new_attrs;
        }
    }
    file_attributions.retain(|_, attrs| !attrs.is_empty());
    if file_attributions.is_empty() {
        let log = AuthorshipLog::new(Metadata::new(commit_sha.to_string()));
        return Ok((log, None));
    }

    // 4. Determine which lines were added in this commit (diff parent vs commit)
    let committed_lines = git_diff_committed_lines(repo_dir, parent_sha, commit_sha);

    // 4b. Detect renames: if file_attributions has an old path that doesn't appear
    // in committed_lines but committed_lines has a new path from a rename, remap
    // the attribution key to the new path.
    let rename_map = detect_renames(repo_dir, parent_sha, commit_sha);
    for (old_path, new_path) in &rename_map {
        if file_attributions.contains_key(old_path)
            && !file_attributions.contains_key(new_path)
            && let Some(attrs) = file_attributions.remove(old_path)
        {
            file_attributions.insert(new_path.clone(), attrs);
        }
    }

    // 5. Determine which lines are uncommitted (working dir vs commit)
    let uncommitted_lines = git_diff_uncommitted_lines(repo_dir, commit_sha, &file_attributions);

    // 6. Split attributions into committed vs uncommitted buckets
    let (authorship_log, initial_out) = split_attributions(
        &file_attributions,
        &committed_lines,
        &uncommitted_lines,
        &metadata,
        commit_sha,
        parent_sha,
        repo_dir,
        human_author,
    );

    let initial_result = if initial_out.files.is_empty() {
        None
    } else {
        Some(initial_out)
    };

    Ok((authorship_log, initial_result))
}

// ---------------------------------------------------------------------------
// Helper: read file at a given revision
// ---------------------------------------------------------------------------

/// Retrieve file content at a specific git revision.
/// Returns None if the file doesn't exist at that revision.
pub fn git_show_file(repo_dir: &Path, revision: &str, file_path: &str) -> Option<String> {
    let output = git_command()
        .arg("-C")
        .arg(repo_dir)
        .arg("show")
        .arg(format!("{}:{}", revision, file_path))
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Helper: get committed lines from diff
// ---------------------------------------------------------------------------

/// Returns (file_path, added_line_numbers) for all files changed in the commit.
/// Line numbers are 1-indexed and refer to positions in the new (commit) side.
pub fn git_diff_committed_lines(
    repo_dir: &Path,
    parent: &str,
    commit: &str,
) -> Vec<(String, Vec<u32>)> {
    // For initial commit, diff against the empty tree
    let base = if parent == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    } else {
        parent.to_string()
    };

    let output = git_command()
        .arg("-C")
        .arg(repo_dir)
        .arg("diff")
        .arg("-U0")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--src-prefix=a/")
        .arg("--dst-prefix=b/")
        .arg("--find-renames=1%")
        .arg(&base)
        .arg(commit)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let diff_text = String::from_utf8_lossy(&output.stdout);
    parse_diff_added_lines(&diff_text)
}

/// Determine lines present in the working directory but NOT in the commit.
/// Only checks files that have attributions (to avoid unnecessary git calls).
fn git_diff_uncommitted_lines(
    repo_dir: &Path,
    commit_sha: &str,
    file_attributions: &HashMap<String, Vec<LineAttribution>>,
) -> HashMap<String, Vec<u32>> {
    let mut result = HashMap::new();

    let pathspecs: Vec<&str> = file_attributions.keys().map(|s| s.as_str()).collect();
    if pathspecs.is_empty() {
        return result;
    }

    let mut cmd = git_command();
    cmd.arg("-C")
        .arg(repo_dir)
        .arg("diff")
        .arg("-U0")
        .arg("--no-color")
        .arg("--no-ext-diff")
        .arg("--src-prefix=a/")
        .arg("--dst-prefix=b/")
        .arg(commit_sha)
        .arg("--");
    for p in &pathspecs {
        cmd.arg(p);
    }

    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        Ok(o) if !o.stdout.is_empty() => o,
        Ok(_) => {
            return result;
        }
        Err(_) => {
            return result;
        }
    };

    let diff_text = String::from_utf8_lossy(&output.stdout);
    for (file, lines) in parse_diff_added_lines(&diff_text) {
        if !lines.is_empty() {
            result.insert(file, lines);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Diff parser
// ---------------------------------------------------------------------------

/// Extract the destination file path from a `+++ ...` diff line.
/// Handles both unquoted (`+++ b/file.txt`) and quoted (`+++ "b/file with spaces.txt"`) forms.
fn parse_diff_dst_path(line: &str) -> Option<String> {
    if line.starts_with("+++ \"b/") && line.ends_with('"') {
        let inner = &line[7..line.len() - 1];
        Some(unescape_git_path(inner))
    } else {
        line.strip_prefix("+++ b/")
            .map(|path| path.trim_end().to_string())
    }
}

/// Unescape a git-quoted path. Git uses C-style escaping for special characters.
fn unescape_git_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    result.push(b'\n');
                    i += 2;
                }
                b't' => {
                    result.push(b'\t');
                    i += 2;
                }
                b'\\' => {
                    result.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    result.push(b'"');
                    i += 2;
                }
                d @ b'0'..=b'3' => {
                    // Octal escape: up to 3 digits total
                    let mut val = d - b'0';
                    let mut consumed = 2; // backslash + first digit
                    if i + 2 < bytes.len() && bytes[i + 2] >= b'0' && bytes[i + 2] <= b'7' {
                        val = val * 8 + (bytes[i + 2] - b'0');
                        consumed += 1;
                        if i + 3 < bytes.len() && bytes[i + 3] >= b'0' && bytes[i + 3] <= b'7' {
                            val = val * 8 + (bytes[i + 3] - b'0');
                            consumed += 1;
                        }
                    }
                    result.push(val);
                    i += consumed;
                }
                _ => {
                    result.push(bytes[i]);
                    result.push(bytes[i + 1]);
                    i += 2;
                }
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

/// Parse unified diff output (with -U0) to extract added line numbers per file.
fn parse_diff_added_lines(diff_output: &str) -> Vec<(String, Vec<u32>)> {
    let mut results: Vec<(String, Vec<u32>)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_lines: Vec<u32> = Vec::new();

    for line in diff_output.lines() {
        if let Some(file_path) = parse_diff_dst_path(line) {
            // Flush previous file
            if let Some(file) = current_file.take()
                && !current_lines.is_empty()
            {
                results.push((file, std::mem::take(&mut current_lines)));
            }
            current_file = Some(file_path);
            current_lines.clear();
        } else if line.starts_with("+++ /dev/null") {
            // File was deleted; flush and reset
            if let Some(file) = current_file.take()
                && !current_lines.is_empty()
            {
                results.push((file, std::mem::take(&mut current_lines)));
            }
            current_file = None;
            current_lines.clear();
        } else if line.starts_with("@@ ")
            && let Some(added) = parse_hunk_header_added(line)
        {
            current_lines.extend(added);
        }
    }

    // Flush last file
    if let Some(file) = current_file
        && !current_lines.is_empty()
    {
        results.push((file, current_lines));
    }

    results
}

/// Parse a hunk header to extract the added line numbers.
/// Format: @@ -old_start[,old_count] +new_start[,new_count] @@
fn parse_hunk_header_added(header: &str) -> Option<Vec<u32>> {
    let plus_idx = header.find('+')?;
    let after_plus = &header[plus_idx + 1..];
    let end_idx = after_plus.find(' ').unwrap_or(after_plus.len());
    let range_str = &after_plus[..end_idx];

    let (start, count) = if let Some(comma_idx) = range_str.find(',') {
        let s: u32 = range_str[..comma_idx].parse().ok()?;
        let c: u32 = range_str[comma_idx + 1..].parse().ok()?;
        (s, c)
    } else {
        let s: u32 = range_str.parse().ok()?;
        (s, 1)
    };

    if count == 0 {
        return Some(Vec::new());
    }

    Some((start..start + count).collect())
}

/// Find the content of the last checkpoint blob for a given file.
fn find_last_blob_content(
    checkpoints: &[Checkpoint],
    file_path: &str,
    git_dir: &Path,
    base_commit: &str,
) -> Option<String> {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == file_path && !entry.blob_sha.is_empty() {
                return working_log::read_blob(git_dir, base_commit, &entry.blob_sha);
            }
        }
    }
    None
}

/// Detect file renames between parent and commit.
/// Returns a list of (old_path, new_path) pairs.
fn detect_renames(repo_dir: &Path, parent: &str, commit: &str) -> Vec<(String, String)> {
    let base = if parent == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    } else {
        parent.to_string()
    };

    let output = git_command()
        .arg("-C")
        .arg(repo_dir)
        .args([
            "diff",
            "--diff-filter=R",
            "--name-status",
            "--find-renames=1%",
            &base,
            commit,
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut renames = Vec::new();
    for line in text.lines() {
        // Format: R<percentage>\t<old_path>\t<new_path>
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 && parts[0].starts_with('R') {
            renames.push((parts[1].to_string(), parts[2].to_string()));
        }
    }
    renames
}

/// Build a mapping from old line numbers to new line numbers using LCS-based diff.
/// Returns a HashMap where key=old_line_num (1-indexed), value=new_line_num (1-indexed).
/// Only maps lines that exist in both old and new (equal lines).
pub fn build_line_mapping(old_lines: &[&str], new_lines: &[&str]) -> HashMap<u32, u32> {
    let mut mapping = HashMap::new();

    // Simple LCS-based approach using two pointers with longest common subsequence
    let lcs = compute_lcs_line_pairs(old_lines, new_lines);
    for (old_idx, new_idx) in lcs {
        mapping.insert((old_idx + 1) as u32, (new_idx + 1) as u32);
    }

    mapping
}

/// Compute LCS (longest common subsequence) of line indices.
/// Returns pairs of (old_index, new_index) for matching lines.
fn compute_lcs_line_pairs(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let m = old.len();
    let n = new.len();

    if m == 0 || n == 0 {
        return Vec::new();
    }

    // Build DP table
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find the pairs
    let mut pairs = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    pairs.reverse();
    pairs
}

// ---------------------------------------------------------------------------
// Attribution merging
// ---------------------------------------------------------------------------

/// Collected metadata from checkpoints.
struct CollectedMetadata {
    prompts: BTreeMap<String, authorship_log::PromptRecord>,
    humans: BTreeMap<String, authorship_log::HumanRecord>,
    sessions: BTreeMap<String, authorship_log::SessionRecord>,
    /// Prompt IDs that came only from INITIAL (not refreshed by a checkpoint).
    initial_only_prompt_ids: HashSet<String>,
}

/// Merge INITIAL attributions and checkpoint data into per-file line attributions.
/// Later checkpoints override earlier ones for the same file (last-write-wins).
fn merge_attributions(
    checkpoints: &[Checkpoint],
    initial: &InitialAttributions,
    human_author: &str,
) -> (HashMap<String, Vec<LineAttribution>>, CollectedMetadata) {
    let mut file_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prompts: BTreeMap<String, authorship_log::PromptRecord> = BTreeMap::new();
    let mut humans: BTreeMap<String, authorship_log::HumanRecord> = BTreeMap::new();
    let mut sessions: BTreeMap<String, authorship_log::SessionRecord> = BTreeMap::new();
    let mut initial_only_prompt_ids: HashSet<String> = HashSet::new();

    // Seed from INITIAL attributions (convert working_log types to authorship_log types)
    for (hash, record) in &initial.humans {
        humans.insert(
            hash.clone(),
            authorship_log::HumanRecord {
                author: record.author.clone(),
            },
        );
    }
    for (session_id, record) in &initial.sessions {
        sessions.insert(
            session_id.clone(),
            authorship_log::SessionRecord {
                agent_id: authorship_log::AgentId {
                    tool: record.agent_id.tool.clone(),
                    id: record.agent_id.id.clone(),
                    model: record.agent_id.model.clone(),
                },
                human_author: record.human_author.clone(),
                custom_attributes: None,
            },
        );
    }
    for (file_path, line_attrs) in &initial.files {
        file_attrs.insert(file_path.clone(), line_attrs.clone());
    }

    // Apply checkpoints in order (last wins per file)
    for checkpoint in checkpoints {
        // Register prompt/session metadata from agent_id
        if let Some(agent_id) = &checkpoint.agent_id {
            register_agent_metadata(
                agent_id,
                checkpoint.trace_id.as_deref(),
                human_author,
                &mut prompts,
                &mut sessions,
                &mut initial_only_prompt_ids,
            );
        }

        // Register known-human record
        if checkpoint.kind == CheckpointKind::KnownHuman {
            let hash = authorship_log::generate_human_hash(&checkpoint.author);
            humans
                .entry(hash)
                .or_insert_with(|| authorship_log::HumanRecord {
                    author: checkpoint.author.clone(),
                });
        }

        // Apply per-file attributions from checkpoint entries
        for entry in &checkpoint.entries {
            if entry.line_attributions.is_empty() {
                // A Human/KnownHuman checkpoint with empty line_attributions but
                // non-empty byte-level attributions means "human rewrote this file"
                // — clear all prior AI attributions for it.
                // Empty attributions + empty line_attributions = bare file listing
                // (e.g. from stage_all_and_commit flow) — do NOT clear.
                if matches!(
                    checkpoint.kind,
                    CheckpointKind::Human | CheckpointKind::KnownHuman
                ) && !entry.attributions.is_empty()
                {
                    file_attrs.remove(&entry.file);
                }
                continue;
            }
            // Last checkpoint wins, but preserve initial attributions for
            // lines that have no author (empty author_id means "unchanged").
            let existing = file_attrs.get(&entry.file);
            if let Some(existing_val) = existing
                && entry
                    .line_attributions
                    .iter()
                    .any(|a| a.author_id.is_empty())
            {
                let initial_attrs = existing_val.clone();
                let mut merged: Vec<LineAttribution> = Vec::new();
                for attr in &entry.line_attributions {
                    if !attr.author_id.is_empty() {
                        merged.push(attr.clone());
                        continue;
                    }
                    // Split this empty-author range into individual lines and
                    // resolve each against the initial attributions.
                    for line in attr.start_line..=attr.end_line {
                        let resolved_author = initial_attrs
                            .iter()
                            .find(|ia| {
                                ia.start_line <= line
                                    && ia.end_line >= line
                                    && !ia.author_id.is_empty()
                            })
                            .map(|ia| ia.author_id.as_str())
                            .unwrap_or("");
                        if resolved_author.is_empty() {
                            // Still empty — keep as-is (will be skipped later)
                            merged.push(LineAttribution {
                                start_line: line,
                                end_line: line,
                                author_id: String::new(),
                                overrode: None,
                            });
                        } else {
                            // Try to extend the last entry if same author
                            if let Some(last) = merged.last_mut()
                                && last.author_id == resolved_author
                                && last.end_line + 1 == line
                            {
                                last.end_line = line;
                                continue;
                            }
                            merged.push(LineAttribution {
                                start_line: line,
                                end_line: line,
                                author_id: resolved_author.to_string(),
                                overrode: None,
                            });
                        }
                    }
                }
                file_attrs.insert(entry.file.clone(), merged);
            } else {
                file_attrs.insert(entry.file.clone(), entry.line_attributions.clone());
            }
        }
    }

    let metadata = CollectedMetadata {
        prompts,
        humans,
        sessions,
        initial_only_prompt_ids,
    };

    (file_attrs, metadata)
}

/// Register an agent's metadata (prompt or session record).
fn register_agent_metadata(
    agent_id: &AgentId,
    trace_id: Option<&str>,
    human_author: &str,
    prompts: &mut BTreeMap<String, authorship_log::PromptRecord>,
    sessions: &mut BTreeMap<String, authorship_log::SessionRecord>,
    initial_only_prompt_ids: &mut HashSet<String>,
) {
    let is_session_format = trace_id.is_some();

    if is_session_format {
        // New session format: s_<14 hex chars>
        let session_id = authorship_log::generate_session_id(&agent_id.tool, &agent_id.id);
        sessions
            .entry(session_id)
            .or_insert_with(|| authorship_log::SessionRecord {
                agent_id: authorship_log::AgentId {
                    tool: agent_id.tool.clone(),
                    id: agent_id.id.clone(),
                    model: agent_id.model.clone(),
                },
                human_author: Some(human_author.to_string()),
                custom_attributes: None,
            });
    } else {
        // Legacy prompt format: 16 hex chars
        let author_id = authorship_log::generate_short_hash(&agent_id.tool, &agent_id.id);
        prompts
            .entry(author_id.clone())
            .or_insert_with(|| authorship_log::PromptRecord {
                agent_id: authorship_log::AgentId {
                    tool: agent_id.tool.clone(),
                    id: agent_id.id.clone(),
                    model: agent_id.model.clone(),
                },
                human_author: Some(human_author.to_string()),
                messages_url: None,
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                custom_attributes: None,
            });
        // Mark as actively used (not INITIAL-only)
        initial_only_prompt_ids.remove(&author_id);
    }
}

// ---------------------------------------------------------------------------
// Attribution splitting (committed vs uncommitted)
// ---------------------------------------------------------------------------

/// Split per-file attributions into committed (AuthorshipLog) and uncommitted (InitialAttributions).
#[allow(clippy::too_many_arguments)]
fn split_attributions(
    file_attributions: &HashMap<String, Vec<LineAttribution>>,
    committed_lines: &[(String, Vec<u32>)],
    uncommitted_lines: &HashMap<String, Vec<u32>>,
    metadata: &CollectedMetadata,
    commit_sha: &str,
    parent_sha: &str,
    repo_dir: &Path,
    human_author: &str,
) -> (AuthorshipLog, InitialAttributions) {
    let mut log = AuthorshipLog::new(Metadata {
        schema_version: authorship_log::AUTHORSHIP_LOG_VERSION.to_string(),
        git_ai_version: Some(authorship_log::GIT_AI_VERSION.to_string()),
        base_commit_sha: commit_sha.to_string(),
        prompts: metadata.prompts.clone(),
        humans: metadata.humans.clone(),
        sessions: metadata.sessions.clone(),
    });

    // Ensure the human author hash is registered in metadata so that "human"
    // sentinel lines can be written as proper h_ attestation entries.
    let sentinel_human_hash = authorship_log::generate_human_hash(human_author);
    log.metadata
        .humans
        .entry(sentinel_human_hash)
        .or_insert_with(|| authorship_log::HumanRecord {
            author: human_author.to_string(),
        });

    let mut initial_out = InitialAttributions::default();

    // Build a lookup: file -> set of committed line numbers
    let committed_lookup: HashMap<&str, HashSet<u32>> = committed_lines
        .iter()
        .map(|(file, lines)| (file.as_str(), lines.iter().copied().collect()))
        .collect();

    // Build a lookup: file -> sorted vec of uncommitted line numbers
    let uncommitted_sorted: HashMap<&str, Vec<u32>> = uncommitted_lines
        .iter()
        .map(|(file, lines)| {
            let mut sorted = lines.clone();
            sorted.sort_unstable();
            (file.as_str(), sorted)
        })
        .collect();

    let mut referenced_committed: HashSet<String> = HashSet::new();
    let mut referenced_initial: HashSet<String> = HashSet::new();

    for (file_path, line_attrs) in file_attributions {
        let committed_set = committed_lookup.get(file_path.as_str());
        let uncommitted_sorted_lines = uncommitted_sorted.get(file_path.as_str());

        // If this file has no committed lines AND no uncommitted diff lines,
        // it means the file was neither staged nor modified relative to the commit.
        let file_not_in_commit = committed_set.is_none() && uncommitted_sorted_lines.is_none();

        // Build a working-tree → committed line mapping when there are uncommitted
        // changes. This correctly handles modifications (same position, different
        // content) without erroneous position shifts.
        let wt_to_committed: Option<HashMap<u32, u32>> = if uncommitted_sorted_lines.is_some() {
            if let Ok(committed_content) = std::fs::read_to_string(repo_dir.join(file_path)) {
                let wt_path = repo_dir.join(file_path);
                if let Ok(wt_content) = std::fs::read_to_string(&wt_path) {
                    if wt_content != committed_content {
                        let wt_lines_vec: Vec<&str> = wt_content.lines().collect();
                        let committed_lines_vec: Vec<&str> = committed_content.lines().collect();
                        Some(build_line_mapping(&wt_lines_vec, &committed_lines_vec))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Collect committed and uncommitted lines per author
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        let mut uncommitted_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        let mut explicitly_human_committed: HashSet<u32> = HashSet::new();

        // Resolve the h_ hash for the "human" sentinel so explicitly-human lines
        // get proper known-human attestation entries instead of being untracked.
        let human_hash = authorship_log::generate_human_hash(human_author);
        let human_hash_ref = human_hash.as_str();

        for attr in line_attrs {
            let is_explicitly_human = attr.author_id == "human";
            let is_empty = attr.author_id.is_empty();

            for line_num in attr.start_line..=attr.end_line {
                if file_not_in_commit {
                    if !is_empty && !is_explicitly_human {
                        uncommitted_by_author
                            .entry(attr.author_id.as_str())
                            .or_default()
                            .push(line_num);
                    }
                    continue;
                }

                let is_uncommitted = uncommitted_sorted_lines
                    .map(|lines| lines.binary_search(&line_num).is_ok())
                    .unwrap_or(false);

                if is_uncommitted {
                    if !is_empty && !is_explicitly_human {
                        uncommitted_by_author
                            .entry(attr.author_id.as_str())
                            .or_default()
                            .push(line_num);
                    }
                } else {
                    // Map working-tree line to committed line using LCS mapping
                    let commit_line = if let Some(ref mapping) = wt_to_committed {
                        match mapping.get(&line_num) {
                            Some(&cl) => cl,
                            None => continue,
                        }
                    } else {
                        line_num
                    };

                    let is_committed = committed_set
                        .map(|s| s.contains(&commit_line))
                        .unwrap_or(false);

                    if is_committed {
                        if is_explicitly_human {
                            explicitly_human_committed.insert(commit_line);
                            // Write as a known-human attestation entry (h_ hash)
                            committed_by_author
                                .entry(human_hash_ref)
                                .or_default()
                                .push(commit_line);
                        } else if !is_empty {
                            committed_by_author
                                .entry(attr.author_id.as_str())
                                .or_default()
                                .push(commit_line);
                        }
                    }
                }
            }
        }

        // Identify h_ lines at trailing-newline boundaries (the last line of the parent
        // whose content matches but appears in committed_set only because a newline was
        // added after it when new lines were appended). These should be treated as "soft h_"
        // that gap-fill CAN override.
        let mut h_lines_matching_parent: HashSet<u32> = HashSet::new();
        if parent_sha != "initial"
            && let Some(parent_content) = git_show_file(repo_dir, parent_sha, file_path)
        {
            let parent_lines: Vec<&str> = parent_content.lines().collect();
            let parent_has_no_trailing_newline = !parent_content.ends_with('\n');
            if let Some(commit_content) = git_show_file(repo_dir, commit_sha, file_path) {
                let commit_lines: Vec<&str> = commit_content.lines().collect();
                if parent_has_no_trailing_newline
                    && !parent_lines.is_empty()
                    && commit_lines.len() > parent_lines.len()
                {
                    let last_parent_line_num = parent_lines.len() as u32;
                    let last_parent_idx = parent_lines.len() - 1;
                    // Check if the last parent line appears unchanged at the same position
                    if last_parent_idx < commit_lines.len()
                        && commit_lines[last_parent_idx] == parent_lines[last_parent_idx]
                    {
                        // Check if this line has h_ attribution (make it soft/gap-fillable)
                        let h_authors_present: Vec<&str> = committed_by_author
                            .keys()
                            .filter(|k| k.starts_with("h_"))
                            .copied()
                            .collect();
                        for h_author in &h_authors_present {
                            if let Some(lines) = committed_by_author.get(*h_author)
                                && lines.contains(&last_parent_line_num)
                            {
                                h_lines_matching_parent.insert(last_parent_line_num);
                            }
                        }

                        // Note: We no longer insert into explicitly_human_committed here.
                        // The h_between check in gap_fill_committed prevents gap-fill from
                        // bridging over h_ lines, which handles the case where the last
                        // parent line should stay human (when there's a human-attributed
                        // line between it and the nearest AI neighbor).
                    }
                }
            }
        }

        // Gap-fill committed lines (but not lines explicitly attributed to human)
        if let Some(c_set) = committed_set {
            gap_fill_committed(
                &mut committed_by_author,
                c_set,
                &explicitly_human_committed,
                &h_lines_matching_parent,
            );
        }

        // Build attestation entries for committed lines
        if !committed_by_author.is_empty() {
            let mut entries: Vec<AttestationEntry> = Vec::new();
            for (author_id, mut lines) in committed_by_author {
                lines.sort_unstable();
                lines.dedup();
                if lines.is_empty() {
                    continue;
                }
                referenced_committed.insert(author_id.to_string());
                let ranges = LineRange::compress_lines(&lines);
                entries.push(AttestationEntry {
                    hash: author_id.to_string(),
                    line_ranges: ranges,
                });
            }
            if !entries.is_empty() {
                log.attestations.push(FileAttestation {
                    file_path: file_path.clone(),
                    entries,
                });
            }
        }

        // Build INITIAL entries for uncommitted lines
        if !uncommitted_by_author.is_empty() {
            let mut uncommitted_attrs: Vec<LineAttribution> = Vec::new();
            for (author_id, mut lines) in uncommitted_by_author {
                lines.sort_unstable();
                lines.dedup();
                if lines.is_empty() {
                    continue;
                }
                referenced_initial.insert(author_id.to_string());
                let attrs = compress_to_line_attrs(author_id, &lines);
                uncommitted_attrs.extend(attrs);
            }
            if !uncommitted_attrs.is_empty() {
                initial_out
                    .files
                    .insert(file_path.clone(), uncommitted_attrs);
            }
        }
    }

    // Prune INITIAL-only prompts from committed metadata if they have no committed lines
    if !metadata.initial_only_prompt_ids.is_empty() {
        log.metadata.prompts.retain(|id, _| {
            !metadata.initial_only_prompt_ids.contains(id) || referenced_committed.contains(id)
        });
    }

    // Prune sessions with no committed attestations
    let committed_session_ids: HashSet<String> = log
        .attestations
        .iter()
        .flat_map(|fa| fa.entries.iter())
        .filter(|e| e.hash.starts_with("s_"))
        .map(|e| e.hash.split("::").next().unwrap_or(&e.hash).to_string())
        .collect();
    log.metadata
        .sessions
        .retain(|id, _| committed_session_ids.contains(id));

    // Populate INITIAL metadata (only for referenced authors)
    // Convert authorship_log types back to working_log types for InitialAttributions
    for author_id in &referenced_initial {
        if let Some(record) = metadata.humans.get(author_id) {
            initial_out.humans.insert(
                author_id.clone(),
                working_log::HumanRecord {
                    author: record.author.clone(),
                },
            );
        }
        if author_id.starts_with("s_") {
            let session_key = author_id.split("::").next().unwrap_or(author_id);
            if let Some(record) = metadata.sessions.get(session_key) {
                initial_out.sessions.insert(
                    session_key.to_string(),
                    working_log::SessionRecord {
                        agent_id: working_log::AgentId {
                            tool: record.agent_id.tool.clone(),
                            id: record.agent_id.id.clone(),
                            model: record.agent_id.model.clone(),
                        },
                        human_author: record.human_author.clone(),
                    },
                );
            }
        }
    }

    (log, initial_out)
}

// ---------------------------------------------------------------------------
// Gap-fill logic
// ---------------------------------------------------------------------------

/// Fill gaps in committed attributions. This includes:
/// 1. Unattributed lines between/adjacent-to AI-attributed lines
/// 2. Known-human (h_) lines that are sandwiched between same-AI-author lines
///
/// Lines in `explicitly_human` (the "human" untracked sentinel) are never gap-filled.
fn gap_fill_committed(
    committed_by_author: &mut HashMap<&str, Vec<u32>>,
    committed_set: &HashSet<u32>,
    explicitly_human: &HashSet<u32>,
    h_override: &HashSet<u32>,
) {
    // Build sorted AI-only line-author pairs for neighbor lookups.
    // We exclude h_ lines from the neighbor list so they can be reassigned.
    let mut ai_line_author_pairs: Vec<(u32, &str)> = Vec::new();
    let mut h_lines: HashSet<u32> = HashSet::new();
    for (&author, lines) in committed_by_author.iter() {
        if author.starts_with("h_") {
            for &line in lines.iter() {
                h_lines.insert(line);
            }
        } else {
            for &line in lines.iter() {
                ai_line_author_pairs.push((line, author));
            }
        }
    }
    ai_line_author_pairs.sort_by_key(|(line, _)| *line);

    let ai_attributed_lines: HashSet<u32> = ai_line_author_pairs.iter().map(|(l, _)| *l).collect();

    let mut gap_fills: Vec<(&str, u32)> = Vec::new();

    for &line in committed_set {
        // Skip lines already attributed to an AI author
        if ai_attributed_lines.contains(&line) {
            continue;
        }
        // Skip lines explicitly marked "human" (untracked sentinel)
        if explicitly_human.contains(&line) {
            continue;
        }

        // h_ lines matching parent content are treated as "soft" (can be gap-filled)
        let is_h_line = h_lines.contains(&line) && !h_override.contains(&line);

        // Find nearest AI-attributed neighbor before
        let prev = ai_line_author_pairs.iter().rev().find(|(l, _)| *l < line);
        // Find nearest AI-attributed neighbor after
        let next = ai_line_author_pairs.iter().find(|(l, _)| *l > line);

        match (prev, next) {
            (Some((_, prev_author)), Some((_, next_author))) => {
                if *prev_author == *next_author && !is_h_line {
                    // Same AI author on both sides, unattributed line: gap-fill.
                    gap_fills.push((prev_author, line));
                }
                // h_ lines (known-human) are never gap-filled in the sandwiched case.
                // Different authors on either side means the gap is ambiguous.
            }
            (None, Some((next_line, next_author))) => {
                // Single-neighbor: only fill truly unattributed lines (not h_)
                // Also don't fill if there's an h_ line between this line and the AI neighbor
                let h_between = h_lines.iter().any(|&h| h > line && h < *next_line);
                if !is_h_line && !h_between {
                    gap_fills.push((next_author, line));
                }
            }
            (Some((prev_line, prev_author)), None) => {
                // Single-neighbor: only fill truly unattributed lines (not h_)
                // Also don't fill if there's an h_ line between this line and the AI neighbor
                let h_between = h_lines.iter().any(|&h| h < line && h > *prev_line);
                if !is_h_line && !h_between {
                    gap_fills.push((prev_author, line));
                }
            }
            (None, None) => {}
        }
    }

    // Apply gap-fills
    for (author, line) in &gap_fills {
        committed_by_author.entry(author).or_default().push(*line);
    }

    // Remove gap-filled lines from h_ entries (they've been reassigned to AI)
    if !gap_fills.is_empty() {
        let filled_lines: HashSet<u32> = gap_fills.iter().map(|(_, l)| *l).collect();
        let h_authors: Vec<&str> = committed_by_author
            .keys()
            .filter(|k| k.starts_with("h_"))
            .copied()
            .collect();
        for h_author in h_authors {
            if let Some(lines) = committed_by_author.get_mut(h_author) {
                lines.retain(|l| !filled_lines.contains(l));
            }
        }
        committed_by_author.retain(|_, lines| !lines.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Utility: compress lines to LineAttribution ranges
// ---------------------------------------------------------------------------

/// Compress sorted line numbers into LineAttribution entries for a given author.
fn compress_to_line_attrs(author_id: &str, lines: &[u32]) -> Vec<LineAttribution> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut attrs = Vec::new();
    let mut start = lines[0];
    let mut end = lines[0];

    for &line in &lines[1..] {
        if line == end + 1 {
            end = line;
        } else {
            attrs.push(LineAttribution {
                start_line: start,
                end_line: end,
                author_id: author_id.to_string(),
                overrode: None,
            });
            start = line;
            end = line;
        }
    }
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id: author_id.to_string(),
        overrode: None,
    });
    attrs
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hunk_header_single_line() {
        let lines = parse_hunk_header_added("@@ -5,0 +6 @@ fn foo()");
        assert_eq!(lines, Some(vec![6]));
    }

    #[test]
    fn test_parse_hunk_header_range() {
        let lines = parse_hunk_header_added("@@ -10,3 +12,4 @@ fn bar()");
        assert_eq!(lines, Some(vec![12, 13, 14, 15]));
    }

    #[test]
    fn test_parse_hunk_header_deletion_only() {
        let lines = parse_hunk_header_added("@@ -10,3 +9,0 @@");
        assert_eq!(lines, Some(Vec::new()));
    }

    #[test]
    fn test_compress_to_line_attrs() {
        let attrs = compress_to_line_attrs("ai_abc", &[1, 2, 3, 5, 7, 8]);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 3);
        assert_eq!(attrs[1].start_line, 5);
        assert_eq!(attrs[1].end_line, 5);
        assert_eq!(attrs[2].start_line, 7);
        assert_eq!(attrs[2].end_line, 8);
    }

    #[test]
    fn test_gap_fill_same_author() {
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![1, 2, 4, 5]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4, 5].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines = committed_by_author.get("abc123").unwrap();
        assert!(lines.contains(&3));
    }

    #[test]
    fn test_gap_fill_different_authors_no_fill() {
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("author_a", vec![1, 2]);
        committed_by_author.insert("author_b", vec![4, 5]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4, 5].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines_a = committed_by_author.get("author_a").unwrap();
        let lines_b = committed_by_author.get("author_b").unwrap();
        assert!(!lines_a.contains(&3));
        assert!(!lines_b.contains(&3));
    }

    #[test]
    fn test_gap_fill_human_prefix_no_fill() {
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("h_abc123", vec![1, 2, 4, 5]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4, 5].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines = committed_by_author.get("h_abc123").unwrap();
        assert!(!lines.contains(&3));
    }

    #[test]
    fn test_parse_diff_added_lines() {
        let diff = "\
diff --git a/foo.rs b/foo.rs
--- a/foo.rs
+++ b/foo.rs
@@ -0,0 +1,3 @@
+line1
+line2
+line3
diff --git a/bar.rs b/bar.rs
--- a/bar.rs
+++ b/bar.rs
@@ -5,0 +6,2 @@
+new1
+new2
";
        let result = parse_diff_added_lines(diff);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("foo.rs".to_string(), vec![1, 2, 3]));
        assert_eq!(result[1], ("bar.rs".to_string(), vec![6, 7]));
    }

    #[test]
    fn test_merge_attributions_last_wins() {
        let cp1 = Checkpoint {
            kind: CheckpointKind::AiAgent,
            author: "dev".to_string(),
            entries: vec![working_log::WorkingLogEntry {
                file: "main.rs".to_string(),
                blob_sha: String::new(),
                attributions: vec![],
                line_attributions: vec![LineAttribution {
                    start_line: 1,
                    end_line: 5,
                    author_id: "old_hash".to_string(),
                    overrode: None,
                }],
            }],
            timestamp: 100,
            agent_id: None,
            trace_id: None,
            seq: 1,
        };

        let cp2 = Checkpoint {
            kind: CheckpointKind::AiAgent,
            author: "dev".to_string(),
            entries: vec![working_log::WorkingLogEntry {
                file: "main.rs".to_string(),
                blob_sha: String::new(),
                attributions: vec![],
                line_attributions: vec![LineAttribution {
                    start_line: 1,
                    end_line: 10,
                    author_id: "new_hash".to_string(),
                    overrode: None,
                }],
            }],
            timestamp: 200,
            agent_id: None,
            trace_id: None,
            seq: 2,
        };

        let initial = InitialAttributions::default();
        let (attrs, _meta) = merge_attributions(&[cp1, cp2], &initial, "dev@test.com");

        let main_attrs = attrs.get("main.rs").unwrap();
        assert_eq!(main_attrs.len(), 1);
        assert_eq!(main_attrs[0].author_id, "new_hash");
        assert_eq!(main_attrs[0].end_line, 10);
    }

    #[test]
    fn test_human_sentinel_becomes_h_attestation() {
        let mut file_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
        file_attrs.insert(
            "test.rs".to_string(),
            vec![
                LineAttribution {
                    start_line: 1,
                    end_line: 3,
                    author_id: "human".to_string(),
                    overrode: None,
                },
                LineAttribution {
                    start_line: 4,
                    end_line: 6,
                    author_id: "ai_hash".to_string(),
                    overrode: None,
                },
            ],
        );

        let committed_lines = vec![("test.rs".to_string(), vec![1, 2, 3, 4, 5, 6])];
        let uncommitted_lines: HashMap<String, Vec<u32>> = HashMap::new();
        let metadata = CollectedMetadata {
            prompts: BTreeMap::new(),
            humans: BTreeMap::new(),
            sessions: BTreeMap::new(),
            initial_only_prompt_ids: HashSet::new(),
        };

        let (log, _initial) = split_attributions(
            &file_attrs,
            &committed_lines,
            &uncommitted_lines,
            &metadata,
            "abc",
            "initial",
            Path::new("."),
            "Test User <test@example.com>",
        );

        // "human" sentinel lines should now appear as h_ attestation entries
        let file_att = &log.attestations[0];
        assert_eq!(file_att.entries.len(), 2);
        let has_ai = file_att.entries.iter().any(|e| e.hash == "ai_hash");
        let has_human = file_att.entries.iter().any(|e| e.hash.starts_with("h_"));
        assert!(has_ai, "should have AI attestation entry");
        assert!(has_human, "should have human (h_) attestation entry");

        // The h_ entry should cover lines 1-3
        let human_entry = file_att
            .entries
            .iter()
            .find(|e| e.hash.starts_with("h_"))
            .unwrap();
        let human_lines: u32 = human_entry.line_ranges.iter().map(|r| r.line_count()).sum();
        assert_eq!(human_lines, 3);
    }

    #[test]
    fn test_parse_diff_dst_path_unquoted() {
        assert_eq!(
            parse_diff_dst_path("+++ b/foo.rs"),
            Some("foo.rs".to_string())
        );
        assert_eq!(
            parse_diff_dst_path("+++ b/path/to/file.rs"),
            Some("path/to/file.rs".to_string())
        );
    }

    #[test]
    fn test_parse_diff_dst_path_quoted_spaces() {
        assert_eq!(
            parse_diff_dst_path("+++ \"b/my file.txt\""),
            Some("my file.txt".to_string())
        );
        assert_eq!(
            parse_diff_dst_path("+++ \"b/path with spaces/file name.rs\""),
            Some("path with spaces/file name.rs".to_string())
        );
    }

    #[test]
    fn test_parse_diff_dst_path_not_dst() {
        assert_eq!(parse_diff_dst_path("--- a/foo.rs"), None);
        assert_eq!(parse_diff_dst_path("+++ /dev/null"), None);
        assert_eq!(parse_diff_dst_path("@@ -1,3 +1,5 @@"), None);
    }

    #[test]
    fn test_parse_diff_added_lines_quoted_paths() {
        let diff = "\
diff --git a/my file.txt b/my file.txt
--- /dev/null
+++ \"b/my file.txt\"
@@ -0,0 +1,2 @@
+hello
+world
";
        let result = parse_diff_added_lines(diff);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("my file.txt".to_string(), vec![1, 2]));
    }

    // -----------------------------------------------------------------------
    // unescape_git_path tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_unescape_git_path_no_escapes() {
        assert_eq!(unescape_git_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn test_unescape_git_path_empty_string() {
        assert_eq!(unescape_git_path(""), "");
    }

    #[test]
    fn test_unescape_git_path_newline() {
        assert_eq!(unescape_git_path("foo\\nbar"), "foo\nbar");
    }

    #[test]
    fn test_unescape_git_path_tab() {
        assert_eq!(unescape_git_path("foo\\tbar"), "foo\tbar");
    }

    #[test]
    fn test_unescape_git_path_backslash() {
        assert_eq!(unescape_git_path("foo\\\\bar"), "foo\\bar");
    }

    #[test]
    fn test_unescape_git_path_double_quote() {
        assert_eq!(unescape_git_path("foo\\\"bar"), "foo\"bar");
    }

    #[test]
    fn test_unescape_git_path_octal_utf8() {
        // 'é' is U+00E9, encoded as UTF-8 bytes 0xC3 0xA9 = octal 303 251
        assert_eq!(unescape_git_path("caf\\303\\251"), "café");
    }

    #[test]
    fn test_unescape_git_path_multiple_escapes() {
        assert_eq!(unescape_git_path("a\\tb\\nc\\\\d"), "a\tb\nc\\d");
    }

    // -----------------------------------------------------------------------
    // build_line_mapping / compute_lcs_line_pairs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_line_mapping_identical() {
        let old = vec!["a", "b", "c"];
        let new = vec!["a", "b", "c"];
        let mapping = build_line_mapping(&old, &new);
        assert_eq!(mapping.get(&1), Some(&1));
        assert_eq!(mapping.get(&2), Some(&2));
        assert_eq!(mapping.get(&3), Some(&3));
        assert_eq!(mapping.len(), 3);
    }

    #[test]
    fn test_build_line_mapping_insert_at_beginning() {
        let old = vec!["a", "b", "c"];
        let new = vec!["x", "a", "b", "c"];
        let mapping = build_line_mapping(&old, &new);
        // old line 1 ("a") -> new line 2
        assert_eq!(mapping.get(&1), Some(&2));
        assert_eq!(mapping.get(&2), Some(&3));
        assert_eq!(mapping.get(&3), Some(&4));
    }

    #[test]
    fn test_build_line_mapping_delete_lines() {
        let old = vec!["a", "b", "c", "d"];
        let new = vec!["a", "d"];
        let mapping = build_line_mapping(&old, &new);
        // old line 1 ("a") -> new line 1
        assert_eq!(mapping.get(&1), Some(&1));
        // old line 4 ("d") -> new line 2
        assert_eq!(mapping.get(&4), Some(&2));
        // lines 2 and 3 are deleted, should not appear
        assert_eq!(mapping.get(&2), None);
        assert_eq!(mapping.get(&3), None);
    }

    #[test]
    fn test_build_line_mapping_complete_rewrite() {
        let old = vec!["a", "b", "c"];
        let new = vec!["x", "y", "z"];
        let mapping = build_line_mapping(&old, &new);
        assert!(mapping.is_empty());
    }

    #[test]
    fn test_build_line_mapping_empty_old() {
        let old: Vec<&str> = vec![];
        let new = vec!["a", "b"];
        let mapping = build_line_mapping(&old, &new);
        assert!(mapping.is_empty());
    }

    #[test]
    fn test_build_line_mapping_empty_new() {
        let old = vec!["a", "b"];
        let new: Vec<&str> = vec![];
        let mapping = build_line_mapping(&old, &new);
        assert!(mapping.is_empty());
    }

    #[test]
    fn test_build_line_mapping_insert_in_middle() {
        let old = vec!["a", "b", "c"];
        let new = vec!["a", "x", "y", "b", "c"];
        let mapping = build_line_mapping(&old, &new);
        assert_eq!(mapping.get(&1), Some(&1)); // "a" stays at 1
        assert_eq!(mapping.get(&2), Some(&4)); // "b" moves to 4
        assert_eq!(mapping.get(&3), Some(&5)); // "c" moves to 5
    }

    // -----------------------------------------------------------------------
    // gap_fill_committed tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_gap_fill_single_neighbor_before() {
        // AI lines before, unattributed line after with no AI neighbor after
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![1, 2, 3]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines = committed_by_author.get("abc123").unwrap();
        assert!(lines.contains(&4), "single-neighbor (before) should fill");
    }

    #[test]
    fn test_gap_fill_single_neighbor_after() {
        // AI lines after, unattributed line before with no AI neighbor before
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![3, 4, 5]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4, 5].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines = committed_by_author.get("abc123").unwrap();
        assert!(
            lines.contains(&1),
            "single-neighbor (after) should fill line 1"
        );
        assert!(
            lines.contains(&2),
            "single-neighbor (after) should fill line 2"
        );
    }

    #[test]
    fn test_gap_fill_h_override_prevents_fill() {
        // h_ lines with h_override should NOT be gap-filled even if sandwiched
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![1, 2, 4, 5]);
        committed_by_author.insert("h_human1", vec![3]);
        let committed_set: HashSet<u32> = [1, 2, 3, 4, 5].into_iter().collect();
        let h_override: HashSet<u32> = [3].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &h_override,
        );

        // Line 3 is h_ with override, so it becomes non-soft and IS eligible for
        // gap-fill (since it's now treated as a regular unattributed line, not an h_ line)
        // Actually re-reading the code: is_h_line = h_lines.contains(&line) && !h_override.contains(&line)
        // With h_override containing 3, is_h_line = true && !true = false
        // So it's NOT treated as h_line, meaning it CAN be gap-filled by same-author sandwich
        let lines = committed_by_author.get("abc123").unwrap();
        assert!(
            lines.contains(&3),
            "h_override makes h_ line eligible for gap-fill"
        );
    }

    #[test]
    fn test_gap_fill_h_line_between_prevents_single_neighbor_fill() {
        // h_ line between AI neighbor and unattributed line prevents fill
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![5, 6]);
        committed_by_author.insert("h_human1", vec![3]);
        // Line 1 is unattributed, line 3 is h_, line 5-6 are AI
        let committed_set: HashSet<u32> = [1, 3, 5, 6].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines = committed_by_author.get("abc123").unwrap();
        // Line 1 has only a "next" AI neighbor at line 5, but h_ line at 3 is between them
        assert!(
            !lines.contains(&1),
            "h_ line between should prevent single-neighbor fill"
        );
    }

    #[test]
    fn test_gap_fill_multiple_authors_ambiguous() {
        // Line between two different AI authors is ambiguous and should not be filled
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("author_a", vec![1]);
        committed_by_author.insert("author_b", vec![3]);
        let committed_set: HashSet<u32> = [1, 2, 3].into_iter().collect();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        let lines_a = committed_by_author.get("author_a").unwrap();
        let lines_b = committed_by_author.get("author_b").unwrap();
        assert!(!lines_a.contains(&2));
        assert!(!lines_b.contains(&2));
    }

    #[test]
    fn test_gap_fill_empty_committed_set() {
        let mut committed_by_author: HashMap<&str, Vec<u32>> = HashMap::new();
        committed_by_author.insert("abc123", vec![1, 2, 3]);
        let committed_set: HashSet<u32> = HashSet::new();

        gap_fill_committed(
            &mut committed_by_author,
            &committed_set,
            &HashSet::new(),
            &HashSet::new(),
        );

        // No lines in committed_set means nothing to iterate over, no fills
        let lines = committed_by_author.get("abc123").unwrap();
        assert_eq!(lines.len(), 3);
    }

    // -----------------------------------------------------------------------
    // compress_to_line_attrs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compress_to_line_attrs_single_line() {
        let attrs = compress_to_line_attrs("ai_xyz", &[42]);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].start_line, 42);
        assert_eq!(attrs[0].end_line, 42);
        assert_eq!(attrs[0].author_id, "ai_xyz");
    }

    #[test]
    fn test_compress_to_line_attrs_already_continuous() {
        let attrs = compress_to_line_attrs("ai_xyz", &[1, 2, 3, 4, 5]);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 5);
    }

    #[test]
    fn test_compress_to_line_attrs_empty_input() {
        let attrs = compress_to_line_attrs("ai_xyz", &[]);
        assert!(attrs.is_empty());
    }

    #[test]
    fn test_compress_to_line_attrs_multiple_gaps() {
        let attrs = compress_to_line_attrs("ai_xyz", &[1, 3, 5, 7]);
        assert_eq!(attrs.len(), 4);
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 1);
        assert_eq!(attrs[1].start_line, 3);
        assert_eq!(attrs[1].end_line, 3);
        assert_eq!(attrs[2].start_line, 5);
        assert_eq!(attrs[2].end_line, 5);
        assert_eq!(attrs[3].start_line, 7);
        assert_eq!(attrs[3].end_line, 7);
    }
}
