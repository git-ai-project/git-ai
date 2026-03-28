use crate::authorship::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::authorship::working_log::Checkpoint;
use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use crate::utils::debug_log;
use serde_json;
use std::collections::{HashMap, HashSet};

// Modern refspecs without force to enable proper merging
pub const AI_AUTHORSHIP_REFNAME: &str = "ai";
pub const AI_AUTHORSHIP_PUSH_REFSPEC: &str = "refs/notes/ai:refs/notes/ai";

// Sharded notes: 256 independent refs keyed by first 2 hex chars of the annotated
// commit SHA. This dramatically reduces push contention on busy monorepos.
// Uses "ai-s" prefix (not "ai/" which conflicts with "ai" as a loose ref file).
pub const AI_SHARDED_NOTES_PREFIX: &str = "refs/notes/ai-s/";

/// Return the shard suffix (first 2 hex chars) for a given commit SHA.
/// Panics if commit_sha is shorter than 2 characters (should never happen with real SHAs).
pub fn shard_for_commit(commit_sha: &str) -> &str {
    debug_assert!(
        commit_sha.len() >= 2,
        "shard_for_commit called with SHA shorter than 2 chars: {:?}",
        commit_sha
    );
    if commit_sha.len() < 2 {
        return "00";
    }
    &commit_sha[..2]
}

/// Return the full sharded ref (e.g. "refs/notes/ai-s/ab") for a commit.
fn sharded_ref_for_commit(commit_sha: &str) -> String {
    format!(
        "{}{}",
        AI_SHARDED_NOTES_PREFIX,
        shard_for_commit(commit_sha)
    )
}

/// Return the --ref= argument value for the shard of a commit (e.g. "ai-s/ab").
fn sharded_refname_for_commit(commit_sha: &str) -> String {
    format!("ai-s/{}", shard_for_commit(commit_sha))
}

fn sharded_notes_enabled() -> bool {
    if crate::daemon::daemon_process_active() {
        let config = crate::config::Config::fresh();
        config.get_feature_flags().sharded_notes
    } else {
        crate::config::Config::get()
            .get_feature_flags()
            .sharded_notes
    }
}

pub fn notes_add(
    repo: &Repository,
    commit_sha: &str,
    note_content: &str,
) -> Result<(), GitAiError> {
    // Always write to the legacy ref (backward compat for old clients)
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("add".to_string());
    args.push("-f".to_string());
    args.push("-F".to_string());
    args.push("-".to_string());
    args.push(commit_sha.to_string());
    exec_git_stdin(&args, note_content.as_bytes())?;

    // Dual-write to shard ref when sharded notes are enabled
    if sharded_notes_enabled() {
        let shard_ref = sharded_refname_for_commit(commit_sha);
        let mut args = repo.global_args_for_exec();
        args.push("notes".to_string());
        args.push(format!("--ref={}", shard_ref));
        args.push("add".to_string());
        args.push("-f".to_string());
        args.push("-F".to_string());
        args.push("-".to_string());
        args.push(commit_sha.to_string());
        // Best-effort: shard write failure should not block the operation
        if let Err(e) = exec_git_stdin(&args, note_content.as_bytes()) {
            debug_log(&format!("shard notes_add failed for {}: {}", shard_ref, e));
        }
    }

    crate::authorship::git_ai_hooks::post_notes_updated_single(repo, commit_sha, note_content);
    Ok(())
}

fn notes_path_for_object(oid: &str) -> String {
    if oid.len() <= 2 {
        oid.to_string()
    } else {
        format!("{}/{}", &oid[..2], &oid[2..])
    }
}

fn flat_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!("refs/notes/ai:{}", commit_sha)
}

fn fanout_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!("refs/notes/ai:{}", notes_path_for_object(commit_sha))
}

fn sharded_flat_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!("{}:{}", sharded_ref_for_commit(commit_sha), commit_sha)
}

fn sharded_fanout_note_pathspec_for_commit(commit_sha: &str) -> String {
    format!(
        "{}:{}",
        sharded_ref_for_commit(commit_sha),
        notes_path_for_object(commit_sha)
    )
}

fn parse_batch_check_blob_oid(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let oid = parts.first().copied().unwrap_or_default();
    let valid_oid_len = oid.len() == 40 || oid.len() == 64;
    if parts.len() >= 2
        && parts[1] == "blob"
        && valid_oid_len
        && oid.as_bytes().iter().all(|b| b.is_ascii_hexdigit())
    {
        Some(oid.to_string())
    } else {
        None
    }
}

fn parse_cat_file_batch_output_with_oids(
    data: &[u8],
) -> Result<HashMap<String, String>, GitAiError> {
    let mut results = HashMap::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end])?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 2 {
            pos = header_end + 1;
            continue;
        }

        let oid = parts[0].to_string();
        if parts[1] == "missing" {
            pos = header_end + 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            continue;
        }

        let size: usize = parts[2]
            .parse()
            .map_err(|e| GitAiError::Generic(format!("Invalid size in cat-file output: {}", e)))?;

        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            return Err(GitAiError::Generic(
                "Malformed cat-file --batch output: truncated content".to_string(),
            ));
        }

        let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
        results.insert(oid, content);

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    Ok(results)
}

fn batch_read_blob_contents(
    repo: &Repository,
    blob_oids: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if blob_oids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = blob_oids.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    parse_cat_file_batch_output_with_oids(&output.stdout)
}

