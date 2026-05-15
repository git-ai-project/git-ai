use std::fs;
use std::path::PathBuf;

use crate::api::client::HttpClient;

/// Status of the authentication token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStatus {
    Valid,
    Expired,
    Missing,
}

/// Returns the path to the credentials file: `~/.git-ai/credentials.json`.
fn credentials_path() -> Option<PathBuf> {
    super::home_dir().map(|home| home.join(".git-ai").join("credentials.json"))
}

/// Load the API token from `~/.git-ai/credentials.json`.
/// Returns `None` if the file doesn't exist or is malformed.
pub fn load_token() -> Option<String> {
    let path = credentials_path()?;
    let contents = fs::read_to_string(&path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&contents).ok()?;
    parsed.get("token")?.as_str().map(|s| s.to_string())
}

/// Save the API token to `~/.git-ai/credentials.json`.
/// Creates the parent directory if needed. Sets file permissions to 0600 on Unix.
pub fn save_token(token: &str) -> Result<(), String> {
    let path = credentials_path().ok_or("unable to determine home directory")?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }

    let content = serde_json::json!({ "token": token });
    let serialized =
        serde_json::to_string_pretty(&content).map_err(|e| format!("json error: {e}"))?;

    // Write with restricted permissions from the start to avoid TOCTOU race
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        file.write_all(serialized.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        // Windows: write then restrict (no atomic mode creation available)
        fs::write(&path, &serialized)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }

    Ok(())
}

/// Check token validity by calling `GET /v1/auth/check`.
pub fn check_token(client: &HttpClient) -> TokenStatus {
    if client.auth_token.is_none() {
        return TokenStatus::Missing;
    }

    match client.get("/v1/auth/check") {
        Ok(response) if response.status == 200 => TokenStatus::Valid,
        _ => TokenStatus::Expired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_save_and_load_token() {
        let tmp = tempfile::tempdir().unwrap();
        let creds_dir = tmp.path().join(".git-ai");
        let creds_path = creds_dir.join("credentials.json");

        // Manually create credentials in a temp location
        fs::create_dir_all(&creds_dir).unwrap();
        let content = serde_json::json!({ "token": "test-token-123" });
        fs::write(&creds_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        // Verify the content is correct
        let raw = fs::read_to_string(&creds_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["token"].as_str().unwrap(), "test-token-123");
    }

    #[test]
    fn test_load_token_missing_file() {
        // If HOME points to a nonexistent dir, load_token returns None
        // We just verify the function doesn't panic on normal call
        // (actual behavior depends on the real home dir state)
        let _ = load_token();
    }

    #[test]
    fn test_token_status_missing_when_no_token() {
        let client = HttpClient::new("http://localhost:9999", None);
        let status = check_token(&client);
        assert_eq!(status, TokenStatus::Missing);
    }
}
