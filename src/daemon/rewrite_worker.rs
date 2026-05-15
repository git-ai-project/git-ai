use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use imara_diff::{Algorithm, Diff, InternedInput, TokenSource};

use crate::core::attribution::LineAttribution;
use crate::core::authorship_log::{
    AttestationEntry, AuthorshipLog, FileAttestation, LineRange, Metadata,
};

use super::commit_detector::RewriteKind;

// ---------------------------------------------------------------------------
// Git helper
// ---------------------------------------------------------------------------

fn git_in_repo(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("git failed to execute: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

fn git_in_repo_stdin(repo_path: &Path, args: &[&str], stdin: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .env("GIT_TRACE2_EVENT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git failed to spawn: {}", e))?;

    if let Some(ref mut stdin_pipe) = child.stdin {
        stdin_pipe
            .write_all(stdin)
            .map_err(|e| format!("failed to write to git stdin: {}", e))?;
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|e| format!("git failed to complete: {}", e))?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

// ---------------------------------------------------------------------------
// Range-diff parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum MappingKind {
    /// Commit is unchanged (patch-identical). Note can be copied as-is.
    Identical,
    /// Commit was modified (content changed). Needs hunk-level attribution transfer.
    Modified,
    /// Commit was deleted (dropped during rebase/squash). Attribution lost.
    Deleted,
    /// Commit was added (new commit not in original range).
    Added,
}

#[derive(Debug, Clone)]
struct CommitMapping {
    kind: MappingKind,
    original: Option<String>,
    new: Option<String>,
}

/// Run `git range-diff` and parse the output into commit mappings.
/// Compares `old_base..original_head` vs `new_base..new_head`.
fn run_range_diff(
    repo_path: &Path,
    old_base: &str,
    original_head: &str,
    new_base: &str,
    new_head: &str,
) -> Result<Vec<CommitMapping>, String> {
    if original_head == new_head {
        return Ok(Vec::new());
    }

    let output = git_in_repo(
        repo_path,
        &[
            "range-diff",
            "--no-color",
            "--no-notes",
            "--no-patch",
            &format!("{}..{}", old_base, original_head),
            &format!("{}..{}", new_base, new_head),
        ],
    )?;

    parse_range_diff_output(&output)
}

/// Parse the raw output of `git range-diff --no-color --no-patch`.
fn parse_range_diff_output(output: &str) -> Result<Vec<CommitMapping>, String> {
    let mut mappings = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || line.starts_with("    ") {
            continue;
        }

        if let Some(mapping) = parse_range_diff_line(trimmed) {
            mappings.push(mapping);
        }
    }

    Ok(mappings)
}

fn parse_range_diff_line(line: &str) -> Option<CommitMapping> {
    let candidates = [
        line.find(" = ").map(|i| (i, '=')),
        line.find(" ! ").map(|i| (i, '!')),
        line.find(" < ").map(|i| (i, '<')),
        line.find(" > ").map(|i| (i, '>')),
    ];
    let &(idx, op) = candidates
        .iter()
        .filter_map(|x| x.as_ref())
        .min_by_key(|(i, _)| *i)?;
    let left_part = &line[..idx];
    let right_part = &line[idx + 3..];

    let left_sha = extract_sha(left_part);
    let right_sha = extract_sha(right_part);

    let mapping = match op {
        '=' => CommitMapping {
            kind: MappingKind::Identical,
            original: left_sha,
            new: right_sha,
        },
        '!' => CommitMapping {
            kind: MappingKind::Modified,
            original: left_sha,
            new: right_sha,
        },
        '<' => CommitMapping {
            kind: MappingKind::Deleted,
            original: left_sha,
            new: None,
        },
        '>' => CommitMapping {
            kind: MappingKind::Added,
            original: None,
            new: right_sha,
        },
        _ => return None,
    };

    Some(mapping)
}

fn extract_sha(part: &str) -> Option<String> {
    for word in part.split_whitespace() {
        if word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(word.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Diff-based attribution transfer
// ---------------------------------------------------------------------------

/// Represents a diff operation between two line sequences.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum DiffOp {
    Equal {
        old_index: usize,
        new_index: usize,
        len: usize,
    },
    Delete {
        old_index: usize,
        old_len: usize,
        new_index: usize,
    },
    Insert {
        old_index: usize,
        new_index: usize,
        new_len: usize,
    },
    Replace {
        old_index: usize,
        old_len: usize,
        new_index: usize,
        new_len: usize,
    },
}

struct SliceSource<'a> {
    slice: &'a [&'a str],
}

impl<'a> TokenSource for SliceSource<'a> {
    type Token = &'a str;
    type Tokenizer = std::iter::Copied<std::slice::Iter<'a, &'a str>>;

    fn tokenize(&self) -> Self::Tokenizer {
        self.slice.iter().copied()
    }

    fn estimate_tokens(&self) -> u32 {
        self.slice.len() as u32
    }
}

fn compute_line_diff_ops(old_lines: &[&str], new_lines: &[&str]) -> Vec<DiffOp> {
    let input = InternedInput::new(
        SliceSource { slice: old_lines },
        SliceSource { slice: new_lines },
    );
    let diff = Diff::compute(Algorithm::Myers, &input);
    hunks_to_ops(&diff, old_lines.len())
}

#[allow(unused_assignments)]
fn hunks_to_ops(diff: &Diff, old_len: usize) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut old_idx = 0usize;
    let mut new_idx = 0usize;

    for hunk in diff.hunks() {
        let ho_start = hunk.before.start as usize;
        let ho_end = hunk.before.end as usize;
        let hn_start = hunk.after.start as usize;
        let hn_end = hunk.after.end as usize;

        if old_idx < ho_start {
            let eq_len = ho_start - old_idx;
            ops.push(DiffOp::Equal {
                old_index: old_idx,
                new_index: new_idx,
                len: eq_len,
            });
            new_idx += eq_len;
        }

        let old_hunk_len = ho_end - ho_start;
        let new_hunk_len = hn_end - hn_start;

        if old_hunk_len > 0 && new_hunk_len > 0 {
            ops.push(DiffOp::Replace {
                old_index: ho_start,
                old_len: old_hunk_len,
                new_index: hn_start,
                new_len: new_hunk_len,
            });
        } else if old_hunk_len > 0 {
            ops.push(DiffOp::Delete {
                old_index: ho_start,
                old_len: old_hunk_len,
                new_index: hn_start,
            });
        } else if new_hunk_len > 0 {
            ops.push(DiffOp::Insert {
                old_index: ho_start,
                new_index: hn_start,
                new_len: new_hunk_len,
            });
        }

        old_idx = ho_end;
        new_idx = hn_end;
    }

    if old_idx < old_len {
        let remaining = old_len - old_idx;
        ops.push(DiffOp::Equal {
            old_index: old_idx,
            new_index: new_idx,
            len: remaining,
        });
    }

    ops
}