/// Resolve authorship note blob OIDs for a set of commits using one batched cat-file call.
///
/// Returns a map of commit SHA -> note blob SHA for commits that currently have notes.
/// When sharded notes are enabled, checks shard refs first and falls back to the legacy ref.
pub fn note_blob_oids_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let sharded = sharded_notes_enabled();

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch-check".to_string());

    // When sharded: 4 lines per commit (shard flat, shard fanout, legacy flat, legacy fanout)
    // When not sharded: 2 lines per commit (legacy flat, legacy fanout)
    let mut stdin_data = String::new();
    for commit_sha in commit_shas {
        if sharded {
            stdin_data.push_str(&sharded_flat_note_pathspec_for_commit(commit_sha));
            stdin_data.push('\n');
            stdin_data.push_str(&sharded_fanout_note_pathspec_for_commit(commit_sha));
            stdin_data.push('\n');
        }
        stdin_data.push_str(&flat_note_pathspec_for_commit(commit_sha));
        stdin_data.push('\n');
        stdin_data.push_str(&fanout_note_pathspec_for_commit(commit_sha));
        stdin_data.push('\n');
    }

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let mut result = HashMap::new();

    for commit_sha in commit_shas {
        if sharded {
            let shard_flat = lines.next().unwrap_or_default();
            let shard_fanout = lines.next().unwrap_or_default();
            if let Some(oid) = parse_batch_check_blob_oid(shard_flat)
                .or_else(|| parse_batch_check_blob_oid(shard_fanout))
            {
                // Consume legacy lines but don't use them
                let _ = lines.next();
                let _ = lines.next();
                result.insert(commit_sha.clone(), oid);
                continue;
            }
        }

        let Some(flat_line) = lines.next() else {
            break;
        };
        let fanout_line = lines.next().unwrap_or_default();

        if let Some(oid) = parse_batch_check_blob_oid(flat_line)
            .or_else(|| parse_batch_check_blob_oid(fanout_line))
        {
            result.insert(commit_sha.clone(), oid);
        }
    }

    Ok(result)
}

