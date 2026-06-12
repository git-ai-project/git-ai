use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};
use crate::error::GitAiError;
use crate::git::repository::{
    Repository, batch_read_paths_at_treeishes, disable_internal_git_hooks,
    exec_git_allow_nonzero_with_env,
};

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
        // Trailing-`*` prefix glob (e.g. `src/foo*`, or a bare `*`), matching
        // the pathspec semantics the pre-rewrite stash matcher supported.
        if let Some(prefix) = spec.strip_suffix('*') {
            return path.starts_with(prefix);
        }
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

pub fn handle_stash_pop_or_apply_with_head(
    repo: &Repository,
    stash_sha: &str,
    is_pop: bool,
    target_head: Option<&str>,
) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    let metadata_path = dir.join(format!("{}.json", stash_sha));

    if !metadata_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&metadata_path)?;
    let metadata: StashMetadata = serde_json::from_str(&content)?;

    let Some(current_head) = target_head.filter(|h| !h.is_empty()) else {
        return Ok(());
    };

    if metadata.base_commit != current_head {
        restore_stash_attributions_with_shift(repo, stash_sha, current_head)?;
    } else {
        restore_stash_attributions(repo, stash_sha, current_head)?;
    }

    if is_pop {
        let _ = fs::remove_file(&metadata_path);
        let attr_path = dir.join(format!("{}_attrs.json", stash_sha));
        let _ = fs::remove_file(&attr_path);
        let worklog_dir = dir.join(format!("{}_worklog", stash_sha));
        let _ = fs::remove_dir_all(&worklog_dir);
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
    let worklog_dir = dir.join(format!("{}_worklog", stash_sha));
    if worklog_dir.exists() {
        let _ = fs::remove_dir_all(&worklog_dir);
    }
    Ok(())
}

fn save_stash_attributions(
    repo: &Repository,
    stash_sha: &str,
    head_sha: &str,
    pathspecs: &[String],
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

    // A partial `git stash push -- <pathspec>` only stashes the matching paths,
    // so the saved attribution must be scoped to them too. Otherwise the stash
    // carries checkpoints/INITIAL entries for files that were never stashed,
    // and a later (cross-branch / shifted) pop resurrects their attribution.
    // The realistic AI flow keeps attribution in checkpoints.jsonl (INITIAL is
    // often absent), so we filter both. Orphan content blobs left in blobs/ are
    // harmless -- restore only reads blobs referenced by surviving checkpoints.
    if !pathspecs.is_empty() {
        filter_stash_checkpoints_to_pathspecs(&stash_log_dir.join("checkpoints.jsonl"), pathspecs)?;
        filter_stash_initial_to_pathspecs(&stash_log_dir.join("INITIAL"), pathspecs)?;
    }

    Ok(())
}

/// Retain only checkpoint entries whose file matches one of `pathspecs`,
/// dropping checkpoints left with no entries. Round-trips each JSONL line
/// through serde_json::Value so all non-`entries` fields are preserved exactly.
fn filter_stash_checkpoints_to_pathspecs(
    path: &std::path::Path,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(());
    };

    let mut kept_lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
            // Preserve any line we can't parse rather than silently dropping it.
            kept_lines.push(line.to_string());
            continue;
        };
        if let Some(entries) = value.get_mut("entries").and_then(|e| e.as_array_mut()) {
            entries.retain(|entry| {
                entry
                    .get("file")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| path_matches_any(f, pathspecs))
            });
            if entries.is_empty() {
                continue;
            }
        }
        kept_lines.push(serde_json::to_string(&value)?);
    }

    let mut out = kept_lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