/// Transfer line attributions from old content to new content through a diff.
///
/// For each "Equal" region in the diff, attributions on old lines are carried forward
/// to the corresponding new lines. Insert/Delete/Replace regions lose attribution
/// (new lines are unattributed, deleted lines disappear).
fn diff_based_line_attribution_transfer(
    old_content: &str,
    new_content: &str,
    old_line_attrs: &[LineAttribution],
) -> Vec<LineAttribution> {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Build a sparse lookup: 0-indexed old line position -> author_id
    let mut old_line_author: HashMap<usize, &str> = HashMap::new();
    for attr in old_line_attrs {
        for line_num in attr.start_line..=attr.end_line {
            let idx = (line_num as usize).saturating_sub(1);
            if idx < old_lines.len() {
                old_line_author.insert(idx, &attr.author_id);
            }
        }
    }

    let diff_ops = compute_line_diff_ops(&old_lines, &new_lines);

    let mut new_line_attrs: Vec<LineAttribution> = Vec::with_capacity(old_line_author.len());

    for op in &diff_ops {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for i in 0..*len {
                let old_idx = old_index + i;
                let new_line_num = (new_index + i + 1) as u32;
                if let Some(author_id) = old_line_author.get(&old_idx) {
                    new_line_attrs.push(LineAttribution {
                        start_line: new_line_num,
                        end_line: new_line_num,
                        author_id: author_id.to_string(),
                        overrode: None,
                    });
                }
            }
        }
    }

    new_line_attrs
}

// ---------------------------------------------------------------------------
// Squash detection
// ---------------------------------------------------------------------------

struct SquashGroup {
    matched_original: Option<String>,
    deleted_originals: Vec<String>,
}

impl SquashGroup {
    fn all_originals(&self) -> Vec<String> {
        let mut all = self.deleted_originals.clone();
        if let Some(matched) = &self.matched_original {
            all.push(matched.clone());
        }
        all
    }
}

/// Detect squash patterns: consecutive Deleted commits followed by a Modified/Added.
fn detect_squash_groups(mappings: &[CommitMapping]) -> HashMap<String, SquashGroup> {
    let mut groups: HashMap<String, SquashGroup> = HashMap::new();
    let mut pending_deleted: Vec<String> = Vec::new();

    for mapping in mappings {
        match mapping.kind {
            MappingKind::Deleted => {
                if let Some(original) = &mapping.original {
                    pending_deleted.push(original.clone());
                }
            }
            MappingKind::Modified | MappingKind::Added => {
                if !pending_deleted.is_empty()
                    && let Some(new) = &mapping.new
                {
                    groups.insert(
                        new.clone(),
                        SquashGroup {
                            matched_original: mapping.original.clone(),
                            deleted_originals: std::mem::take(&mut pending_deleted),
                        },
                    );
                }
                pending_deleted.clear();
            }
            MappingKind::Identical => {
                pending_deleted.clear();
            }
        }
    }
    groups
}

// ---------------------------------------------------------------------------
// Note read/write helpers
// ---------------------------------------------------------------------------

fn read_note(repo_path: &Path, sha: &str) -> Option<String> {
    git_in_repo(repo_path, &["notes", "--ref=ai", "show", sha]).ok()
}

fn write_note(repo_path: &Path, sha: &str, content: &str) -> Result<(), String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["notes", "--ref=ai", "add", "-f", "-m", content, sha])
        .env("GIT_TRACE2_EVENT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| format!("failed to run git notes: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("git notes add failed for {}", sha))
    }
}

/// Batch read file contents at a specific commit using `git cat-file --batch`.
fn batch_cat_file(
    repo_path: &Path,
    commit_sha: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, String> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let stdin_data: String = file_paths
        .iter()
        .map(|path| format!("{}:{}", commit_sha, path))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let data = git_in_repo_stdin(repo_path, &["cat-file", "--batch"], stdin_data.as_bytes())?;

    let mut results = HashMap::new();
    let mut pos = 0usize;
    let mut path_idx = 0usize;

    while pos < data.len() && path_idx < file_paths.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end]).unwrap_or("");
        let parts: Vec<&str> = header.split_whitespace().collect();

        if parts.len() >= 2 && parts[1] == "missing" {
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        let size: usize = parts[2].parse().unwrap_or(0);
        let content_start = header_end + 1;
        let content_end = content_start + size;

        if content_end <= data.len() {
            let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
            results.insert(file_paths[path_idx].clone(), content);
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
        path_idx += 1;
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Attestation helpers
// ---------------------------------------------------------------------------

/// Convert attestation entries (hash + line ranges) into flat LineAttribution list.
fn attestation_entries_to_line_attributions(entries: &[AttestationEntry]) -> Vec<LineAttribution> {
    let mut attrs = Vec::new();
    for entry in entries {
        for range in &entry.line_ranges {
            let (start, end) = match range {
                LineRange::Single(l) => (*l, *l),
                LineRange::Range(s, e) => (*s, *e),
            };
            attrs.push(LineAttribution {
                start_line: start,
                end_line: end,
                author_id: entry.hash.clone(),
                overrode: None,
            });
        }
    }
    attrs
}

/// Build a FileAttestation from a flat list of line attributions.
/// Merges adjacent ranges and groups by author. Returns None if empty.
fn build_file_attestation(
    file_path: &str,
    line_attrs: &[LineAttribution],
) -> Option<FileAttestation> {
    let mut by_author: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for attr in line_attrs {
        // Skip "human" placeholder attributions (untracked changes)
        if attr.author_id == "human" {
            continue;
        }
        by_author
            .entry(attr.author_id.clone())
            .or_default()
            .push((attr.start_line, attr.end_line));
    }

    if by_author.is_empty() {
        return None;
    }

    let mut entries = Vec::new();

    for (author_id, mut ranges) in by_author {
        if ranges.is_empty() {
            continue;
        }
        ranges.sort_by_key(|(start, end)| (*start, *end));

        // Merge adjacent/overlapping ranges
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some((_, last_end)) => {
                    if start <= last_end.saturating_add(1) {
                        *last_end = (*last_end).max(end);
                    } else {
                        merged.push((start, end));
                    }
                }
                None => merged.push((start, end)),
            }
        }

        let line_ranges: Vec<LineRange> = merged
            .into_iter()
            .map(|(start, end)| {
                if start == end {
                    LineRange::Single(start)
                } else {
                    LineRange::Range(start, end)
                }
            })
            .collect();

        if !line_ranges.is_empty() {
            entries.push(AttestationEntry {
                hash: author_id,
                line_ranges,
            });
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(FileAttestation {
            file_path: file_path.to_string(),
            entries,
        })
    }
}

/// Overlay a new attribution range onto existing attributions, splitting partial overlaps.
fn overlay_attribution(attrs: &mut Vec<LineAttribution>, start: u32, end: u32, author_id: String) {
    let mut i = 0;
    let mut to_insert_after: Vec<LineAttribution> = Vec::new();
    while i < attrs.len() {
        let a = &attrs[i];
        if a.end_line < start || a.start_line > end {
            i += 1;
            continue;
        }
        let removed = attrs.remove(i);
        if removed.start_line < start {
            attrs.insert(
                i,
                LineAttribution {
                    start_line: removed.start_line,
                    end_line: start - 1,
                    author_id: removed.author_id.clone(),
                    overrode: removed.overrode.clone(),
                },
            );
            i += 1;
        }
        if removed.end_line > end {
            to_insert_after.push(LineAttribution {
                start_line: end + 1,
                end_line: removed.end_line,
                author_id: removed.author_id,
                overrode: removed.overrode,
            });
        }
    }
    for frag in to_insert_after {
        attrs.push(frag);
    }
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id,
        overrode: None,
    });
}