/// Resolve the current tip of a notes ref, returning None if the ref doesn't exist.
fn resolve_notes_tip(repo: &Repository, notes_ref: &str) -> Result<Option<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push("--verify".to_string());
    args.push(notes_ref.to_string());
    match exec_git(&args) {
        Ok(output) => Ok(Some(String::from_utf8(output.stdout)?.trim().to_string())),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        })
        | Err(GitAiError::GitCliError { code: Some(1), .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Append a fast-import commit block for a notes ref to `script`.
/// `entries` are (mark_index, commit_sha) pairs — marks must already be defined as blobs.
fn append_notes_commit_block(
    script: &mut Vec<u8>,
    notes_ref: &str,
    existing_tip: Option<&str>,
    entries: &[(usize, &str)],
    now: u64,
) {
    script.extend_from_slice(format!("commit {}\n", notes_ref).as_bytes());
    script.extend_from_slice(format!("committer git-ai <git-ai@local> {} +0000\n", now).as_bytes());
    script.extend_from_slice(b"data 0\n");
    if let Some(tip) = existing_tip {
        script.extend_from_slice(format!("from {}\n", tip).as_bytes());
    }

    for (mark_idx, commit_sha) in entries {
        let fanout_path = notes_path_for_object(commit_sha);
        let flat_path = *commit_sha;
        if flat_path != fanout_path {
            script.extend_from_slice(format!("D {}\n", flat_path).as_bytes());
        }
        script.extend_from_slice(format!("D {}\n", fanout_path).as_bytes());
        script.extend_from_slice(format!("M 100644 :{} {}\n", mark_idx, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");
}

pub fn notes_add_batch(repo: &Repository, entries: &[(String, String)]) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }

    let existing_notes_tip = resolve_notes_tip(repo, "refs/notes/ai")?;

    let mut deduped_entries: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (commit_sha, note_content) in entries.iter().rev() {
        if seen.insert(commit_sha.as_str()) {
            deduped_entries.push((commit_sha.clone(), note_content.clone()));
        }
    }
    deduped_entries.reverse();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {}", e)))?
        .as_secs();

    let sharded = sharded_notes_enabled();

    // Resolve shard tips upfront if sharding is enabled
    let shard_tips: HashMap<String, Option<String>> = if sharded {
        let mut tips = HashMap::new();
        for (commit_sha, _) in &deduped_entries {
            let shard_ref = sharded_ref_for_commit(commit_sha);
            if !tips.contains_key(&shard_ref) {
                tips.insert(shard_ref.clone(), resolve_notes_tip(repo, &shard_ref)?);
            }
        }
        tips
    } else {
        HashMap::new()
    };

    let mut script = Vec::<u8>::new();

    // Emit blob definitions with marks
    for (idx, (_commit_sha, note_content)) in deduped_entries.iter().enumerate() {
        script.extend_from_slice(b"blob\n");
        script.extend_from_slice(format!("mark :{}\n", idx + 1).as_bytes());
        script.extend_from_slice(format!("data {}\n", note_content.len()).as_bytes());
        script.extend_from_slice(note_content.as_bytes());
        script.extend_from_slice(b"\n");
    }

    // Legacy commit block (always written)
    let legacy_entries: Vec<(usize, &str)> = deduped_entries
        .iter()
        .enumerate()
        .map(|(idx, (sha, _))| (idx + 1, sha.as_str()))
        .collect();
    append_notes_commit_block(
        &mut script,
        "refs/notes/ai",
        existing_notes_tip.as_deref(),
        &legacy_entries,
        now,
    );

    // Shard commit blocks (one per unique shard, when enabled)
    if sharded {
        // Group entries by shard
        let mut shard_entries: HashMap<String, Vec<(usize, &str)>> = HashMap::new();
        for (idx, (commit_sha, _)) in deduped_entries.iter().enumerate() {
            let shard_ref = sharded_ref_for_commit(commit_sha);
            shard_entries
                .entry(shard_ref)
                .or_default()
                .push((idx + 1, commit_sha.as_str()));
        }

        // Sort shard keys for deterministic output
        let mut shard_keys: Vec<&String> = shard_entries.keys().collect();
        shard_keys.sort();

        for shard_ref in shard_keys {
            let entries = &shard_entries[shard_ref];
            let tip = shard_tips.get(shard_ref).and_then(|t| t.as_deref());
            append_notes_commit_block(&mut script, shard_ref, tip, entries, now);
        }
    }

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;
    crate::authorship::git_ai_hooks::post_notes_updated(repo, &deduped_entries);

    Ok(())
}

/// Append a fast-import commit block that references existing blob OIDs (no marks needed).
fn append_notes_blob_commit_block(
    script: &mut Vec<u8>,
    notes_ref: &str,
    existing_tip: Option<&str>,
    entries: &[(&str, &str)], // (commit_sha, blob_oid)
    now: u64,
) {
    script.extend_from_slice(format!("commit {}\n", notes_ref).as_bytes());
    script.extend_from_slice(format!("committer git-ai <git-ai@local> {} +0000\n", now).as_bytes());
    script.extend_from_slice(b"data 0\n");
    if let Some(tip) = existing_tip {
        script.extend_from_slice(format!("from {}\n", tip).as_bytes());
    }

    for (commit_sha, blob_oid) in entries {
        let fanout_path = notes_path_for_object(commit_sha);
        if *commit_sha != fanout_path {
            script.extend_from_slice(format!("D {}\n", commit_sha).as_bytes());
        }
        script.extend_from_slice(format!("D {}\n", fanout_path).as_bytes());
        script.extend_from_slice(format!("M 100644 {} {}\n", blob_oid, fanout_path).as_bytes());
    }
    script.extend_from_slice(b"\n");
}

/// Batch-attach existing note blobs to commits without rewriting blob contents.
///
/// Each entry is (commit_sha, existing_note_blob_oid).
#[allow(dead_code)]
pub fn notes_add_blob_batch(
    repo: &Repository,
    entries: &[(String, String)],
) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }

    let existing_notes_tip = resolve_notes_tip(repo, "refs/notes/ai")?;

    let mut deduped_entries: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (commit_sha, blob_oid) in entries.iter().rev() {
        if seen.insert(commit_sha.as_str()) {
            deduped_entries.push((commit_sha.clone(), blob_oid.clone()));
        }
    }
    deduped_entries.reverse();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| GitAiError::Generic(format!("System clock before epoch: {}", e)))?
        .as_secs();

    let sharded = sharded_notes_enabled();

    // Resolve shard tips upfront
    let shard_tips: HashMap<String, Option<String>> = if sharded {
        let mut tips = HashMap::new();
        for (commit_sha, _) in &deduped_entries {
            let shard_ref = sharded_ref_for_commit(commit_sha);
            if !tips.contains_key(&shard_ref) {
                tips.insert(shard_ref.clone(), resolve_notes_tip(repo, &shard_ref)?);
            }
        }
        tips
    } else {
        HashMap::new()
    };

    let mut script = Vec::<u8>::new();

    // Legacy commit block
    let legacy_entries: Vec<(&str, &str)> = deduped_entries
        .iter()
        .map(|(sha, oid)| (sha.as_str(), oid.as_str()))
        .collect();
    append_notes_blob_commit_block(
        &mut script,
        "refs/notes/ai",
        existing_notes_tip.as_deref(),
        &legacy_entries,
        now,
    );

    // Shard commit blocks
    if sharded {
        let mut shard_entries: HashMap<String, Vec<(&str, &str)>> = HashMap::new();
        for (commit_sha, blob_oid) in &deduped_entries {
            let shard_ref = sharded_ref_for_commit(commit_sha);
            shard_entries
                .entry(shard_ref)
                .or_default()
                .push((commit_sha.as_str(), blob_oid.as_str()));
        }

        let mut shard_keys: Vec<&String> = shard_entries.keys().collect();
        shard_keys.sort();

        for shard_ref in shard_keys {
            let entries = &shard_entries[shard_ref];
            let tip = shard_tips.get(shard_ref).and_then(|t| t.as_deref());
            append_notes_blob_commit_block(&mut script, shard_ref, tip, entries, now);
        }
    }

    let mut fast_import_args = repo.global_args_for_exec();
    fast_import_args.push("fast-import".to_string());
    fast_import_args.push("--quiet".to_string());
    exec_git_stdin(&fast_import_args, &script)?;

    let has_post_notes_updated_hooks = crate::config::Config::get()
        .git_ai_hook_commands("post_notes_updated")
        .is_some_and(|commands| !commands.is_empty());
    if has_post_notes_updated_hooks {
        let hook_entries = (|| -> Result<Vec<(String, String)>, GitAiError> {
            let mut unique_blob_oids: Vec<String> = deduped_entries
                .iter()
                .map(|(_commit_sha, blob_oid)| blob_oid.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            unique_blob_oids.sort();
            let blob_contents = batch_read_blob_contents(repo, &unique_blob_oids)?;

            Ok(deduped_entries
                .iter()
                .filter_map(|(commit_sha, blob_oid)| {
                    blob_contents
                        .get(blob_oid)
                        .map(|note_content| (commit_sha.clone(), note_content.clone()))
                })
                .collect())
        })();
        match hook_entries {
            Ok(entries) if !entries.is_empty() => {
                crate::authorship::git_ai_hooks::post_notes_updated(repo, &entries)
            }
            Ok(_) => {}
            Err(e) => debug_log(&format!(
                "Failed to prepare post_notes_updated payload for notes_add_blob_batch: {}",
                e
            )),
        }
    }

    Ok(())
}

// Check which commits from the given list have authorship notes.
// Uses git cat-file --batch-check to efficiently check multiple commits in one invocation.
// Returns a Vec of CommitAuthorship for each commit.
#[derive(Debug, Clone)]

pub enum CommitAuthorship {
    NoLog {
        sha: String,
        git_author: String,
    },
    Log {
        sha: String,
        git_author: String,
        authorship_log: AuthorshipLog,
    },
}
pub fn get_commits_with_notes_from_list(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<Vec<CommitAuthorship>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(Vec::new());
    }

    // Get the git authors for all commits using git rev-list
    // This approach works in both bare and normal repositories
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--no-walk".to_string());
    args.push("--pretty=format:%H%n%an%n%ae".to_string());
    for sha in commit_shas {
        args.push(sha.clone());
    }

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse git rev-list output".to_string()))?;

    let mut commit_authors = HashMap::new();
    let lines: Vec<&str> = stdout.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Skip commit headers (start with "commit ")
        if line.starts_with("commit ") {
            i += 1;
            if i + 2 < lines.len() {
                let sha = lines[i].to_string();
                let name = lines[i + 1].to_string();
                let email = lines[i + 2].to_string();
                let author = format!("{} <{}>", name, email);
                commit_authors.insert(sha, author);
                i += 3;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }

    // Build the result Vec
    let mut result = Vec::new();
    for sha in commit_shas {
        let git_author = commit_authors
            .get(sha)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());

        // Check if this commit has a note by trying to show it
        if let Some(authorship_log) = get_authorship(repo, sha) {
            result.push(CommitAuthorship::Log {
                sha: sha.clone(),
                git_author,
                authorship_log,
            });
        } else {
            result.push(CommitAuthorship::NoLog {
                sha: sha.clone(),
                git_author,
            });
        }
    }

    Ok(result)
}

