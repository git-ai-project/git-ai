use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};
use crate::error::GitAiError;
use crate::git::repo_storage::InitialAttributions;
use crate::git::repository::{Repository, exec_git_allow_nonzero};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StashMetadata {
    pub base_commit: String,
    pub timestamp: u64,
    #[serde(default)]
    pub pathspecs: Vec<String>,
}

fn stashes_dir(repo: &Repository) -> PathBuf {
    repo.storage.ai_dir.join("stashes")
}

fn path_matches_any(path: &str, pathspecs: &[String]) -> bool {
    pathspecs.iter().any(|spec| {
        let normalized = spec.trim_end_matches('/');
        path == spec || path == normalized || {
            let prefix = format!("{}/", normalized);
            path.starts_with(&prefix)
        }
    })
}

fn clean_working_log_for_stash(
    repo: &Repository,
    head_sha: &str,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    if !repo.storage.has_working_log(head_sha) {
        return Ok(());
    }

    let persisted = repo.storage.working_log_for_base_commit(head_sha)?;
    let mut initial = persisted.read_initial_attributions();

    if pathspecs.is_empty() {
        initial.files.clear();
        initial.file_blobs.clear();
    } else {
        initial
            .files
            .retain(|path, _| !path_matches_any(path, pathspecs));
        initial
            .file_blobs
            .retain(|path, _| !path_matches_any(path, pathspecs));
    }

    persisted.write_initial(initial)?;
    Ok(())
}

pub fn handle_stash_create(
    repo: &Repository,
    stash_sha: &str,
    head_sha: &str,
    pathspecs: Vec<String>,
) -> Result<(), GitAiError> {
    let metadata = StashMetadata {
        base_commit: head_sha.to_string(),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        pathspecs: pathspecs.clone(),
    };

    let dir = stashes_dir(repo);
    fs::create_dir_all(&dir)?;

    let metadata_path = dir.join(format!("{}.json", stash_sha));
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(&metadata_path, json)?;

    // Save stashed file attributions before cleaning them from the working log
    save_stash_attributions(repo, stash_sha, head_sha, &pathspecs)?;

    clean_working_log_for_stash(repo, head_sha, &pathspecs)?;

    Ok(())
}

pub fn handle_stash_pop_or_apply(
    repo: &Repository,
    stash_sha: &str,
    is_pop: bool,
) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    let metadata_path = dir.join(format!("{}.json", stash_sha));

    if !metadata_path.exists() {
        return try_restore_pending_stash(repo, stash_sha, is_pop);
    }

    let content = fs::read_to_string(&metadata_path)?;
    let metadata: StashMetadata = serde_json::from_str(&content)?;

    let current_head = {
        let mut args = repo.global_args_for_exec();
        args.extend(["rev-parse".to_string(), "HEAD".to_string()]);
        exec_git_allow_nonzero(&args)
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };

    if !current_head.is_empty() && metadata.base_commit != current_head {
        // HEAD advanced since stash was created. Restore with content-based
        // shifting so attributions map to correct lines in the working tree.
        restore_stash_attributions_with_shift(repo, stash_sha, &current_head)?;
    } else {
        // Same HEAD - restore directly
        restore_stash_attributions(repo, stash_sha, &current_head)?;
    }

    if is_pop {
        let _ = fs::remove_file(&metadata_path);
        let attr_path = dir.join(format!("{}_attrs.json", stash_sha));
        let _ = fs::remove_file(&attr_path);
    }

    Ok(())
}

pub fn handle_stash_drop(repo: &Repository, stash_sha: &str) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    let metadata_path = dir.join(format!("{}.json", stash_sha));
    if metadata_path.exists() {
        let _ = fs::remove_file(&metadata_path);
    }
    let attr_path = dir.join(format!("{}_attrs.json", stash_sha));
    if attr_path.exists() {
        let _ = fs::remove_file(&attr_path);
    }
    Ok(())
}