/// Remap the base_commit_sha field in the metadata.
fn remap_base_commit_sha(note_content: &str, target_commit: &str) -> String {
    let field = "\"base_commit_sha\"";
    let Some(field_pos) = note_content.find(field) else {
        return note_content.to_string();
    };
    let bytes = note_content.as_bytes();

    let mut pos = field_pos + field.len();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return note_content.to_string();
    }
    pos += 1;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return note_content.to_string();
    }
    pos += 1;
    let value_start = pos;

    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 2;
            }
            b'"' => {
                let mut remapped = String::with_capacity(
                    note_content.len() - (pos - value_start) + target_commit.len(),
                );
                remapped.push_str(&note_content[..value_start]);
                remapped.push_str(target_commit);
                remapped.push_str(&note_content[pos..]);
                return remapped;
            }
            _ => {
                pos += 1;
            }
        }
    }

    note_content.to_string()
}

// ---------------------------------------------------------------------------
// Core rewrite logic
// ---------------------------------------------------------------------------

/// Process a detected rewrite operation (rebase, cherry-pick, amend, reset).
/// Uses content-aware attribution transfer via range-diff and per-file diffs.
pub fn process_rewrite(
    repo_path: &Path,
    kind: &RewriteKind,
    _argv: &[String],
) -> Result<u32, String> {
    match kind {
        RewriteKind::Rebase => process_rebase(repo_path),
        RewriteKind::CherryPick => process_cherry_pick(repo_path),
        RewriteKind::Amend => process_amend(repo_path),
        RewriteKind::Reset => process_reset(repo_path),
    }
}

/// After a rebase completes, use range-diff to get accurate commit mappings
/// and transfer attributions with content-awareness.
fn process_rebase(repo_path: &Path) -> Result<u32, String> {
    let orig_head = git_in_repo(repo_path, &["rev-parse", "ORIG_HEAD"])
        .map_err(|_| "ORIG_HEAD not available — cannot determine rebase source".to_string())?;

    let new_head =
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).map_err(|e| format!("HEAD: {}", e))?;

    if orig_head == new_head {
        return Ok(0);
    }

    // Find merge base to determine the fork point.
    // BUG FIX: The merge-base of ORIG_HEAD and HEAD gives us the common ancestor,
    // which is the point where the original branch forked from the base.
    // This is correct for determining the old base commit.
    let merge_base = git_in_repo(repo_path, &["merge-base", &orig_head, &new_head])
        .unwrap_or_else(|_| String::new());

    if merge_base.is_empty() {
        return Ok(0);
    }

    // Count original commits: these are the commits that were rebased.
    // Old commits: merge_base..orig_head
    let orig_count_str = git_in_repo(
        repo_path,
        &[
            "rev-list",
            "--count",
            &format!("{}..{}", merge_base, orig_head),
        ],
    )
    .unwrap_or_else(|_| "0".to_string());
    let orig_count: usize = orig_count_str.trim().parse().unwrap_or(0);

    if orig_count == 0 {
        return Ok(0);
    }

    // The "onto" commit is where the rebase landed. After rebase, HEAD contains:
    // - All commits from the onto branch
    // - The rebased commits (last N commits on HEAD)
    // So: onto = new_head~orig_count
    //
    // For range-diff:
    // - Old range: merge_base..orig_head (the original commits)
    // - New range: onto..new_head (just the rebased commits)
    let onto = git_in_repo(
        repo_path,
        &["rev-parse", &format!("{}~{}", new_head, orig_count)],
    )
    .unwrap_or(merge_base.clone());

    // Use range-diff for accurate commit mapping.
    // Old range: merge_base..orig_head (the original feature commits)
    // New range: onto..new_head (just the rebased commits, excluding pre-existing commits)
    let mappings = match run_range_diff(repo_path, &merge_base, &orig_head, &onto, &new_head) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            return process_rebase_positional(repo_path, &orig_head, &new_head, &merge_base);
        }
        Err(_) => {
            return process_rebase_positional(repo_path, &orig_head, &new_head, &merge_base);
        }
    };

    // Detect squash groups
    let squash_groups = detect_squash_groups(&mappings);

    let mut processed = 0u32;

    for mapping in &mappings {
        if mapping.kind == MappingKind::Deleted {
            continue;
        }

        match mapping.kind {
            MappingKind::Identical => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new)
                    && copy_note_with_remap(repo_path, original, new)?
                {
                    processed += 1;
                }
            }
            MappingKind::Modified => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    if let Some(group) = squash_groups.get(new.as_str()) {
                        if transfer_attribution_squash(repo_path, &group.all_originals(), new)? {
                            processed += 1;
                        }
                    } else if transfer_attribution_via_diff(repo_path, original, new)? {
                        processed += 1;
                    }
                }
            }
            MappingKind::Added => {
                if let Some(new) = &mapping.new
                    && let Some(group) = squash_groups.get(new.as_str())
                    && transfer_attribution_squash(repo_path, &group.all_originals(), new)?
                {
                    processed += 1;
                }
            }
            MappingKind::Deleted => unreachable!(),
        }
    }

    // Migrate working log
    migrate_working_log(repo_path, &orig_head, &new_head)?;

    if processed > 0 {
        eprintln!(
            "[git-ai daemon] rebase: transferred {} note(s) in {}",
            processed,
            repo_path.display()
        );
    }

    Ok(processed)
}