// Show an authorship note and return its JSON content if found, or None if it doesn't exist.
// When sharded notes are enabled, checks the shard ref first, then falls back to legacy.
pub fn show_authorship_note(repo: &Repository, commit_sha: &str) -> Option<String> {
    if sharded_notes_enabled() {
        let shard_ref = sharded_refname_for_commit(commit_sha);
        let mut args = repo.global_args_for_exec();
        args.push("notes".to_string());
        args.push(format!("--ref={}", shard_ref));
        args.push("show".to_string());
        args.push(commit_sha.to_string());

        if let Ok(output) = exec_git(&args) {
            let result = String::from_utf8(output.stdout)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if result.is_some() {
                return result;
            }
        }
    }

    // Fall back to legacy ref
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("show".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        Err(GitAiError::GitCliError { code: Some(1), .. }) => None,
        Err(_) => None,
    }
}

/// Return the subset of `commit_shas` that currently has an authorship note.
///
/// This uses a single `git notes --ref=ai list` invocation instead of one
/// `git notes show` call per commit.
pub fn commits_with_authorship_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashSet<String>, GitAiError> {
    Ok(note_blob_oids_for_commits(repo, commit_shas)?
        .into_keys()
        .collect())
}

// Show an authorship note and return its JSON content if found, or None if it doesn't exist.
pub fn get_authorship(repo: &Repository, commit_sha: &str) -> Option<AuthorshipLog> {
    let content = show_authorship_note(repo, commit_sha)?;
    let mut authorship_log = AuthorshipLog::deserialize_from_string(&content).ok()?;
    // Keep metadata aligned with the commit where this note is attached.
    authorship_log.metadata.base_commit_sha = commit_sha.to_string();
    Some(authorship_log)
}

#[allow(dead_code)]
pub fn get_reference_as_working_log(
    repo: &Repository,
    commit_sha: &str,
) -> Result<Vec<Checkpoint>, GitAiError> {
    let content = show_authorship_note(repo, commit_sha)
        .ok_or_else(|| GitAiError::Generic("No authorship note found".to_string()))?;
    let working_log = serde_json::from_str(&content)?;
    Ok(working_log)
}

pub fn get_reference_as_authorship_log_v3(
    repo: &Repository,
    commit_sha: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let content = show_authorship_note(repo, commit_sha)
        .ok_or_else(|| GitAiError::Generic("No authorship note found".to_string()))?;

    // Try to deserialize as AuthorshipLog
    let mut authorship_log = match AuthorshipLog::deserialize_from_string(&content) {
        Ok(log) => log,
        Err(_) => {
            return Err(GitAiError::Generic(
                "Failed to parse authorship log".to_string(),
            ));
        }
    };

    // Check version compatibility
    if authorship_log.metadata.schema_version != AUTHORSHIP_LOG_VERSION {
        return Err(GitAiError::Generic(format!(
            "Unsupported authorship log version: {} (expected: {})",
            authorship_log.metadata.schema_version, AUTHORSHIP_LOG_VERSION
        )));
    }

    // Keep metadata aligned with the commit where this note is attached.
    authorship_log.metadata.base_commit_sha = commit_sha.to_string();

    Ok(authorship_log)
}

/// Sanitize a remote name to create a safe ref name
/// Replaces special characters with underscores to ensure valid ref names
fn sanitize_remote_name(remote: &str) -> String {
    remote
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Generate a tracking ref name for notes from a specific remote
/// Returns a ref like "refs/notes/ai-remote/origin"
///
/// SAFETY: These tracking refs are stored under refs/notes/ai-remote/* which:
/// - Won't be pushed by `git push` (only pushes refs/heads/* by default)
/// - Won't be pushed by `git push --all` (only pushes refs/heads/*)
/// - Won't be pushed by `git push --tags` (only pushes refs/tags/*)
/// - **WILL** be pushed by `git push --mirror` (usually only used for backups, etc.)
/// - **WILL** be pushed if user explicitly specifies refs/notes/ai-remote/* (extremely rare)
pub fn tracking_ref_for_remote(remote_name: &str) -> String {
    format!("refs/notes/ai-remote/{}", sanitize_remote_name(remote_name))
}

/// Check if a ref exists in the repository
pub fn ref_exists(repo: &Repository, ref_name: &str) -> bool {
    let mut args = repo.global_args_for_exec();
    args.push("show-ref".to_string());
    args.push("--verify".to_string());
    args.push("--quiet".to_string());
    args.push(ref_name.to_string());

    exec_git(&args).is_ok()
}

/// Merge notes from a source ref into refs/notes/ai
/// Uses the 'ours' strategy to combine notes without data loss
pub fn merge_notes_from_ref(repo: &Repository, source_ref: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push(format!("--ref={}", AI_AUTHORSHIP_REFNAME));
    args.push("merge".to_string());
    args.push("-s".to_string());
    args.push("ours".to_string());
    args.push("--quiet".to_string());
    args.push(source_ref.to_string());

    debug_log(&format!(
        "Merging notes from {} into refs/notes/ai",
        source_ref
    ));
    exec_git(&args)?;
    Ok(())
}

/// Fallback merge when `git notes merge -s ours` fails (e.g., due to git assertion
/// failures on corrupted/mixed-fanout notes trees). Implements the "ours" strategy
/// using a single `git fast-import` invocation that:
///   1. Creates a merge commit with both local and source as parents
///   2. Emits all notes via `N <blob> <object>` commands (source first, then local —
///      last writer wins, so local takes precedence on conflicts = "ours" strategy)
///   3. Produces a clean notes tree with correct fanout regardless of input tree format
///
/// This is O(1) git process invocations regardless of note count, which matters on
/// large monorepos with thousands of notes.
pub fn fallback_merge_notes_ours(repo: &Repository, source_ref: &str) -> Result<(), GitAiError> {
    let local_ref = format!("refs/notes/{}", AI_AUTHORSHIP_REFNAME);

    // 1. List notes from both refs
    let source_notes = list_all_notes(repo, source_ref)?;
    let local_notes = list_all_notes(repo, &local_ref)?;

    // 2. Resolve parent commit SHAs for the merge commit
    let local_commit = rev_parse(repo, &local_ref)?;
    let source_commit = rev_parse(repo, source_ref)?;

    // 3. Build the fast-import stream.
    //    Emit source (remote) notes first, then local notes. fast-import uses
    //    last-writer-wins for duplicate annotated objects, so local notes take
    //    precedence — this implements the "ours" merge strategy.
    let mut stream = String::new();
    stream.push_str(&format!("commit {}\n", local_ref));
    stream.push_str("committer git-ai <git-ai@noreply> 0 +0000\n");
    stream.push_str("data 23\nMerge notes (fallback)\n");
    stream.push_str(&format!("from {}\n", local_commit));
    stream.push_str(&format!("merge {}\n", source_commit));

    // Source notes first (will be overwritten by local on conflict)
    for (blob, object) in &source_notes {
        stream.push_str(&format!("N {} {}\n", blob, object));
    }
    // Local notes second (wins on conflict)
    for (blob, object) in &local_notes {
        stream.push_str(&format!("N {} {}\n", blob, object));
    }
    stream.push_str("done\n");

    // 4. Run fast-import
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "fast-import".to_string(),
        "--quiet".to_string(),
        "--done".to_string(),
    ]);
    exec_git_stdin(&args, stream.as_bytes())?;

    debug_log("fallback merge via fast-import completed successfully");
    Ok(())
}