fn save_stash_attributions(
    repo: &Repository,
    stash_sha: &str,
    head_sha: &str,
    _pathspecs: &[String],
) -> Result<(), GitAiError> {
    if !repo.storage.has_working_log(head_sha) {
        return Ok(());
    }

    let src_dir = repo.storage.working_logs.join(head_sha);
    let dir = stashes_dir(repo);
    let stash_log_dir = dir.join(format!("{}_worklog", stash_sha));

    if src_dir.exists() {
        let _ = copy_dir_recursive(&src_dir, &stash_log_dir);
    }

    Ok(())
}

fn restore_stash_attributions(
    repo: &Repository,
    stash_sha: &str,
    current_head: &str,
) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    let stash_log_dir = dir.join(format!("{}_worklog", stash_sha));

    if !stash_log_dir.exists() {
        return Ok(());
    }

    let dst_dir = repo.storage.working_logs.join(current_head);
    fs::create_dir_all(&dst_dir)?;

    if let Ok(entries) = fs::read_dir(&stash_log_dir) {
        for entry in entries.flatten() {
            let src_path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst_dir.join(&file_name);

            if file_name == "checkpoints.jsonl" {
                if let Ok(stash_content) = fs::read_to_string(&src_path) {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&dst_path)?;
                    f.write_all(stash_content.as_bytes())?;
                }
            } else if !dst_path.exists() {
                let _ = fs::copy(&src_path, &dst_path);
            }
        }
    }

    Ok(())
}

fn restore_stash_attributions_with_shift(
    repo: &Repository,
    stash_sha: &str,
    current_head: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::VirtualAttributions;

    let dir = stashes_dir(repo);
    let stash_log_dir = dir.join(format!("{}_worklog", stash_sha));

    if !stash_log_dir.exists() {
        return Ok(());
    }

    // Temporarily restore the stash worklog to a temp base_commit path so we can
    // use VirtualAttributions to consolidate checkpoints into line attributions.
    let temp_base = format!("_stash_restore_{}", stash_sha);
    let temp_dir = repo.storage.working_logs.join(&temp_base);
    let _ = copy_dir_recursive(&stash_log_dir, &temp_dir);

    // Build a snapshot of file contents from the blob storage in the stash worklog.
    // This gives us the file content as it was at stash time.
    let blobs_dir = temp_dir.join("blobs");
    let working_log = repo.storage.working_log_for_base_commit(&temp_base)?;
    let checkpoints = working_log.read_all_checkpoints().unwrap_or_default();

    // For each file, find the last blob SHA from checkpoints to determine content at stash time
    let mut stash_file_contents: HashMap<String, String> = HashMap::new();
    for checkpoint in &checkpoints {
        for entry in &checkpoint.entries {
            if !entry.blob_sha.is_empty() {
                let blob_path = blobs_dir.join(&entry.blob_sha);
                if let Ok(content) = fs::read_to_string(&blob_path) {
                    stash_file_contents.insert(entry.file.clone(), content);
                }
            }
        }
    }

    // Use from_working_log_snapshot with the stash content as the snapshot
    let va_result = VirtualAttributions::from_working_log_snapshot(
        repo.clone(),
        temp_base.clone(),
        None,
        &stash_file_contents,
    );

    // Clean up temp dir
    let _ = fs::remove_dir_all(&temp_dir);

    let va = va_result?;

    // Extract file attributions and content from VirtualAttributions
    let workdir = repo.workdir()?;
    let mut files: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut file_blobs: HashMap<String, String> = HashMap::new();
    let mut prompts = HashMap::new();
    let mut sessions = std::collections::BTreeMap::new();
    let mut humans = std::collections::BTreeMap::new();

    let authorship_log = va.to_authorship_log()?;

    for (key, record) in &authorship_log.metadata.prompts {
        prompts.insert(key.clone(), record.clone());
    }
    for (key, record) in &authorship_log.metadata.sessions {
        sessions.insert(key.clone(), record.clone());
    }
    for (key, record) in &authorship_log.metadata.humans {
        humans.insert(key.clone(), record.clone());
    }

    for fa in &authorship_log.attestations {
        let file_path = &fa.file_path;
        let current_content = {
            let abs_path = workdir.join(file_path);
            if abs_path.exists() {
                fs::read_to_string(&abs_path).unwrap_or_default()
            } else {
                continue;
            }
        };

        if current_content.is_empty() {
            continue;
        }

        let stash_content = stash_file_contents
            .get(file_path)
            .cloned()
            .or_else(|| va.get_file_content(file_path).cloned())
            .unwrap_or_default();

        // Build line attributions from attestation entries
        let mut attrs: Vec<LineAttribution> = Vec::new();
        for entry in &fa.entries {
            for range in &entry.line_ranges {
                let (start, end) = match range {
                    crate::authorship::authorship_log::LineRange::Single(l) => (*l, *l),
                    crate::authorship::authorship_log::LineRange::Range(s, e) => (*s, *e),
                };
                attrs.push(LineAttribution::new(start, end, entry.hash.clone(), None));
            }
        }

        if stash_content == current_content {
            files.insert(file_path.clone(), attrs);
            file_blobs.insert(file_path.clone(), current_content);
            continue;
        }

        // Content-based shift using Equal regions
        let old_lines: Vec<&str> = stash_content.lines().collect();
        let new_lines: Vec<&str> = current_content.lines().collect();
        let ops = capture_diff_slices(&old_lines, &new_lines);

        let mut line_map: HashMap<u32, u32> = HashMap::new();
        for op in &ops {
            if let DiffOp::Equal {
                old_index,
                new_index,
                len,
            } = op
            {
                for i in 0..*len {
                    line_map.insert((*old_index + i + 1) as u32, (*new_index + i + 1) as u32);
                }
            }
        }

        let shifted: Vec<LineAttribution> = attrs
            .into_iter()
            .filter_map(|attr| {
                let new_start = line_map.get(&attr.start_line).copied()?;
                let new_end = line_map.get(&attr.end_line).copied()?;
                Some(LineAttribution::new(
                    new_start,
                    new_end,
                    attr.author_id,
                    attr.overrode,
                ))
            })
            .collect();

        if !shifted.is_empty() {
            files.insert(file_path.clone(), shifted);
            file_blobs.insert(file_path.clone(), current_content);
        }
    }

    if files.is_empty() {
        return Ok(());
    }

    let initial = InitialAttributions {
        files,
        prompts,
        file_blobs,
        humans,
        sessions,
    };

    let working_log = repo.storage.working_log_for_base_commit(current_head)?;
    working_log.write_initial(initial)?;

    Ok(())
}

