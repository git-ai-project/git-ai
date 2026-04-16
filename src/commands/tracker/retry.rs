use super::config::TrackerConfig;
use super::notes;
use super::upload;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_RETRIES: u32 = 3;

#[derive(Serialize, Deserialize, Clone)]
struct RetryEntry {
    repo_path: String,
    commit_sha: String,
    diff_gz: Vec<u8>,
    retry_count: u32,
}

fn queue_path() -> PathBuf {
    crate::mdm::utils::home_dir()
        .join(".git-ai")
        .join("tracker-retry-queue.json")
}

pub fn save_to_queue(repo_path: &str, commit_sha: &str, diff_gz: Vec<u8>) -> Result<(), String> {
    let path = queue_path();

    let mut entries: Vec<RetryEntry> = if path.exists() {
        let raw = fs::read_to_string(&path).map_err(|e| format!("Failed to read queue: {}", e))?;
        serde_json::from_str(&raw).map_err(|e| format!("Failed to parse queue: {}", e))?
    } else {
        Vec::new()
    };

    entries.push(RetryEntry {
        repo_path: repo_path.to_string(),
        commit_sha: commit_sha.to_string(),
        diff_gz,
        retry_count: 0,
    });

    let tmp_path = path.with_extension("json.tmp");
    let json =
        serde_json::to_string(&entries).map_err(|e| format!("Failed to serialize queue: {}", e))?;

    fs::write(&tmp_path, json).map_err(|e| format!("Failed to write temp queue: {}", e))?;

    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to rename queue file: {}", e))?;

    Ok(())
}

pub fn process_retries(config: &TrackerConfig) -> Result<(), String> {
    let path = queue_path();

    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("Failed to read queue: {}", e))?;
    let entries: Vec<RetryEntry> =
        serde_json::from_str(&raw).map_err(|e| format!("Failed to parse queue: {}", e))?;

    let mut remaining_entries = Vec::new();

    for mut entry in entries {
        if entry.retry_count < MAX_RETRIES {
            match upload::upload_commit(
                &entry.repo_path,
                &entry.commit_sha,
                entry.diff_gz.clone(),
                config,
            ) {
                Ok(()) => {
                    let _ = notes::mark_reported(&entry.repo_path, &entry.commit_sha);
                }
                Err(_) => {
                    entry.retry_count += 1;
                    if entry.retry_count < MAX_RETRIES {
                        remaining_entries.push(entry);
                    }
                }
            }
        }
    }

    if remaining_entries.is_empty() {
        let _ = fs::remove_file(&path);
    } else {
        let tmp_path = path.with_extension("json.tmp");
        let json = serde_json::to_string(&remaining_entries)
            .map_err(|e| format!("Failed to serialize queue: {}", e))?;

        fs::write(&tmp_path, json).map_err(|e| format!("Failed to write temp queue: {}", e))?;

        fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to rename queue file: {}", e))?;
    }

    Ok(())
}