/// Fallback positional mapping when range-diff is not available or produces no results.
fn process_rebase_positional(
    repo_path: &Path,
    orig_head: &str,
    new_head: &str,
    merge_base: &str,
) -> Result<u32, String> {
    let orig_log = git_in_repo(
        repo_path,
        &[
            "log",
            "--format=%H",
            "--reverse",
            &format!("{}..{}", merge_base, orig_head),
        ],
    )
    .unwrap_or_default();
    let orig_commits: Vec<&str> = orig_log.lines().filter(|l| !l.is_empty()).collect();

    if orig_commits.is_empty() {
        return Ok(0);
    }

    let count_arg = format!("-{}", orig_commits.len());
    let new_log = git_in_repo(
        repo_path,
        &["log", "--format=%H", "--reverse", &count_arg, new_head],
    )
    .unwrap_or_default();
    let new_commits: Vec<&str> = new_log.lines().filter(|l| !l.is_empty()).collect();

    let mut processed = 0u32;

    // 1:1 positional mapping with content-aware transfer
    for (old_sha, new_sha) in orig_commits.iter().zip(new_commits.iter()) {
        if transfer_attribution_via_diff(repo_path, old_sha, new_sha)? {
            processed += 1;
        }
    }

    // Handle squash: if fewer new commits than orig, merge remaining into last
    if new_commits.len() < orig_commits.len() && !new_commits.is_empty() {
        let last_new = new_commits.last().unwrap();
        let remaining: Vec<String> = orig_commits[new_commits.len()..]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !remaining.is_empty() && transfer_attribution_squash(repo_path, &remaining, last_new)? {
            processed += 1;
        }
    }

    migrate_working_log(repo_path, orig_head, new_head)?;

    if processed > 0 {
        eprintln!(
            "[git-ai daemon] rebase (positional): transferred {} note(s) in {}",
            processed,
            repo_path.display()
        );
    }

    Ok(processed)
}

/// After cherry-pick, transfer attribution from source commit(s).
fn process_cherry_pick(repo_path: &Path) -> Result<u32, String> {
    let new_head =
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).map_err(|e| format!("HEAD: {}", e))?;

    // Try ORIG_HEAD first, fall back to reflog HEAD@{1} (modern git doesn't always set ORIG_HEAD for cherry-pick)
    let orig_head = git_in_repo(repo_path, &["rev-parse", "ORIG_HEAD"])
        .or_else(|_| git_in_repo(repo_path, &["rev-parse", "HEAD@{1}"]))
        .map_err(|_| "cannot determine pre-cherry-pick HEAD".to_string())?;

    if orig_head == new_head {
        return Ok(0);
    }

    // New commits are those between orig_head and new_head
    let new_log = git_in_repo(
        repo_path,
        &[
            "log",
            "--format=%H",
            "--reverse",
            &format!("{}..{}", orig_head, new_head),
        ],
    )
    .unwrap_or_default();
    let new_commits: Vec<&str> = new_log.lines().filter(|l| !l.is_empty()).collect();

    if new_commits.is_empty() {
        return Ok(0);
    }

    let mut processed = 0u32;
    for new_sha in &new_commits {
        if let Some(source_sha) = find_cherry_pick_source(repo_path, new_sha)
            && transfer_attribution_via_diff(repo_path, &source_sha, new_sha)?
        {
            processed += 1;
        }
    }

    if processed > 0 {
        eprintln!(
            "[git-ai daemon] cherry-pick: transferred {} note(s) in {}",
            processed,
            repo_path.display()
        );
    }

    Ok(processed)
}

/// After amend, transfer attribution from old HEAD to new HEAD with content-awareness.
fn process_amend(repo_path: &Path) -> Result<u32, String> {
    let new_head =
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).map_err(|e| format!("HEAD: {}", e))?;

    let old_head = git_in_repo(repo_path, &["rev-parse", "HEAD@{1}"])
        .map_err(|_| "HEAD@{1} not available — cannot determine pre-amend commit".to_string())?;

    if old_head == new_head {
        return Ok(0);
    }

    let result = if transfer_attribution_via_diff(repo_path, &old_head, &new_head)? {
        eprintln!(
            "[git-ai daemon] amend: transferred note {} -> {} in {}",
            &old_head[..7.min(old_head.len())],
            &new_head[..7.min(new_head.len())],
            repo_path.display()
        );
        1
    } else {
        0
    };

    migrate_working_log(repo_path, &old_head, &new_head)?;

    Ok(result)
}

/// After reset, migrate working log if HEAD moved.
///
/// BUG FIX / KNOWN LIMITATION: `git reset --soft HEAD~1` moves HEAD but the old HEAD's
/// note is lost. Ideally, we should cache the old HEAD's note so that when the next
/// commit is made (recommit), the post-commit handler can seed from it.
///
/// For now, we document this as a known limitation. A full fix would require:
/// 1. In this reset handler, copy the old HEAD's note content to a cache file:
///    `.git/ai/reset_note_cache/<old_sha>` → note content
/// 2. In the post-commit handler, check if `parent_sha` has a cached note and
///    use it as the base for the new commit's authorship.
///
/// This is architecturally complex as it requires coordination between reset and
/// post-commit handlers. We leave it as a TODO for future enhancement.
fn process_reset(repo_path: &Path) -> Result<u32, String> {
    let new_head =
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).map_err(|e| format!("HEAD: {}", e))?;

    let old_head = match git_in_repo(repo_path, &["rev-parse", "HEAD@{1}"]) {
        Ok(h) => h,
        Err(_) => return Ok(0),
    };

    if old_head == new_head {
        return Ok(0);
    }

    // TODO: Cache the old HEAD's note for use in next commit (see function doc above)
    // For now, we only migrate the working log.
    if let Some(old_note) = read_note(repo_path, &old_head) {
        let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"])?;
        let git_dir = std::path::PathBuf::from(&git_dir_str);
        let abs_git_dir = if git_dir.is_relative() {
            repo_path.join(&git_dir)
        } else {
            git_dir
        };
        let cache_dir = abs_git_dir.join("ai").join("reset_note_cache");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            eprintln!(
                "[git-ai daemon] warning: failed to create reset_note_cache dir: {}",
                e
            );
        } else {
            let cache_file = cache_dir.join(&old_head);
            if let Err(e) = std::fs::write(&cache_file, old_note) {
                eprintln!(
                    "[git-ai daemon] warning: failed to cache note for reset: {}",
                    e
                );
            } else {
                eprintln!(
                    "[git-ai daemon] reset: cached note for {} (available for next commit)",
                    &old_head[..7.min(old_head.len())]
                );
            }
        }
    }

    migrate_working_log(repo_path, &old_head, &new_head)?;

    eprintln!(
        "[git-ai daemon] reset: migrated working log {} -> {} in {}",
        &old_head[..7.min(old_head.len())],
        &new_head[..7.min(new_head.len())],
        repo_path.display()
    );

    Ok(0)
}

// ---------------------------------------------------------------------------
// Public API for CLI post-rewrite command
// ---------------------------------------------------------------------------