/// Save a pending stash snapshot when the stash SHA can't be resolved at push time.
/// Keyed by the HEAD SHA (which is the stash's parent commit).
/// Also cleans the working log so subsequent commits don't consume stashed attributions.
pub fn save_pending_stash(
    repo: &Repository,
    head_sha: &str,
    pathspecs: Vec<String>,
) -> Result<(), GitAiError> {
    if !repo.storage.has_working_log(head_sha) {
        return Ok(());
    }

    let src_dir = repo.storage.working_logs.join(head_sha);
    if !src_dir.exists() {
        return Ok(());
    }

    let dir = stashes_dir(repo);
    fs::create_dir_all(&dir)?;
    let pending_dir = dir.join(format!("pending_{}_worklog", head_sha));
    copy_dir_recursive(&src_dir, &pending_dir)?;

    clean_working_log_for_stash(repo, head_sha, &pathspecs)?;

    Ok(())
}

/// Try to restore attributions from a pending stash snapshot.
/// Called when `handle_stash_pop_or_apply` finds no metadata for the stash SHA.
fn try_restore_pending_stash(
    repo: &Repository,
    stash_sha: &str,
    is_pop: bool,
) -> Result<(), GitAiError> {
    // Find the stash's parent commit (= the HEAD at stash push time)
    let mut args = repo.global_args_for_exec();
    args.extend(["rev-parse".to_string(), format!("{}^", stash_sha)]);
    let parent_sha = exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let Some(parent_sha) = parent_sha else {
        return Ok(());
    };

    let dir = stashes_dir(repo);
    let pending_dir = dir.join(format!("pending_{}_worklog", parent_sha));
    if !pending_dir.exists() {
        return Ok(());
    }

    let current_head = {
        let mut args = repo.global_args_for_exec();
        args.extend(["rev-parse".to_string(), "HEAD".to_string()]);
        exec_git_allow_nonzero(&args)
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };

    if current_head.is_empty() {
        return Ok(());
    }

    // Restore the pending worklog to the current HEAD's working log
    let dst_dir = repo.storage.working_logs.join(&current_head);
    fs::create_dir_all(&dst_dir)?;

    if let Ok(entries) = fs::read_dir(&pending_dir) {
        for entry in entries.flatten() {
            let src_path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst_dir.join(&file_name);

            if file_name == "checkpoints.jsonl" {
                if let Ok(stash_content) = fs::read_to_string(&src_path) {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&dst_path)?;
                    f.write_all(stash_content.as_bytes())?;
                }
            } else if !dst_path.exists() {
                let _ = fs::copy(&src_path, &dst_path);
            }
        }
    }

    if is_pop {
        let _ = fs::remove_dir_all(&pending_dir);
    }

    Ok(())
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), GitAiError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

