use super::config::TrackerConfig;
use crate::http;
use serde_json::json;

pub fn upload_commit(
    repo_path: &str,
    commit_sha: &str,
    diff_gz: Vec<u8>,
    config: &TrackerConfig,
) -> Result<(), String> {
    // Encode gzipped diff as base64 using standard alphabet
    let diff_gz_base64 = encode_base64(&diff_gz);

    let payload = json!({
        "team_id": config.team_id,
        "commit_sha": commit_sha,
        "repo_path": repo_path,
        "diff_gz_base64": diff_gz_base64,
    });

    let payload_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;

    let agent = http::build_agent(None);
    let request = agent
        .post(&config.tracker_url)
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

/// Encode bytes as base64 using the standard alphabet (A-Z, a-z, 0-9, +, /).
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