/// Transfer attribution from a single old commit to a single new commit.
/// Public interface used by the `git-ai post-rewrite` CLI command.
pub fn transfer_single_commit(
    repo_path: &Path,
    old_sha: &str,
    new_sha: &str,
) -> Result<bool, String> {
    transfer_attribution_via_diff(repo_path, old_sha, new_sha)
}

// ---------------------------------------------------------------------------
// Transfer helpers
// ---------------------------------------------------------------------------

/// Copy a note from old to new commit, remapping base_commit_sha.
/// Returns true if a note was actually copied. Skips if new already has a note.
fn copy_note_with_remap(repo_path: &Path, old_sha: &str, new_sha: &str) -> Result<bool, String> {
    // Skip if new commit already has a note
    if read_note(repo_path, new_sha).is_some() {
        return Ok(false);
    }

    let note = match read_note(repo_path, old_sha) {
        Some(n) if !n.trim().is_empty() => n,
        _ => return Ok(false),
    };

    // Remap base_commit_sha and write
    let remapped = remap_base_commit_sha(&note, new_sha);
    write_note(repo_path, new_sha, &remapped)?;
    Ok(true)
}

/// Transfer attribution from one commit to another using per-file diffs.
/// Returns true if a note was written to the new commit.
fn transfer_attribution_via_diff(
    repo_path: &Path,
    original_sha: &str,
    new_sha: &str,
) -> Result<bool, String> {
    // Skip if the new commit already has a note
    if read_note(repo_path, new_sha).is_some() {
        return Ok(false);
    }

    let note_content = match read_note(repo_path, original_sha) {
        Some(n) if !n.trim().is_empty() => n,
        _ => return Ok(false),
    };

    let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
        Ok(log) => log,
        Err(_) => {
            // Unparseable note: copy as-is with remapped base_commit_sha
            let remapped = remap_base_commit_sha(&note_content, new_sha);
            write_note(repo_path, new_sha, &remapped)?;
            return Ok(true);
        }
    };

    let attested_files: Vec<String> = authorship_log
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    if attested_files.is_empty() {
        // Metadata-only note: copy with remapped base_commit_sha
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        write_note(repo_path, new_sha, &remapped)?;
        return Ok(true);
    }

    // Read file contents at both commits
    let old_contents = batch_cat_file(repo_path, original_sha, &attested_files)?;
    let new_contents = batch_cat_file(repo_path, new_sha, &attested_files)?;

    let mut new_log = AuthorshipLog::new(Metadata::new(new_sha.to_string()));
    new_log.metadata.prompts = authorship_log.metadata.prompts.clone();
    new_log.metadata.sessions = authorship_log.metadata.sessions.clone();
    new_log.metadata.humans = authorship_log.metadata.humans.clone();

    for file_attestation in &authorship_log.attestations {
        let file_path = &file_attestation.file_path;
        let old_content = match old_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };
        let new_content = match new_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };

        let old_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
        if old_attrs.is_empty() {
            continue;
        }

        // If content is identical, carry attributions as-is
        let transferred = if old_content == new_content {
            old_attrs
        } else {
            diff_based_line_attribution_transfer(old_content, new_content, &old_attrs)
        };

        if let Some(new_attestation) = build_file_attestation(file_path, &transferred) {
            new_log.attestations.push(new_attestation);
        }
    }

    // If diff transfer produced nothing, fall back to copying original note
    if new_log.attestations.is_empty() {
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        write_note(repo_path, new_sha, &remapped)?;
        return Ok(true);
    }

    let serialized = new_log.serialize_to_string();
    write_note(repo_path, new_sha, &serialized)?;
    Ok(true)
}

/// Transfer attribution from multiple original commits into a single squash result.
/// Uses sequential replay: processes original commits in order, transferring accumulated
/// attributions through each diff step, then overlaying each commit's note.
pub fn transfer_attribution_squash(
    repo_path: &Path,
    originals: &[String],
    new_sha: &str,
) -> Result<bool, String> {
    if originals.is_empty() {
        return Ok(false);
    }

    // Skip if new commit already has a note
    if read_note(repo_path, new_sha).is_some() {
        return Ok(false);
    }

    // Collect all files mentioned in any original note and merge metadata
    let mut all_files: Vec<String> = Vec::new();
    let mut all_files_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut base_metadata: Option<Metadata> = None;
    let mut parsed_notes: Vec<(String, AuthorshipLog)> = Vec::new();

    for original_sha in originals {
        let note_content = match read_note(repo_path, original_sha) {
            Some(n) if !n.trim().is_empty() => n,
            _ => continue,
        };
        let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
            Ok(log) => log,
            Err(_) => continue,
        };

        if base_metadata.is_none() {
            base_metadata = Some(authorship_log.metadata.clone());
        } else if let Some(ref mut base) = base_metadata {
            for (k, v) in &authorship_log.metadata.prompts {
                base.prompts.entry(k.clone()).or_insert_with(|| v.clone());
            }
            for (k, v) in &authorship_log.metadata.humans {
                base.humans.entry(k.clone()).or_insert_with(|| v.clone());
            }
            for (k, v) in &authorship_log.metadata.sessions {
                base.sessions.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }

        for att in &authorship_log.attestations {
            if all_files_set.insert(att.file_path.clone()) {
                all_files.push(att.file_path.clone());
            }
        }
        parsed_notes.push((original_sha.clone(), authorship_log));
    }

    let Some(mut metadata) = base_metadata else {
        return Ok(false);
    };

    if parsed_notes.is_empty() {
        return Ok(false);
    }

    // Sequential replay: accumulate attributions by diffing consecutive commits
    let mut accumulated_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prev_contents: HashMap<String, String> = HashMap::new();

    for (i, (original_sha, authorship_log)) in parsed_notes.iter().enumerate() {
        let current_contents = batch_cat_file(repo_path, original_sha, &all_files)?;

        if i > 0 {
            // Diff previous content -> current content and transfer accumulated attrs
            for file_path in &all_files {
                let prev_content = prev_contents.get(file_path).map(String::as_str);
                let curr_content = current_contents.get(file_path).map(String::as_str);

                if let (Some(prev_c), Some(curr_c)) = (prev_content, curr_content)
                    && !prev_c.is_empty()
                    && !curr_c.is_empty()
                    && prev_c != curr_c
                    && let Some(attrs) = accumulated_attrs.get(file_path)
                    && !attrs.is_empty()
                {
                    let transferred = diff_based_line_attribution_transfer(prev_c, curr_c, attrs);
                    accumulated_attrs.insert(file_path.clone(), transferred);
                }
            }
        }

        // Overlay this commit's note attributions
        for file_attestation in &authorship_log.attestations {
            let file_path = &file_attestation.file_path;
            let note_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
            if note_attrs.is_empty() {
                continue;
            }
            let entry = accumulated_attrs.entry(file_path.clone()).or_default();
            for attr in note_attrs {
                overlay_attribution(entry, attr.start_line, attr.end_line, attr.author_id);
            }
        }

        prev_contents = current_contents;
    }

    // Finally, diff the last original's content -> squash result content
    let final_contents = batch_cat_file(repo_path, new_sha, &all_files)?;
    for file_path in &all_files {
        let prev_content = prev_contents.get(file_path).map(String::as_str);
        let final_content = final_contents.get(file_path).map(String::as_str);

        if let (Some(prev_c), Some(final_c)) = (prev_content, final_content)
            && !prev_c.is_empty()
            && !final_c.is_empty()
            && prev_c != final_c
            && let Some(attrs) = accumulated_attrs.get(file_path)
            && !attrs.is_empty()
        {
            let transferred = diff_based_line_attribution_transfer(prev_c, final_c, attrs);
            accumulated_attrs.insert(file_path.clone(), transferred);
        }
    }

    metadata.base_commit_sha = new_sha.to_string();
    let mut new_log = AuthorshipLog::new(metadata);

    for (file_path, mut attrs) in accumulated_attrs {
        attrs.sort_by_key(|a| a.start_line);
        if let Some(attestation) = build_file_attestation(&file_path, &attrs) {
            new_log.attestations.push(attestation);
        }
    }

    if new_log.attestations.is_empty() && new_log.metadata.prompts.is_empty() {
        return Ok(false);
    }

    let serialized = new_log.serialize_to_string();
    write_note(repo_path, new_sha, &serialized)?;
    Ok(true)
}