pub fn gc_stash_metadata(repo: &Repository) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    if !dir.exists() {
        return Ok(());
    }

    let mut args = repo.global_args_for_exec();
    args.extend([
        "reflog".to_string(),
        "show".to_string(),
        "--format=%H".to_string(),
        "refs/stash".to_string(),
    ]);

    let live_shas: std::collections::HashSet<String> = exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if let Some(sha) = name_str.strip_suffix(".json")
            && !live_shas.contains(sha)
        {
            let _ = fs::remove_file(entry.path());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_matches_any_exact() {
        let specs = vec!["src/main.rs".to_string()];
        assert!(path_matches_any("src/main.rs", &specs));
        assert!(!path_matches_any("src/lib.rs", &specs));
    }

    #[test]
    fn test_path_matches_any_directory_prefix() {
        let specs = vec!["src/".to_string()];
        assert!(path_matches_any("src/main.rs", &specs));
        assert!(path_matches_any("src/lib.rs", &specs));
        assert!(!path_matches_any("tests/main.rs", &specs));
    }

    #[test]
    fn test_path_matches_any_directory_without_slash() {
        let specs = vec!["src".to_string()];
        assert!(path_matches_any("src/main.rs", &specs));
        assert!(!path_matches_any("src2/main.rs", &specs));
    }

    #[test]
    fn test_path_matches_any_trailing_slash_normalized() {
        let specs = vec!["dir/".to_string()];
        assert!(path_matches_any("dir", &specs));
        assert!(path_matches_any("dir/file.txt", &specs));
    }

    #[test]
    fn test_path_matches_any_empty_specs() {
        let specs: Vec<String> = vec![];
        assert!(!path_matches_any("anything", &specs));
    }

    #[test]
    fn test_stash_metadata_serialization_roundtrip() {
        let metadata = StashMetadata {
            base_commit: "abc123def456".to_string(),
            timestamp: 1700000000,
            pathspecs: vec!["src/".to_string(), "Cargo.toml".to_string()],
        };

        let json = serde_json::to_string_pretty(&metadata).unwrap();
        let deserialized: StashMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.base_commit, "abc123def456");
        assert_eq!(deserialized.timestamp, 1700000000);
        assert_eq!(deserialized.pathspecs.len(), 2);
        assert_eq!(deserialized.pathspecs[0], "src/");
        assert_eq!(deserialized.pathspecs[1], "Cargo.toml");
    }

    #[test]
    fn test_stash_metadata_empty_pathspecs_default() {
        let json = r#"{"base_commit":"abc123","timestamp":100}"#;
        let metadata: StashMetadata = serde_json::from_str(json).unwrap();
        assert!(metadata.pathspecs.is_empty());
    }
}
