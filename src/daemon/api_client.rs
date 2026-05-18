use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::telemetry_types::{
    CasUploadRequest, CasUploadResponse, DEFAULT_API_BASE_URL, MetricsBatch, MetricsUploadResponse,
};

const REQUEST_TIMEOUT_SECS: u64 = 30;
const DEFAULT_RETRY_DELAY_SECS: u64 = 60;

fn retry_delay() -> Duration {
    let secs = std::env::var("GIT_AI_RETRY_DELAY_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETRY_DELAY_SECS);
    Duration::from_secs(secs)
}

/// HTTP API client for outbound telemetry uploads (uses curl subprocess).
pub struct ApiClient {
    base_url: String,
    api_key: Option<String>,
    auth_token: Option<String>,
    distinct_id: String,
    version: String,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    pub fn new() -> Self {
        let base_url = std::env::var("GIT_AI_API_BASE_URL").unwrap_or_else(|_| {
            read_config_field("api_base_url").unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string())
        });

        let api_key = std::env::var("GIT_AI_API_KEY")
            .ok()
            .or_else(|| read_config_field("api_key"));

        let auth_token = load_auth_token();
        let distinct_id = get_or_create_distinct_id();
        let version = env!("CARGO_PKG_VERSION").to_string();

        Self {
            base_url,
            api_key,
            auth_token,
            distinct_id,
            version,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.auth_token.is_some() || self.api_key.is_some()
    }

    pub fn should_upload(&self) -> bool {
        self.base_url != DEFAULT_API_BASE_URL || self.is_authenticated()
    }

    /// Upload metrics batch. Returns Ok on 200 response.
    pub fn upload_metrics(&self, batch: &MetricsBatch) -> Result<MetricsUploadResponse, String> {
        let url = format!("{}/worker/metrics/upload", self.base_url);
        let body = serde_json::to_string(batch).map_err(|e| format!("serialize metrics: {}", e))?;

        let response = self.post(&url, &body)?;
        serde_json::from_str(&response).map_err(|e| format!("parse metrics response: {}", e))
    }

    /// Upload metrics with retry (1 retry after configurable delay, default 60s).
    pub fn upload_metrics_with_retry(&self, batch: &MetricsBatch) -> Result<(), String> {
        match self.upload_metrics(batch) {
            Ok(_) => Ok(()),
            Err(first_err) => {
                let delay = retry_delay();
                eprintln!(
                    "[git-ai daemon] metrics upload failed: {}, retrying in {:?}",
                    first_err, delay
                );
                std::thread::sleep(delay);
                self.upload_metrics(batch).map(|_| ())
            }
        }
    }

    /// Upload CAS objects.
    pub fn upload_cas(&self, request: &CasUploadRequest) -> Result<CasUploadResponse, String> {
        let url = format!("{}/worker/cas/upload", self.base_url);
        let body = serde_json::to_string(request).map_err(|e| format!("serialize cas: {}", e))?;

        let response = self.post(&url, &body)?;
        serde_json::from_str(&response).map_err(|e| format!("parse cas response: {}", e))
    }

    fn post(&self, url: &str, body: &str) -> Result<String, String> {
        let mut cmd = Command::new("curl");
        cmd.arg("-s")
            .arg("-S")
            .arg("--fail")
            .arg("--max-time")
            .arg(REQUEST_TIMEOUT_SECS.to_string())
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-H")
            .arg(format!("User-Agent: git-ai/{}", self.version))
            .arg("-H")
            .arg(format!("X-Distinct-ID: {}", self.distinct_id));

        if let Some(ref token) = self.auth_token {
            cmd.arg("-H")
                .arg(format!("Authorization: Bearer {}", token));
        }
        if let Some(ref key) = self.api_key {
            cmd.arg("-H").arg(format!("X-API-Key: {}", key));
        }

        cmd.arg("-d").arg(body).arg(url);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd
            .output()
            .map_err(|e| format!("curl failed to execute: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "curl failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ));
        }

        String::from_utf8(output.stdout)
            .map_err(|e| format!("invalid UTF-8 in curl response: {}", e))
    }
}

/// HTTP POST using curl. Returns (status_code, body).
/// Used by the OAuth login flow.
pub fn curl_post(url: &str, body: &str, timeout_secs: u64) -> Result<(u16, String), String> {
    let output = Command::new("curl")
        .arg("-s")
        .arg("-S")
        .arg("--max-time")
        .arg(timeout_secs.to_string())
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-w")
        .arg("\n%{http_code}")
        .arg("-d")
        .arg(body)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("curl failed to execute: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl failed: {}", stderr.trim()));
    }

    let raw = String::from_utf8(output.stdout)
        .map_err(|e| format!("invalid UTF-8 in curl response: {}", e))?;

    // Last line is the HTTP status code (from -w "\n%{http_code}")
    let (body_part, status_str) = match raw.rfind('\n') {
        Some(pos) => (&raw[..pos], &raw[pos + 1..]),
        None => (raw.as_str(), "0"),
    };

    let status: u16 = status_str.trim().parse().unwrap_or(0);
    Ok((status, body_part.to_string()))
}

/// Read a field from ~/.git-ai/config.json
fn read_config_field(field: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let config_path = PathBuf::from(&home).join(".git-ai").join("config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get(field)?.as_str().map(|s| s.to_string())
}

/// Load OAuth auth token from credential store.
fn load_auth_token() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let cred_path = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("credentials.json");
    let content = std::fs::read_to_string(&cred_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("access_token")?.as_str().map(|s| s.to_string())
}

/// Generate a random UUID-like string without external deps.
fn generate_random_id() -> String {
    use std::fs::File;
    use std::io::Read;

    let mut bytes = [0u8; 16];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    } else {
        // Fallback: use timestamp + pid
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let combined = t ^ (pid << 64);
        bytes.copy_from_slice(&combined.to_le_bytes());
    }

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ]),
    )
}

/// Get or create a persistent distinct_id at ~/.git-ai/internal/distinct_id
fn get_or_create_distinct_id() -> String {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return generate_random_id(),
    };

    let internal_dir = PathBuf::from(&home).join(".git-ai").join("internal");
    let id_path = internal_dir.join("distinct_id");

    if let Ok(id) = std::fs::read_to_string(&id_path) {
        let trimmed = id.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    let new_id = generate_random_id();
    let _ = std::fs::create_dir_all(&internal_dir);
    let _ = write_private_file(&id_path, new_id.as_bytes());
    new_id
}

/// Write a file with owner-only permissions (0600 on unix).
fn write_private_file(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content)?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_id_is_stable() {
        let id1 = get_or_create_distinct_id();
        let id2 = get_or_create_distinct_id();
        assert_eq!(id1, id2);
        assert!(!id1.is_empty());
    }

    #[test]
    fn random_id_format() {
        let id = generate_random_id();
        assert_eq!(id.len(), 36); // 8-4-4-4-12
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
    }
}
