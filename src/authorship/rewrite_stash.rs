use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::authorship::rewrite::migrate_working_log_if_needed;
use crate::error::GitAiError;
use crate::git::repository::{exec_git_allow_nonzero, Repository};

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
        initial.files.retain(|path, _| !path_matches_any(path, pathspecs));
        initial.file_blobs.retain(|path, _| !path_matches_any(path, pathspecs));
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
        return Ok(());
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
        let _ = migrate_working_log_if_needed(
            repo,
            &[(metadata.base_commit.clone(), current_head)],
        );
    }

    if is_pop {
        let _ = fs::remove_file(&metadata_path);
    }

    Ok(())
}

pub fn handle_stash_drop(repo: &Repository, stash_sha: &str) -> Result<(), GitAiError> {
    let dir = stashes_dir(repo);
    let metadata_path = dir.join(format!("{}.json", stash_sha));
    if metadata_path.exists() {
        let _ = fs::remove_file(&metadata_path);
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
        if let Some(sha) = name_str.strip_suffix(".json") {
            if !live_shas.contains(sha) {
                let _ = fs::remove_file(entry.path());
            }
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
