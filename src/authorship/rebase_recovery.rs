use crate::error::GitAiError;
use crate::git::repo_storage::RepoStorage;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SNAPSHOTS: usize = 5;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RebaseSnapshot {
    pub timestamp: u64,
    pub original_head: String,
    pub new_head: String,
    pub original_commits: Vec<String>,
    pub note_entries: HashMap<String, String>,
}

impl RebaseSnapshot {
    pub fn new(
        original_head: String,
        new_head: String,
        original_commits: Vec<String>,
        note_contents: &HashMap<String, String>,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            timestamp,
            original_head,
            new_head,
            original_commits,
            note_entries: note_contents.clone(),
        }
    }
}

fn snapshots_dir(storage: &RepoStorage) -> PathBuf {
    storage.ai_dir.join("rebase_snapshots")
}

pub fn save_snapshot(storage: &RepoStorage, snapshot: &RebaseSnapshot) -> Result<(), GitAiError> {
    let dir = snapshots_dir(storage);
    fs::create_dir_all(&dir)?;

    let filename = format!("{}.json", snapshot.timestamp);
    let path = dir.join(filename);
    let json = serde_json::to_string(snapshot)
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize rebase snapshot: {}", e)))?;
    fs::write(&path, json)?;

    prune_old_snapshots(&dir);
    Ok(())
}

pub fn list_snapshots(storage: &RepoStorage) -> Vec<RebaseSnapshot> {
    let dir = snapshots_dir(storage);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut snapshots = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Ok(content) = fs::read_to_string(&path)
            && let Ok(snapshot) = serde_json::from_str::<RebaseSnapshot>(&content)
        {
            snapshots.push(snapshot);
        }
    }

    snapshots.sort_by_key(|s| std::cmp::Reverse(s.timestamp));
    snapshots
}

pub fn load_latest_snapshot(storage: &RepoStorage) -> Option<RebaseSnapshot> {
    list_snapshots(storage).into_iter().next()
}

pub fn load_snapshot_by_timestamp(storage: &RepoStorage, timestamp: u64) -> Option<RebaseSnapshot> {
    let dir = snapshots_dir(storage);
    let path = dir.join(format!("{}.json", timestamp));
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn prune_old_snapshots(dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();

    if files.len() <= MAX_SNAPSHOTS {
        return;
    }

    // Sort by filename (which is the timestamp), newest first
    files.sort_by(|a, b| b.cmp(a));

    // Remove oldest beyond MAX_SNAPSHOTS
    for old_file in files.into_iter().skip(MAX_SNAPSHOTS) {
        let _ = fs::remove_file(old_file);
    }
}

pub fn recover_from_snapshot(
    repo: &crate::git::repository::Repository,
    snapshot: &RebaseSnapshot,
) -> Result<usize, GitAiError> {
    use crate::git::refs::notes_add_batch;

    let entries: Vec<(String, String)> = snapshot
        .note_entries
        .iter()
        .map(|(sha, content)| (sha.clone(), content.clone()))
        .collect();

    if entries.is_empty() {
        return Ok(0);
    }

    let count = entries.len();
    notes_add_batch(repo, &entries)?;
    Ok(count)
}