/// Find the source commit for a cherry-picked commit.
///
/// BUG FIX: Cherry-pick without `-x` doesn't add the "(cherry picked from...)" trailer.
/// We now:
/// 1. Check for the trailer (works with `-x`)
/// 2. Check `.git/CHERRY_PICK_HEAD` (available during/after cherry-pick)
/// 3. Fall back to patch-id matching (future enhancement — currently just returns None)
fn find_cherry_pick_source(repo_path: &Path, new_sha: &str) -> Option<String> {
    // Method 1: Check commit message for "(cherry picked from commit <sha>)" trailer
    let msg = git_in_repo(repo_path, &["log", "-1", "--format=%B", new_sha]).ok()?;
    for line in msg.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("(cherry picked from commit ") {
            let sha = rest.trim_end_matches(')').trim();
            if sha.len() >= 7
                && sha.chars().all(|c| c.is_ascii_hexdigit())
                && let Ok(full_sha) = git_in_repo(repo_path, &["rev-parse", sha])
            {
                return Some(full_sha);
            }
        }
    }

    // Method 2: Check .git/CHERRY_PICK_HEAD (available during cherry-pick operation)
    let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"]).ok()?;
    let git_dir = std::path::PathBuf::from(&git_dir_str);
    let abs_git_dir = if git_dir.is_relative() {
        repo_path.join(&git_dir)
    } else {
        git_dir
    };
    let cherry_pick_head = abs_git_dir.join("CHERRY_PICK_HEAD");
    if let Ok(source_sha) = std::fs::read_to_string(&cherry_pick_head) {
        let sha = source_sha.trim();
        if !sha.is_empty() && sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(sha.to_string());
        }
    }

    // Method 3: Patch-id matching (TODO - complex fallback)
    // Could use `git patch-id` to find commits with matching diffs in recent history.
    // For now, we return None and document the limitation.

    // Known limitation: cherry-pick without -x and after CHERRY_PICK_HEAD is cleaned up
    // will not find the source commit. Users should use `git cherry-pick -x` for proper
    // attribution tracking.
    None
}

