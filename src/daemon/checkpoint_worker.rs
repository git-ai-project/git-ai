use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::attribution::{attributions_to_line_attributions, update_attributions};
use crate::core::authorship_log;
use crate::core::working_log::{self, AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};
use crate::git_cmd::git_in_repo;

use super::protocol::{CheckpointRequest, StatusRequest, StatusResponse};

pub fn process_checkpoint(req: &CheckpointRequest) -> Result<u32, String> {
    let repo_path = PathBuf::from(&req.repo_dir);
    if !repo_path.exists() {
        return Err(format!("repo_dir does not exist: {}", req.repo_dir));
    }

    let kind = match req.kind.as_str() {
        "ai" | "ai_agent" | "mock_ai" => CheckpointKind::AiAgent,
        "known_human" | "mock_known_human" => CheckpointKind::KnownHuman,
        _ => CheckpointKind::Human,
    };

    let git_dir_str = git_in_repo(&repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir_path = PathBuf::from(&git_dir_str);
    let git_dir = if git_dir_path.is_relative() {
        let abs = repo_path.join(&git_dir_path);
        fs::canonicalize(&abs).unwrap_or(abs)
    } else {
        fs::canonicalize(&git_dir_path).unwrap_or(git_dir_path)
    };

    let base_commit =
        git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    let repo_root = git_in_repo(&repo_path, &["rev-parse", "--show-toplevel"])
        .unwrap_or_else(|_| req.repo_dir.clone());
    let repo_root_path = PathBuf::from(&repo_root);

    let mut processed = 0u32;

    for file_entry in &req.files {
        let file_path = if PathBuf::from(&file_entry.path).is_absolute() {
            PathBuf::from(&file_entry.path)
        } else {
            repo_root_path.join(&file_entry.path)
        };

        let relative_path = file_path
            .strip_prefix(&repo_root_path)
            .unwrap_or(&file_path)
            .to_string_lossy()
            .replace('\\', "/");

        if is_file_conflicted(&repo_path, &relative_path) {
            continue;
        }

        let content = if let Some(ref c) = file_entry.content {
            c.clone()
        } else {
            match fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => continue,
            }
        };

        // Skip binary files (non-UTF8 content with null bytes in first 8KB)
        if let Ok(bytes) = fs::read(&file_path) {
            let check_len = bytes.len().min(8192);
            if bytes[..check_len].contains(&0) {
                continue;
            }
        }

        let blob_sha = working_log::save_blob(&git_dir, &base_commit, content.as_bytes());

        // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
        if kind == CheckpointKind::KnownHuman && has_pending_ai_edit(&git_dir, &relative_path) {
            continue;
        }

        // For AI checkpoints, clear the pending AI edit marker
        if kind == CheckpointKind::AiAgent {
            clear_pending_ai_edit(&git_dir, &relative_path);
        }

        let existing_checkpoints = working_log::read_checkpoints(&git_dir, &base_commit);
        let previous_attributions = find_latest_attributions(&existing_checkpoints, &relative_path);
        let previous_content = find_latest_content(
            &existing_checkpoints,
            &relative_path,
            &git_dir,
            &base_commit,
            &repo_path,
        );

        let checkpoint_agent_id = if kind == CheckpointKind::AiAgent {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let agent = req.agent.as_ref();
            Some(AgentId {
                tool: agent
                    .map(|a| a.tool.clone())
                    .unwrap_or_else(|| "ai".to_string()),
                id: agent
                    .and_then(|a| a.id.clone())
                    .unwrap_or_else(|| format!("ai-thread-{}", ts)),
                model: agent
                    .and_then(|a| a.model.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
            })
        } else {
            None
        };

        let known_human_identity = if kind == CheckpointKind::KnownHuman {
            let name = git_in_repo(&repo_path, &["config", "user.name"])
                .unwrap_or_else(|_| "Unknown".to_string());
            let email = git_in_repo(&repo_path, &["config", "user.email"])
                .unwrap_or_else(|_| "unknown".to_string());
            Some(format!("{} <{}>", name, email))
        } else {
            None
        };

        let trace_value = if kind == CheckpointKind::AiAgent {
            Some(format!(
                "trace-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
        } else {
            None
        };

        let author_id = match &kind {
            CheckpointKind::AiAgent => {
                let aid = checkpoint_agent_id.as_ref().unwrap();
                let session_id = authorship_log::generate_session_id(&aid.tool, &aid.id);
                let trace_hash =
                    authorship_log::generate_trace_hash(trace_value.as_deref().unwrap());
                format!("{}::{}", session_id, trace_hash)
            }
            CheckpointKind::KnownHuman => {
                authorship_log::generate_human_hash(known_human_identity.as_deref().unwrap())
            }
            CheckpointKind::Human => "human".to_string(),
        };

        let enable_move_detection =
            kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
        let new_attributions = update_attributions(
            &previous_content,
            &content,
            &previous_attributions,
            &author_id,
            enable_move_detection,
        );

        let line_attributions = attributions_to_line_attributions(&content, &new_attributions);

        let entry = WorkingLogEntry {
            file: relative_path,
            blob_sha,
            attributions: new_attributions,
            line_attributions,
        };

        let checkpoint_author = if let Some(ref identity) = known_human_identity {
            identity.clone()
        } else if kind == CheckpointKind::AiAgent {
            req.agent
                .as_ref()
                .map(|a| a.tool.clone())
                .unwrap_or_else(|| "ai".to_string())
        } else {
            "human".to_string()
        };

        let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
        checkpoint.agent_id = checkpoint_agent_id;
        checkpoint.trace_id = trace_value;

        working_log::append_checkpoint(&git_dir, &base_commit, &checkpoint);
        processed += 1;
    }

    Ok(processed)
}

pub fn get_status(req: &StatusRequest) -> Result<StatusResponse, String> {
    let repo_path = PathBuf::from(&req.repo_dir);
    if !repo_path.exists() {
        return Err(format!("repo_dir does not exist: {}", req.repo_dir));
    }

    let git_dir_str = git_in_repo(&repo_path, &["rev-parse", "--git-dir"])?;
    let git_dir_path = PathBuf::from(&git_dir_str);
    let git_dir = if git_dir_path.is_relative() {
        let abs = repo_path.join(&git_dir_path);
        fs::canonicalize(&abs).unwrap_or(abs)
    } else {
        fs::canonicalize(&git_dir_path).unwrap_or(git_dir_path)
    };

    let base_commit =
        git_in_repo(&repo_path, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    let checkpoints = working_log::read_checkpoints(&git_dir, &base_commit);

    let mut files: Vec<String> = Vec::new();
    for cp in &checkpoints {
        for entry in &cp.entries {
            if !files.contains(&entry.file) {
                files.push(entry.file.clone());
            }
        }
    }

    Ok(StatusResponse {
        base_commit,
        checkpoint_count: checkpoints.len() as u32,
        files,
    })
}

fn find_latest_attributions(
    checkpoints: &[Checkpoint],
    relative_path: &str,
) -> Vec<crate::core::attribution::Attribution> {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path && !entry.attributions.is_empty() {
                return entry.attributions.clone();
            }
        }
    }
    Vec::new()
}

fn pending_ai_edits_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("ai").join("pending_ai_edits")
}

fn marker_filename(relative_path: &str) -> String {
    relative_path.replace(['/', '\\'], "__")
}

fn has_pending_ai_edit(git_dir: &Path, relative_path: &str) -> bool {
    pending_ai_edits_dir(git_dir)
        .join(marker_filename(relative_path))
        .exists()
}

fn is_file_conflicted(repo_path: &Path, relative_path: &str) -> bool {
    use std::process::Stdio;
    let output = crate::git_cmd::git_command(repo_path)
        .args(["status", "--porcelain", "--", relative_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = output {
        let status = String::from_utf8_lossy(&out.stdout);
        for line in status.lines() {
            if line.len() >= 2 {
                let xy = &line[..2];
                if xy == "UU" || xy == "AA" || xy == "DU" || xy == "UD" {
                    return true;
                }
            }
        }
    }
    false
}

fn clear_pending_ai_edit(git_dir: &Path, relative_path: &str) {
    let marker_path = pending_ai_edits_dir(git_dir).join(marker_filename(relative_path));
    let _ = fs::remove_file(&marker_path);
}

fn find_latest_content(
    checkpoints: &[Checkpoint],
    relative_path: &str,
    git_dir: &Path,
    base_commit: &str,
    repo_path: &Path,
) -> String {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path
                && !entry.blob_sha.is_empty()
                && let Some(content) = working_log::read_blob(git_dir, base_commit, &entry.blob_sha)
            {
                return content;
            }
        }
    }

    if base_commit != "initial"
        && let Ok(content) = git_in_repo(
            repo_path,
            &["show", &format!("{}:{}", base_commit, relative_path)],
        )
    {
        return content;
    }

    String::new()
}
