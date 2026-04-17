use super::config::TrackerConfig;
use crate::http;
use serde_json::{json, Value};
use std::process::Command;

const API_PATH: &str = "/ai-code-boost/open/report/stats";

pub fn upload_commit(
    repo_path: &str,
    commit_sha: &str,
    diff_gz: Vec<u8>,
    config: &TrackerConfig,
    remote: &str,
    branch: &str,
) -> Result<(), String> {
    let commit_info = gather_commit_info(repo_path, commit_sha)?;
    let repo_url = get_remote_url(repo_path, remote).unwrap_or_else(|| repo_path.to_string());
    let git_ai_raw = get_git_ai_stats(repo_path, commit_sha);
    let git_ai_version = get_git_ai_version();
    let pusher_identity = get_pusher_identity(repo_path, commit_sha);

    let diff_gz_base64 = if diff_gz.is_empty() {
        Value::Null
    } else {
        Value::String(encode_base64(&diff_gz))
    };

    let team_id: i64 = config
        .team_id
        .parse()
        .map_err(|_| "invalid team_id".to_string())?;

    let pushed_at = chrono::Utc::now().to_rfc3339();

    let payload = json!({
        "team_id": team_id,
        "team_key": config.team_key,
        "is_doctor": false,
        "repo_url": repo_url,
        "pushed_at": pushed_at,
        "pusher_email": pusher_identity.email,
        "pusher_name": pusher_identity.name,
        "local_ref": branch,
        "remote_ref": branch,
        "commits": [{
            "commit_sha": commit_sha,
            "commit_author_email": pusher_identity.email,
            "commit_author_name": pusher_identity.name,
            "commit_message": commit_info.message,
            "commit_timestamp": commit_info.committer_timestamp,
            "git_ai_raw": git_ai_raw,
            "git_ai_version": git_ai_version,
            "diff_gz": diff_gz_base64,
        }]
    });

    let payload_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let url = format!("{}{}", config.tracker_url, API_PATH);

    let agent = http::build_agent(None);
    let request = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .set("X-Team-Key", &config.team_key);

    let response = http::send_with_body(request, &payload_str)?;

    if response.status_code >= 200 && response.status_code < 300 {
        println!("[git-ai tracker] uploaded {}", &commit_sha[..8]);
        Ok(())
    } else {
        Err(format!(
            "tracker upload failed: HTTP {}",
            response.status_code
        ))
    }
}

struct CommitMeta {
    message: String,
    committer_timestamp: String,
}

struct Identity {
    email: String,
    name: String,
}

fn gather_commit_info(repo_path: &str, commit_sha: &str) -> Result<CommitMeta, String> {
    let output = Command::new("git")
        .args([
            "-C",
            repo_path,
            "show",
            "-s",
            "--format=%s%n%cI",
            commit_sha,
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let message = lines.next().unwrap_or("").to_string();
    let committer_timestamp_raw = lines.next().unwrap_or("").to_string();

    let committer_timestamp =
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&committer_timestamp_raw) {
            dt.with_timezone(&chrono::Utc).to_rfc3339()
        } else {
            committer_timestamp_raw
        };

    Ok(CommitMeta {
        message,
        committer_timestamp,
    })
}

fn get_pusher_identity(repo_path: &str, commit_sha: &str) -> Identity {
    if let Some(id) = try_local_git_config(repo_path) {
        return id;
    }
    if let Some(id) = try_global_git_config(repo_path) {
        return id;
    }
    if let Some(id) = try_commit_author(repo_path, commit_sha) {
        return id;
    }
    if let Some(id) = try_hostname_fallback() {
        return id;
    }
    Identity {
        email: String::new(),
        name: String::new(),
    }
}

fn try_local_git_config(repo_path: &str) -> Option<Identity> {
    let email = git_config_get(repo_path, "--local", "user.email")?;
    let name = git_config_get(repo_path, "--local", "user.name").unwrap_or_default();
    Some(Identity { email, name })
}

fn try_global_git_config(repo_path: &str) -> Option<Identity> {
    let email = git_config_get(repo_path, "--global", "user.email")?;
    let name = git_config_get(repo_path, "--global", "user.name").unwrap_or_default();
    Some(Identity { email, name })
}

fn try_commit_author(repo_path: &str, commit_sha: &str) -> Option<Identity> {
    let email_output = Command::new("git")
        .args(["-C", repo_path, "log", "-1", "--format=%ae", commit_sha])
        .output()
        .ok()?;
    let name_output = Command::new("git")
        .args(["-C", repo_path, "log", "-1", "--format=%an", commit_sha])
        .output()
        .ok()?;

    if !email_output.status.success() {
        return None;
    }

    let email = String::from_utf8_lossy(&email_output.stdout)
        .trim()
        .to_string();
    let name = if name_output.status.success() {
        String::from_utf8_lossy(&name_output.stdout)
            .trim()
            .to_string()
    } else {
        String::new()
    };

    if email.is_empty() {
        None
    } else {
        Some(Identity { email, name })
    }
}

fn try_hostname_fallback() -> Option<Identity> {
    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let hostname_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hostname_str.is_empty() {
        return None;
    }
    Some(Identity {
        email: format!("{}@localhost", hostname_str),
        name: hostname_str,
    })
}

fn git_config_get(repo_path: &str, scope: &str, key: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", repo_path, "config", scope, key])
        .output()
        .ok()?;
    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn get_remote_url(repo_path: &str, remote: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", repo_path, "remote", "get-url", remote])
        .output()
        .ok()?;
    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

fn get_git_ai_stats(repo_path: &str, commit_sha: &str) -> serde_json::Value {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("easylife-ai")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "easylife-ai".to_string());

    let output = Command::new(&binary)
        .args(["stats", "--json", commit_sha])
        .current_dir(repo_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}))
        }
        _ => serde_json::json!({}),
    }
}

fn get_git_ai_version() -> String {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("easylife-ai")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "easylife-ai".to_string());

    let output = Command::new(&binary).arg("--version").output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

fn encode_base64(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }
    result
}