/// Migrate working log from old base commit to new base commit.
fn migrate_working_log(repo_path: &Path, old_head: &str, new_head: &str) -> Result<(), String> {
    let git_dir_str = git_in_repo(repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir_path = PathBuf::from(&git_dir_str);
    let git_dir = if git_dir_path.is_relative() {
        let abs = repo_path.join(&git_dir_path);
        std::fs::canonicalize(&abs).unwrap_or(abs)
    } else {
        std::fs::canonicalize(&git_dir_path).unwrap_or(git_dir_path)
    };

    let old_log_dir = git_dir.join("ai").join("working_logs").join(old_head);
    let new_log_dir = git_dir.join("ai").join("working_logs").join(new_head);

    if old_log_dir.exists()
        && !new_log_dir.exists()
        && let Err(e) = std::fs::rename(&old_log_dir, &new_log_dir)
    {
        eprintln!(
            "[git-ai daemon] working log rename failed ({}), attempting copy",
            e
        );
        copy_dir_recursive(&old_log_dir, &new_log_dir)?;
        let _ = std::fs::remove_dir_all(&old_log_dir);
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| format!("readdir {}: {}", src.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry: {}", e))?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path).map_err(|e| format!("copy: {}", e))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Range-diff parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_identical_mapping() {
        let output = "1:  abc1234 = 1:  def5678 Some commit message\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_modified_mapping() {
        let output = "1:  abc1234 ! 1:  def5678 Modified commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_deleted_mapping() {
        let output = "1:  abc1234 < -:  ------- Dropped commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Deleted);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert!(mappings[0].new.is_none());
    }

    #[test]
    fn parse_added_mapping() {
        let output = "-:  ------- > 1:  def5678 New commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Added);
        assert!(mappings[0].original.is_none());
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_multi_commit_rebase() {
        let output = "\
1:  aaa1111 = 1:  bbb1111 First commit
2:  aaa2222 ! 2:  bbb2222 Second commit (modified)
3:  aaa3333 < -:  ------- Third commit (dropped)
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[1].kind, MappingKind::Modified);
        assert_eq!(mappings[2].kind, MappingKind::Deleted);
    }

    #[test]
    fn parse_squash_pattern() {
        let output = "\
1:  aaa1111 < -:  ------- First commit (squashed away)
2:  aaa2222 < -:  ------- Second commit (squashed away)
3:  aaa3333 ! 1:  bbb1111 Squash result
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 3);

        let groups = detect_squash_groups(&mappings);
        assert_eq!(groups.len(), 1);
        let group = groups.get("bbb1111").unwrap();
        assert_eq!(group.deleted_originals, vec!["aaa1111", "aaa2222"]);
        assert_eq!(group.matched_original.as_deref(), Some("aaa3333"));
    }

    #[test]
    fn parse_skips_indented_diff_content() {
        let output = "\
1:  abc1234 ! 1:  def5678 Modified commit
    diff --git a/file.txt b/file.txt
    --- a/file.txt
    +++ b/file.txt
    @@ -1,3 +1,3 @@
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
    }

    #[test]
    fn parse_empty_output() {
        let mappings = parse_range_diff_output("").unwrap();
        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_full_length_shas() {
        let output = "1:  a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 = 1:  f6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5 Commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].original.as_deref(),
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
        );
    }

    #[test]
    fn parse_operator_in_subject() {
        let output = "1:  abc1234 ! 1:  def5678 set x = 5\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
    }

    // -----------------------------------------------------------------------
    // Diff-based attribution transfer tests
    // -----------------------------------------------------------------------

    #[test]
    fn diff_transfer_identical_content() {
        let content = "line1\nline2\nline3\n";
        let attrs = vec![LineAttribution {
            start_line: 1,
            end_line: 3,
            author_id: "ai1".to_string(),
            overrode: None,
        }];

        let result = diff_based_line_attribution_transfer(content, content, &attrs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[2].start_line, 3);
    }

    #[test]
    fn diff_transfer_prepend_header() {
        let old = "fn base() {}\nfn a1() {}\nfn a2() {}";
        let new = "// header\nfn base() {}\nfn a1() {}\nfn a2() {}";

        let attrs = vec![
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old, new, &attrs);
        // AI lines should shift by +1
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_line, 3); // fn a1 shifted from line 2 to 3
        assert_eq!(result[1].start_line, 4); // fn a2 shifted from line 3 to 4
    }

    #[test]
    fn diff_transfer_deleted_lines() {
        let old = "line1\nline2\nline3\nline4";
        let new = "line1\nline4";

        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 3,
                author_id: "ai2".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 4,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old, new, &attrs);
        // line2, line3 deleted; line1 stays at 1, line4 moves to 2
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[0].author_id, "ai1");
        assert_eq!(result[1].start_line, 2);
        assert_eq!(result[1].author_id, "ai1");
    }

    #[test]
    fn diff_transfer_inserted_lines_no_attribution() {
        let old = "line1\nline2";
        let new = "line1\nnew_line\nline2";

        let attrs = vec![LineAttribution {
            start_line: 1,
            end_line: 2,
            author_id: "ai1".to_string(),
            overrode: None,
        }];

        let result = diff_based_line_attribution_transfer(old, new, &attrs);
        // line1 stays at 1, new_line has no attribution, line2 moves to 3
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].start_line, 1);
        assert_eq!(result[1].start_line, 3);
    }

    // -----------------------------------------------------------------------
    // Attestation builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn build_attestation_merges_ranges() {
        let attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 10,
                end_line: 10,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = build_file_attestation("test.rs", &attrs).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].line_ranges.len(), 2);
        assert_eq!(result.entries[0].line_ranges[0], LineRange::Range(1, 3));
        assert_eq!(result.entries[0].line_ranges[1], LineRange::Single(10));
    }

    #[test]
    fn build_attestation_skips_human() {
        let attrs = vec![LineAttribution {
            start_line: 1,
            end_line: 5,
            author_id: "human".to_string(),
            overrode: None,
        }];

        let result = build_file_attestation("test.rs", &attrs);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Overlay attribution tests
    // -----------------------------------------------------------------------

    #[test]
    fn overlay_splits_partial_overlap() {
        let mut attrs = vec![LineAttribution {
            start_line: 1,
            end_line: 10,
            author_id: "ai1".to_string(),
            overrode: None,
        }];

        overlay_attribution(&mut attrs, 5, 7, "ai2".to_string());

        // Should have: 1-4 ai1, 5-7 ai2, 8-10 ai1
        attrs.sort_by_key(|a| a.start_line);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].start_line, 1);
        assert_eq!(attrs[0].end_line, 4);
        assert_eq!(attrs[0].author_id, "ai1");
        assert_eq!(attrs[1].start_line, 5);
        assert_eq!(attrs[1].end_line, 7);
        assert_eq!(attrs[1].author_id, "ai2");
        assert_eq!(attrs[2].start_line, 8);
        assert_eq!(attrs[2].end_line, 10);
        assert_eq!(attrs[2].author_id, "ai1");
    }

    // -----------------------------------------------------------------------
    // Remap base_commit_sha tests
    // -----------------------------------------------------------------------

    #[test]
    fn remap_base_commit_sha_simple() {
        let note = r#"---
{
  "schema_version": "authorship/3.0.0",
  "base_commit_sha": "old_sha_here",
  "prompts": {}
}"#;
        let result = remap_base_commit_sha(note, "new_sha_value");
        assert!(result.contains("\"new_sha_value\""));
        assert!(!result.contains("old_sha_here"));
    }

    // -----------------------------------------------------------------------
    // Integration tests (require real git repos)
    // -----------------------------------------------------------------------

    fn setup_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_path_buf();

        Command::new("git")
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["config", "user.name", "Test"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["config", "user.email", "t@t.com"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        (dir, repo_path)
    }

    fn commit_file(repo_path: &Path, name: &str, content: &str, msg: &str) -> String {
        std::fs::write(repo_path.join(name), content).unwrap();
        Command::new("git")
            .current_dir(repo_path)
            .args(["add", name])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(repo_path)
            .args(["commit", "-m", msg])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        git_in_repo(repo_path, &["rev-parse", "HEAD"]).unwrap()
    }

    fn make_note(repo_path: &Path, sha: &str, content: &str) {
        Command::new("git")
            .current_dir(repo_path)
            .args(["notes", "--ref=ai", "add", "-f", "-m", content, sha])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
    }

    fn create_test_authorship_note(base_commit_sha: &str, file: &str, lines: (u32, u32)) -> String {
        let log = AuthorshipLog::new(Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: Some("test".to_string()),
            base_commit_sha: base_commit_sha.to_string(),
            prompts: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "abcdef1234567890".to_string(),
                    crate::core::authorship_log::PromptRecord {
                        agent_id: crate::core::authorship_log::AgentId {
                            tool: "test".to_string(),
                            id: "sess_1".to_string(),
                            model: "gpt-4".to_string(),
                        },
                        human_author: None,
                        messages_url: None,
                        total_additions: 5,
                        total_deletions: 0,
                        accepted_lines: 5,
                        overriden_lines: 0,
                        custom_attributes: None,
                    },
                );
                m
            },
            sessions: std::collections::BTreeMap::new(),
            humans: std::collections::BTreeMap::new(),
        });
        let mut log = log;
        log.attestations.push(FileAttestation {
            file_path: file.to_string(),
            entries: vec![AttestationEntry {
                hash: "abcdef1234567890".to_string(),
                line_ranges: vec![LineRange::Range(lines.0, lines.1)],
            }],
        });
        log.serialize_to_string()
    }

    #[test]
    fn test_amend_transfers_attribution_via_diff() {
        let (_dir, repo_path) = setup_repo();

        // Initial commit
        commit_file(&repo_path, "init.txt", "init\n", "initial");

        // Commit with AI attribution
        let old_sha = commit_file(&repo_path, "file.txt", "line1\nline2\nline3\n", "original");
        let note = create_test_authorship_note(&old_sha, "file.txt", (1, 3));
        make_note(&repo_path, &old_sha, &note);

        // Amend: add a line but keep existing lines
        std::fs::write(
            repo_path.join("file.txt"),
            "line1\nline2\nline3\nnew_line\n",
        )
        .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["add", "file.txt"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["commit", "--amend", "-m", "amended"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let result = process_amend(&repo_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // Verify the note was transferred with correct attribution
        let new_sha = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(old_sha, new_sha);
        let new_note = read_note(&repo_path, &new_sha).unwrap();
        let parsed = AuthorshipLog::deserialize_from_string(&new_note).unwrap();

        // Lines 1-3 should still be attributed
        assert!(!parsed.attestations.is_empty());
        let file_att = &parsed.attestations[0];
        assert_eq!(file_att.file_path, "file.txt");
        // The attribution should cover lines 1-3 (the original AI lines preserved)
        let total_lines: u32 = file_att
            .entries
            .iter()
            .flat_map(|e| &e.line_ranges)
            .map(|r| r.line_count())
            .sum();
        assert_eq!(total_lines, 3);
    }

    #[test]
    fn test_amend_skips_if_no_note() {
        let (_dir, repo_path) = setup_repo();

        commit_file(&repo_path, "init.txt", "init\n", "initial");
        commit_file(&repo_path, "file.txt", "content\n", "no note here");

        std::fs::write(repo_path.join("file.txt"), "amended\n").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["add", "file.txt"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["commit", "--amend", "-m", "amended"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let result = process_amend(&repo_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_rebase_transfers_notes_with_diff() {
        let (_dir, repo_path) = setup_repo();

        // Create base commit
        commit_file(&repo_path, "base.txt", "base\n", "base");

        // Create a feature branch
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "-b", "feature"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        let feature_sha = commit_file(
            &repo_path,
            "feat.txt",
            "feat_line1\nfeat_line2\n",
            "feature commit",
        );
        let note = create_test_authorship_note(&feature_sha, "feat.txt", (1, 2));
        make_note(&repo_path, &feature_sha, &note);

        // Add a commit to master
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "master"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        commit_file(&repo_path, "main.txt", "main\n", "main commit");

        // Rebase feature onto master
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "feature"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["rebase", "master"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let result = process_rebase(&repo_path);
        assert!(result.is_ok());
        assert!(result.unwrap() >= 1);

        // Verify note on new HEAD
        let new_sha = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(feature_sha, new_sha);
        let new_note = read_note(&repo_path, &new_sha).unwrap();
        let parsed = AuthorshipLog::deserialize_from_string(&new_note).unwrap();
        assert!(!parsed.attestations.is_empty());
        assert_eq!(parsed.attestations[0].file_path, "feat.txt");
    }

    #[test]
    fn test_rebase_handles_content_shift() {
        let (_dir, repo_path) = setup_repo();

        // Create base with a shared file
        commit_file(&repo_path, "shared.txt", "base_line\n", "base");

        // Feature branch: add lines after base_line
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "-b", "feature"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        let feature_sha = commit_file(
            &repo_path,
            "shared.txt",
            "base_line\nai_line1\nai_line2\n",
            "feature: add AI lines",
        );
        // Note: AI attributed lines 2-3
        let note = create_test_authorship_note(&feature_sha, "shared.txt", (2, 3));
        make_note(&repo_path, &feature_sha, &note);

        // Master: prepend a header line
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "master"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        commit_file(
            &repo_path,
            "shared.txt",
            "header\nbase_line\n",
            "main: add header",
        );

        // Rebase feature onto master
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "feature"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        let rebase_output = Command::new("git")
            .current_dir(&repo_path)
            .args(["rebase", "master"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        if !rebase_output.status.success() {
            // Rebase conflict: skip this test case
            return;
        }

        let result = process_rebase(&repo_path);
        assert!(result.is_ok());

        let new_sha = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();
        let new_note = read_note(&repo_path, &new_sha);
        assert!(new_note.is_some());

        let parsed = AuthorshipLog::deserialize_from_string(&new_note.unwrap()).unwrap();
        assert!(!parsed.attestations.is_empty());

        // After rebase, shared.txt should be: header\nbase_line\nai_line1\nai_line2\n
        // AI lines moved from 2-3 to 3-4
        let file_att = &parsed.attestations[0];
        let all_lines: Vec<u32> = file_att
            .entries
            .iter()
            .flat_map(|e| e.line_ranges.iter().flat_map(|r| r.expand()))
            .collect();
        assert_eq!(all_lines.len(), 2);
        assert!(all_lines.contains(&3));
        assert!(all_lines.contains(&4));
    }

    #[test]
    fn test_cherry_pick_transfers_note() {
        let (_dir, repo_path) = setup_repo();

        // Base
        commit_file(&repo_path, "base.txt", "base\n", "base");

        // Feature branch with noted commit
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "-b", "feature"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        let feature_sha = commit_file(
            &repo_path,
            "cherry.txt",
            "cherry_line1\ncherry_line2\n",
            "cherry commit",
        );
        let note = create_test_authorship_note(&feature_sha, "cherry.txt", (1, 2));
        make_note(&repo_path, &feature_sha, &note);

        // Switch to master and cherry-pick with -x
        Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "master"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(["cherry-pick", "-x", &feature_sha])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let result = process_cherry_pick(&repo_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        let new_sha = git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(feature_sha, new_sha);
        let new_note = read_note(&repo_path, &new_sha).unwrap();
        let parsed = AuthorshipLog::deserialize_from_string(&new_note).unwrap();
        assert!(!parsed.attestations.is_empty());
    }

    #[test]
    fn test_working_log_migration() {
        let (_dir, repo_path) = setup_repo();

        let old_sha = commit_file(&repo_path, "file.txt", "content\n", "first");

        // Create a fake working log directory
        let git_dir = repo_path.join(".git");
        let old_log_dir = git_dir.join("ai").join("working_logs").join(&old_sha);
        std::fs::create_dir_all(&old_log_dir).unwrap();
        std::fs::write(old_log_dir.join("test.json"), "{}").unwrap();

        let new_sha = "deadbeefdeadbeefdeadbeef";
        migrate_working_log(&repo_path, &old_sha, new_sha).unwrap();

        let new_log_dir = git_dir.join("ai").join("working_logs").join(new_sha);
        assert!(new_log_dir.exists());
        assert!(new_log_dir.join("test.json").exists());
        assert!(!old_log_dir.exists());
    }
}