/// Retain only INITIAL `files`/`file_blobs` whose path matches one of
/// `pathspecs` (inverse of clean_working_log_for_stash, which drops them).
fn filter_stash_initial_to_pathspecs(
    path: &std::path::Path,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    use crate::git::repo_storage::InitialAttributions;

    let Ok(content) = fs::read_to_string(path) else {
        return Ok(());
    };
    let Ok(mut initial) = serde_json::from_str::<InitialAttributions>(&content) else {
        return Ok(());
    };

    initial.files.retain(|p, _| path_matches_any(p, pathspecs));
    initial
        .file_blobs
        .retain(|p, _| path_matches_any(p, pathspecs));

    fs::write(path, serde_json::to_string(&initial)?)?;
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

            if src_path.is_dir() {
                let _ = copy_dir_recursive(&src_path, &dst_path);
            } else if file_name == "checkpoints.jsonl" {
                if let Ok(stash_content) = fs::read_to_string(&src_path) {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&dst_path)?;
                    f.write_all(stash_content.as_bytes())?;
                }
            } else if file_name == "INITIAL" && dst_path.exists() {
                merge_initial_files(&src_path, &dst_path)?;
            } else {
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

    // Extract file attributions and reconstruct the applied content from immutable trees.
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

    let applied_paths: Vec<String> = authorship_log
        .attestations
        .iter()
        .map(|fa| fa.file_path.clone())
        .collect();
    let applied_contents =
        reconstruct_stash_applied_contents(repo, stash_sha, current_head, &applied_paths)?;

    for fa in &authorship_log.attestations {
        let file_path = &fa.file_path;
        let stash_content = stash_file_contents
            .get(file_path)
            .cloned()
            .or_else(|| va.get_file_content(file_path).cloned())
            .unwrap_or_default();
        let current_content = applied_contents.get(file_path).cloned().unwrap_or_default();

        if current_content.is_empty() {
            continue;
        }

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

    let working_log = repo.storage.working_log_for_base_commit(current_head)?;
    working_log
        .write_initial_attributions_with_contents(files, prompts, humans, file_blobs, sessions)?;

    Ok(())
}

fn reconstruct_stash_applied_contents(
    repo: &Repository,
    stash_sha: &str,
    target_head: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let unique = format!(
        "git-ai-stash-apply-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp_dir = std::env::temp_dir().join(unique);
    let index_path = temp_dir.join("index");
    let worktree_path = temp_dir.join("worktree");
    fs::create_dir_all(&worktree_path)?;

    let result = (|| {
        let _guard = disable_internal_git_hooks();
        run_isolated_git(
            repo,
            vec!["read-tree".to_string(), target_head.to_string()],
            &index_path,
            &worktree_path,
            true,
        )?;
        run_isolated_git(
            repo,
            vec!["checkout-index".to_string(), "-a".to_string()],
            &index_path,
            &worktree_path,
            true,
        )?;
        let _ = run_isolated_git(
            repo,
            vec![
                "stash".to_string(),
                "apply".to_string(),
                stash_sha.to_string(),
            ],
            &index_path,
            &worktree_path,
            false,
        )?;
        run_isolated_git(
            repo,
            vec!["add".to_string(), "-A".to_string()],
            &index_path,
            &worktree_path,
            true,
        )?;
        let output = run_isolated_git(
            repo,
            vec!["write-tree".to_string()],
            &index_path,
            &worktree_path,
            true,
        )?;
        let result_tree = String::from_utf8(output.stdout)?.trim().to_string();
        let requests: Vec<(String, String)> = file_paths
            .iter()
            .map(|path| (result_tree.clone(), path.clone()))
            .collect();
        let contents = batch_read_paths_at_treeishes(repo, &requests)?;
        Ok(contents
            .into_iter()
            .map(|((_, path), content)| (path, content))
            .collect())
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn run_isolated_git(
    repo: &Repository,
    args: Vec<String>,
    index_path: &std::path::Path,
    worktree_path: &std::path::Path,
    require_success: bool,
) -> Result<std::process::Output, GitAiError> {
    let mut full_args = repo.global_args_for_exec();
    full_args.extend(args);
    let envs = [
        ("GIT_INDEX_FILE", index_path.as_os_str()),
        ("GIT_WORK_TREE", worktree_path.as_os_str()),
    ];
    let output = exec_git_allow_nonzero_with_env(&full_args, &envs)?;
    if require_success && !output.status.success() {
        return Err(GitAiError::GitCliError {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            args: full_args,
        });
    }
    Ok(output)
}

fn merge_initial_files(
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Result<(), GitAiError> {
    use crate::git::repo_storage::InitialAttributions;

    let src_content = fs::read_to_string(src_path)?;
    let dst_content = fs::read_to_string(dst_path)?;

    let src_initial: InitialAttributions = match serde_json::from_str(&src_content) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let mut dst_initial: InitialAttributions = match serde_json::from_str(&dst_content) {
        Ok(v) => v,
        Err(_) => {
            fs::copy(src_path, dst_path)?;
            return Ok(());
        }
    };

    for (path, attrs) in src_initial.files {
        dst_initial.files.entry(path).or_insert(attrs);
    }
    for (path, blob) in src_initial.file_blobs {
        dst_initial.file_blobs.entry(path).or_insert(blob);
    }
    for (key, record) in src_initial.prompts {
        dst_initial.prompts.entry(key).or_insert(record);
    }
    for (key, record) in src_initial.humans {
        dst_initial.humans.entry(key).or_insert(record);
    }
    for (key, record) in src_initial.sessions {
        dst_initial.sessions.entry(key).or_insert(record);
    }

    let merged = serde_json::to_string(&dst_initial)?;
    fs::write(dst_path, merged)?;
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
    fn test_path_matches_any_trailing_glob() {
        // Regression (#5): the pre-rewrite matcher honored a trailing `*`
        // prefix-glob; path_matches_any dropped it, so `git stash push --
        // 'src/foo*'` no longer matched src/foobar.txt.
        let specs = vec!["src/foo*".to_string()];
        assert!(path_matches_any("src/foobar.txt", &specs));
        assert!(path_matches_any("src/foo.rs", &specs));
        assert!(!path_matches_any("src/bar.rs", &specs));
        // A bare `*` matches anything.
        assert!(path_matches_any("anything/at/all.txt", &["*".to_string()]));
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