/// List all notes on a given ref. Returns Vec<(note_blob_sha, annotated_object_sha)>.
fn list_all_notes(repo: &Repository, notes_ref: &str) -> Result<Vec<(String, String)>, GitAiError> {
    // `git notes list` uses --ref to specify which notes ref.
    // The --ref option prepends "refs/notes/" automatically, so for full refs
    // like "refs/notes/ai-remote/origin" we need to strip the prefix.
    let ref_arg = notes_ref.strip_prefix("refs/notes/").unwrap_or(notes_ref);

    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "notes".to_string(),
        format!("--ref={}", ref_arg),
        "list".to_string(),
    ]);

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse notes list output".to_string()))?;

    Ok(stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 {
                Some((parts[0].to_string(), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect())
}

/// Parse a revision to its SHA
fn rev_parse(repo: &Repository, rev: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&["rev-parse".to_string(), rev.to_string()]);
    let output = exec_git(&args)?;
    String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse rev-parse output".to_string()))
        .map(|s| s.trim().to_string())
}

/// Copy a ref to another location (used for initial setup of local notes from tracking ref)
pub fn copy_ref(repo: &Repository, source_ref: &str, dest_ref: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("update-ref".to_string());
    args.push(dest_ref.to_string());
    args.push(source_ref.to_string());

    debug_log(&format!("Copying ref {} to {}", source_ref, dest_ref));
    exec_git(&args)?;
    Ok(())
}

/// Search AI notes for a pattern and return matching commit SHAs ordered by commit date (newest first)
/// Uses git grep to search through refs/notes/ai
pub fn grep_ai_notes(repo: &Repository, pattern: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("--no-pager".to_string());
    args.push("grep".to_string());
    args.push("-nI".to_string());
    args.push(pattern.to_string());
    args.push("refs/notes/ai".to_string());

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("Failed to parse git grep output".to_string()))?;

    // Parse output format: refs/notes/ai:ab/cdef123...:line_number:matched_content
    // Extract the commit SHA from the path
    let mut shas = HashSet::new();
    for line in stdout.lines() {
        if let Some(path_and_rest) = line.strip_prefix("refs/notes/ai:")
            && let Some(path_end) = path_and_rest.find(':')
        {
            let path = &path_and_rest[..path_end];
            // Path is in format "ab/cdef123..." - combine to get full SHA
            let sha = path.replace('/', "");
            shas.insert(sha);
        }
    }

    // If we have multiple results, sort by commit date (newest first)
    if shas.len() > 1 {
        let sha_vec: Vec<String> = shas.into_iter().collect();
        let mut args = repo.global_args_for_exec();
        args.push("log".to_string());
        args.push("--format=%H".to_string());
        args.push("--date-order".to_string());
        args.push("--no-walk".to_string());
        for sha in &sha_vec {
            args.push(sha.clone());
        }

        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| GitAiError::Generic("Failed to parse git log output".to_string()))?;

        Ok(stdout.lines().map(|s| s.to_string()).collect())
    } else {
        Ok(shas.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_flags::FeatureFlags;
    use crate::git::test_utils::TmpRepo;
    use serial_test::serial;

    struct TestFeatureFlagsGuard;

    impl TestFeatureFlagsGuard {
        fn new() -> Self {
            crate::config::Config::clear_test_feature_flags();
            Self
        }
    }

    impl Drop for TestFeatureFlagsGuard {
        fn drop(&mut self) {
            crate::config::Config::clear_test_feature_flags();
        }
    }

    fn set_test_sharded_notes(enabled: bool) {
        let flags = FeatureFlags {
            sharded_notes: enabled,
            ..FeatureFlags::default()
        };
        crate::config::Config::set_test_feature_flags(flags);
    }

    fn create_commit_without_authorship_note(
        tmp_repo: &TmpRepo,
        filename: &str,
        contents: &str,
        message: &str,
    ) -> String {
        tmp_repo
            .write_file(filename, contents, true)
            .expect("write file for manual commit");

        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&["add".to_string(), ".".to_string()]);
        exec_git(&args).expect("stage files");

        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&["commit".to_string(), "-m".to_string(), message.to_string()]);
        exec_git(&args).expect("create manual commit");

        tmp_repo
            .get_head_commit_sha()
            .expect("get manual commit sha")
    }

    #[test]
    fn test_parse_batch_check_blob_oid_accepts_sha1_and_sha256() {
        let sha1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 10";
        let sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb blob 20";
        let invalid = "cccccccc blob 10";

        assert_eq!(
            parse_batch_check_blob_oid(sha1),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())
        );
        assert_eq!(
            parse_batch_check_blob_oid(sha256),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string())
        );
        assert_eq!(parse_batch_check_blob_oid(invalid), None);
    }

    #[test]
    fn test_notes_add_and_show_authorship_note() {
        // Create a temporary repository
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create a commit first
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to create initial commit");

        // Get the commit SHA
        let commit_sha = tmp_repo
            .get_head_commit_sha()
            .expect("Failed to get head commit SHA");

        // Test data - simple string content
        let note_content = "This is a test authorship note with some random content!";

        // Add the authorship note (force overwrite since commit_with_message already created one)
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note_content)
            .expect("Failed to add authorship note");

        // Read the note back
        let retrieved_content = show_authorship_note(tmp_repo.gitai_repo(), &commit_sha)
            .expect("Failed to retrieve authorship note");

        // Assert the content matches exactly
        assert_eq!(retrieved_content, note_content);

        // Test that non-existent commit returns None
        let non_existent_content = show_authorship_note(
            tmp_repo.gitai_repo(),
            "0000000000000000000000000000000000000000",
        );
        assert!(non_existent_content.is_none());
    }

    #[test]
    #[serial]
    fn test_notes_add_respects_sharded_notes_flag_for_writes() {
        let _guard = TestFeatureFlagsGuard::new();
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");
        set_test_sharded_notes(false);

        tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to create initial commit");
        let commit_a = tmp_repo
            .get_head_commit_sha()
            .expect("Failed to get first commit SHA");

        set_test_sharded_notes(false);
        notes_add(
            tmp_repo.gitai_repo(),
            &commit_a,
            "{\"mode\":\"legacy-only\"}",
        )
        .expect("add non-sharded note");

        let shard_ref_a = format!("refs/notes/ai-s/{}", &commit_a[..2]);
        assert!(
            !ref_exists(tmp_repo.gitai_repo(), &shard_ref_a),
            "default-off write should not create shard refs"
        );

        tmp_repo
            .write_file("second.txt", "second\n", true)
            .expect("write second file");
        tmp_repo
            .commit_with_message("Second commit")
            .expect("create second commit");
        let commit_b = tmp_repo
            .get_head_commit_sha()
            .expect("Failed to get second commit SHA");

        set_test_sharded_notes(true);
        notes_add(tmp_repo.gitai_repo(), &commit_b, "{\"mode\":\"sharded\"}")
            .expect("add sharded note");

        let shard_ref_b = format!("refs/notes/ai-s/{}", &commit_b[..2]);
        assert!(
            ref_exists(tmp_repo.gitai_repo(), &shard_ref_b),
            "enabling the flag should create the shard ref"
        );
    }

    #[test]
    #[serial]
    fn test_shard_reads_are_ignored_when_flag_is_disabled() {
        let _guard = TestFeatureFlagsGuard::new();
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");
        set_test_sharded_notes(false);
        let commit_sha = create_commit_without_authorship_note(
            &tmp_repo,
            "shard-only.txt",
            "shard only\n",
            "Manual commit without authorship note",
        );
        let shard_ref = format!("ai-s/{}", &commit_sha[..2]);

        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "notes".to_string(),
            format!("--ref={}", shard_ref),
            "add".to_string(),
            "-f".to_string(),
            "-m".to_string(),
            "{\"note\":\"shard-only\"}".to_string(),
            commit_sha.clone(),
        ]);
        exec_git(&args).expect("add shard-only note");

        set_test_sharded_notes(false);
        assert!(
            show_authorship_note(tmp_repo.gitai_repo(), &commit_sha).is_none(),
            "default-off reads should ignore shard-only notes"
        );
        assert!(
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_sha))
                .expect("resolve note blob oids")
                .is_empty(),
            "default-off blob lookup should ignore shard-only notes"
        );
        assert!(
            commits_with_authorship_notes(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_sha))
                .expect("resolve commit note presence")
                .is_empty(),
            "default-off note existence checks should ignore shard-only notes"
        );

        set_test_sharded_notes(true);
        assert_eq!(
            show_authorship_note(tmp_repo.gitai_repo(), &commit_sha).as_deref(),
            Some("{\"note\":\"shard-only\"}")
        );
        assert!(
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_sha))
                .expect("resolve note blob oids with sharding")
                .contains_key(&commit_sha),
            "enabled shard reads should discover shard-only notes"
        );
        assert!(
            commits_with_authorship_notes(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_sha))
                .expect("resolve commit note presence with sharding")
                .contains(&commit_sha),
            "enabled note existence checks should discover shard-only notes"
        );
    }

    #[test]
    fn test_notes_add_batch_writes_multiple_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        let entries = vec![
            (commit_a.clone(), "{\"note\":\"a\",\"value\":1}".to_string()),
            (commit_b.clone(), "{\"note\":\"b\",\"value\":2}".to_string()),
        ];

        notes_add_batch(tmp_repo.gitai_repo(), &entries).expect("batch notes add");

        let note_a = show_authorship_note(tmp_repo.gitai_repo(), &commit_a).expect("note A");
        let note_b = show_authorship_note(tmp_repo.gitai_repo(), &commit_b).expect("note B");
        assert!(note_a.contains("\"note\":\"a\""));
        assert!(note_b.contains("\"note\":\"b\""));
    }

    #[test]
    fn test_notes_add_blob_batch_reuses_existing_note_blob() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = commit_a.clone();
        let note_content = log.serialize_to_string().expect("serialize authorship log");
        notes_add(tmp_repo.gitai_repo(), &commit_a, &note_content).expect("add note A");

        let blob_oids =
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), std::slice::from_ref(&commit_a))
                .expect("resolve note blob oid");
        let blob_oid = blob_oids
            .get(&commit_a)
            .expect("blob oid for commit A")
            .clone();

        let blob_entry = (commit_b.clone(), blob_oid);
        notes_add_blob_batch(tmp_repo.gitai_repo(), std::slice::from_ref(&blob_entry))
            .expect("batch add blob-backed note");

        let raw_note_b = show_authorship_note(tmp_repo.gitai_repo(), &commit_b).expect("note B");
        assert_eq!(raw_note_b, note_content);

        let parsed_note_b =
            get_reference_as_authorship_log_v3(tmp_repo.gitai_repo(), &commit_b).expect("parse B");
        assert_eq!(parsed_note_b.metadata.base_commit_sha, commit_b);
    }

    #[test]
    fn test_sanitize_remote_name() {
        assert_eq!(sanitize_remote_name("origin"), "origin");
        assert_eq!(sanitize_remote_name("my-remote"), "my-remote");
        assert_eq!(sanitize_remote_name("remote_123"), "remote_123");
        assert_eq!(
            sanitize_remote_name("remote/with/slashes"),
            "remote_with_slashes"
        );
        assert_eq!(
            sanitize_remote_name("remote@with#special$chars"),
            "remote_with_special_chars"
        );
        assert_eq!(sanitize_remote_name("has spaces"), "has_spaces");
    }

    #[test]
    fn test_tracking_ref_for_remote() {
        assert_eq!(
            tracking_ref_for_remote("origin"),
            "refs/notes/ai-remote/origin"
        );
        assert_eq!(
            tracking_ref_for_remote("upstream"),
            "refs/notes/ai-remote/upstream"
        );
        assert_eq!(
            tracking_ref_for_remote("my-fork"),
            "refs/notes/ai-remote/my-fork"
        );
        // Special characters get sanitized
        assert_eq!(
            tracking_ref_for_remote("remote/with/slashes"),
            "refs/notes/ai-remote/remote_with_slashes"
        );
    }

    #[test]
    fn test_ref_exists() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create initial commit
        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("commit");

        // HEAD should exist
        assert!(ref_exists(tmp_repo.gitai_repo(), "HEAD"));

        // refs/heads/main (or master) should exist
        let branch_name = tmp_repo.current_branch().expect("get branch");
        assert!(ref_exists(
            tmp_repo.gitai_repo(),
            &format!("refs/heads/{}", branch_name)
        ));

        // Non-existent ref should not exist
        assert!(!ref_exists(
            tmp_repo.gitai_repo(),
            "refs/heads/nonexistent-branch"
        ));
        assert!(!ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-test"));
    }

    #[test]
    fn test_merge_notes_from_ref() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commits - they will auto-create notes on refs/notes/ai
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let _commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let _commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        // Create a third commit without checkpoint to ensure we have a commit without notes
        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");

        // Manually create commit without checkpoint
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&["add".to_string(), ".".to_string()]);
        crate::git::repository::exec_git(&args).expect("add files");

        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "commit".to_string(),
            "-m".to_string(),
            "Commit C".to_string(),
        ]);
        crate::git::repository::exec_git(&args).expect("commit");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Add note to commit C on a different ref
        let note_c = "{\"note\":\"c\"}";
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "notes".to_string(),
            "--ref=test".to_string(),
            "add".to_string(),
            "-f".to_string(),
            "-m".to_string(),
            note_c.to_string(),
            commit_c.clone(),
        ]);
        crate::git::repository::exec_git(&args).expect("add note C on test ref");

        // Verify initial state - commit C should not have note on refs/notes/ai
        let initial_note_c = show_authorship_note(tmp_repo.gitai_repo(), &commit_c);

        // Merge notes from refs/notes/test into refs/notes/ai
        merge_notes_from_ref(tmp_repo.gitai_repo(), "refs/notes/test").expect("merge notes");

        // After merge, commit C should have a note on refs/notes/ai
        let final_note_c = show_authorship_note(tmp_repo.gitai_repo(), &commit_c);

        // If initially had no note, should now have one. If it had one, should still have one.
        assert!(final_note_c.is_some() || initial_note_c.is_some());
    }

    #[test]
    fn test_copy_ref() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commit with note
        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note_content = "{\"test\":\"note\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note_content).expect("add note");

        // refs/notes/ai should exist
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai"));

        // refs/notes/ai-backup should not exist
        assert!(!ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-backup"));

        // Copy refs/notes/ai to refs/notes/ai-backup
        copy_ref(
            tmp_repo.gitai_repo(),
            "refs/notes/ai",
            "refs/notes/ai-backup",
        )
        .expect("copy ref");

        // Both should now exist and point to the same commit
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai"));
        assert!(ref_exists(tmp_repo.gitai_repo(), "refs/notes/ai-backup"));

        // Verify content is accessible from both refs
        let note_from_ai =
            show_authorship_note(tmp_repo.gitai_repo(), &commit_sha).expect("note from ai");

        // Read from backup ref
        let mut args = tmp_repo.gitai_repo().global_args_for_exec();
        args.extend_from_slice(&[
            "notes".to_string(),
            "--ref=ai-backup".to_string(),
            "show".to_string(),
            commit_sha.clone(),
        ]);
        let output = crate::git::repository::exec_git(&args).expect("show note from backup");
        let note_from_backup = String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string();

        assert_eq!(note_from_ai, note_from_backup);
    }

    #[test]
    fn test_grep_ai_notes_single_match() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note = "{\"tool\":\"cursor\",\"model\":\"claude-3-sonnet\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note).expect("add note");

        // Search for "cursor" should find the commit
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor").expect("grep");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], commit_sha);
    }

    #[test]
    fn test_grep_ai_notes_multiple_matches() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create three commits with notes
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");
        tmp_repo.commit_with_message("Commit C").expect("commit C");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Add notes with "cursor" to all three
        notes_add(tmp_repo.gitai_repo(), &commit_a, "{\"tool\":\"cursor\"}").expect("add note A");
        notes_add(tmp_repo.gitai_repo(), &commit_b, "{\"tool\":\"cursor\"}").expect("add note B");
        notes_add(tmp_repo.gitai_repo(), &commit_c, "{\"tool\":\"cursor\"}").expect("add note C");

        // Search should find all three, sorted by commit date (newest first)
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor").expect("grep");

        // Should find at least 3 commits (may find more from auto-created notes)
        assert!(
            results.len() >= 3,
            "Expected at least 3 results, got {}",
            results.len()
        );

        // Verify our three commits are in the results
        assert!(
            results.contains(&commit_a),
            "Results should contain commit A"
        );
        assert!(
            results.contains(&commit_b),
            "Results should contain commit B"
        );
        assert!(
            results.contains(&commit_c),
            "Results should contain commit C"
        );
    }

    #[test]
    fn test_grep_ai_notes_no_match() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        let note = "{\"tool\":\"cursor\"}";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, note).expect("add note");

        // Search for non-existent pattern
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "vscode");
        // grep may return empty or error if no matches, both are acceptable
        if let Ok(refs) = results {
            assert_eq!(refs.len(), 0);
        }
        // Err is also acceptable - git grep returns non-zero when no matches
    }

    #[test]
    fn test_grep_ai_notes_no_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");

        // No notes exist, search should return empty or error
        let results = grep_ai_notes(tmp_repo.gitai_repo(), "cursor");
        // grep may return empty or error if refs/notes/ai doesn't exist
        if let Ok(refs) = results {
            assert_eq!(refs.len(), 0);
        }
        // Err is also acceptable - refs/notes/ai may not exist yet
    }

    #[test]
    fn test_get_commits_with_notes_from_list() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Create commits - commit_with_message auto-creates authorship notes,
        // so all commits will have notes. This is expected behavior.
        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        tmp_repo.write_file("c.txt", "c\n", true).expect("write c");
        tmp_repo.commit_with_message("Commit C").expect("commit C");
        let commit_c = tmp_repo.get_head_commit_sha().expect("head C");

        // Get authorship for all commits
        let commit_list = vec![commit_a.clone(), commit_b.clone(), commit_c.clone()];
        let result = get_commits_with_notes_from_list(tmp_repo.gitai_repo(), &commit_list)
            .expect("get commits");

        assert_eq!(result.len(), 3);

        // All commits should have logs since commit_with_message creates them
        for (idx, commit_authorship) in result.iter().enumerate() {
            match commit_authorship {
                CommitAuthorship::Log {
                    sha,
                    git_author: _,
                    authorship_log: _,
                } => {
                    // This is expected - verify SHA matches
                    let expected_sha = &commit_list[idx];
                    assert_eq!(sha, expected_sha);
                }
                CommitAuthorship::NoLog { .. } => {
                    // Also acceptable if checkpoint system didn't run
                }
            }
        }
    }

    #[test]
    fn test_notes_path_for_object() {
        // Short SHA (edge case)
        assert_eq!(notes_path_for_object("a"), "a");
        assert_eq!(notes_path_for_object("ab"), "ab");

        // Normal SHA (40 chars)
        assert_eq!(
            notes_path_for_object("abcdef1234567890abcdef1234567890abcdef12"),
            "ab/cdef1234567890abcdef1234567890abcdef12"
        );

        // SHA-256 (64 chars)
        assert_eq!(
            notes_path_for_object(
                "abc1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
            ),
            "ab/c1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
        );
    }

    #[test]
    fn test_flat_note_pathspec_for_commit() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let pathspec = flat_note_pathspec_for_commit(sha);
        assert_eq!(
            pathspec,
            "refs/notes/ai:abcdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn test_fanout_note_pathspec_for_commit() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let pathspec = fanout_note_pathspec_for_commit(sha);
        assert_eq!(
            pathspec,
            "refs/notes/ai:ab/cdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn test_note_blob_oids_for_commits_empty() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        // Empty list should return empty map
        let result = note_blob_oids_for_commits(tmp_repo.gitai_repo(), &[]).expect("empty list");
        assert!(result.is_empty());
    }

    #[test]
    #[ignore] // Checkpoint system auto-creates notes, making this assertion invalid
    fn test_note_blob_oids_for_commits_no_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Commit exists but has no note
        let result =
            note_blob_oids_for_commits(tmp_repo.gitai_repo(), &[commit_sha]).expect("no notes");
        assert!(result.is_empty());
    }

    #[test]
    fn test_commits_with_authorship_notes() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo.write_file("a.txt", "a\n", true).expect("write a");
        tmp_repo.commit_with_message("Commit A").expect("commit A");
        let commit_a = tmp_repo.get_head_commit_sha().expect("head A");

        tmp_repo.write_file("b.txt", "b\n", true).expect("write b");
        tmp_repo.commit_with_message("Commit B").expect("commit B");
        let commit_b = tmp_repo.get_head_commit_sha().expect("head B");

        // Both commits may already have notes from commit_with_message
        // Add a custom note to A to ensure it has one
        notes_add(tmp_repo.gitai_repo(), &commit_a, "{\"test\":\"note\"}").expect("add note");

        let commits = vec![commit_a.clone(), commit_b.clone()];
        let result =
            commits_with_authorship_notes(tmp_repo.gitai_repo(), &commits).expect("check notes");

        // Commit A should definitely be in results
        assert!(result.contains(&commit_a), "Commit A should have a note");

        // Commit B may or may not have a note depending on checkpoint system
        // Just verify we got at least 1 result (commit A)
        assert!(
            !result.is_empty(),
            "Should have at least 1 commit with notes"
        );
    }

    #[test]
    fn test_get_reference_as_working_log() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Add a working log format note
        let working_log_json = "[]";
        notes_add(tmp_repo.gitai_repo(), &commit_sha, working_log_json).expect("add note");

        let result = get_reference_as_working_log(tmp_repo.gitai_repo(), &commit_sha)
            .expect("get working log");
        assert_eq!(result.len(), 0); // Empty array
    }

    #[test]
    fn test_get_reference_as_authorship_log_v3_version_mismatch() {
        let tmp_repo = TmpRepo::new().expect("Failed to create tmp repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("write file");
        tmp_repo.commit_with_message("Commit").expect("commit");
        let commit_sha = tmp_repo.get_head_commit_sha().expect("head");

        // Create log with wrong version
        let mut log = AuthorshipLog::new();
        log.metadata.schema_version = "999".to_string();
        log.metadata.base_commit_sha = commit_sha.clone();

        let note_content = log.serialize_to_string().expect("serialize");
        notes_add(tmp_repo.gitai_repo(), &commit_sha, &note_content).expect("add note");

        // Should fail with version mismatch error
        let result = get_reference_as_authorship_log_v3(tmp_repo.gitai_repo(), &commit_sha);
        assert!(result.is_err());

        if let Err(GitAiError::Generic(msg)) = result {
            assert!(msg.contains("Unsupported authorship log version"));
        } else {
            panic!("Expected version mismatch error");
        }
    }
}
