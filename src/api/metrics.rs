use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::api::client::HttpClient;

/// Anonymous usage metrics payload.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsPayload {
    pub machine_id: String,
    pub version: String,
    pub agents_detected: Vec<String>,
    pub total_commits_tracked: u64,
}

impl MetricsPayload {
    /// Create a new metrics payload with an anonymized machine ID.
    /// The `machine_id` is the SHA-256 of the hostname.
    pub fn new(version: &str, agents_detected: Vec<String>, total_commits_tracked: u64) -> Self {
        Self {
            machine_id: compute_machine_id(),
            version: version.to_string(),
            agents_detected,
            total_commits_tracked,
        }
    }
}

/// Compute an anonymous machine ID by hashing the hostname.
fn compute_machine_id() -> String {
    let hostname = get_hostname();
    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Get the system hostname.
fn get_hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Push metrics to the server. Best-effort — failures are logged but not propagated
/// to the caller in a way that would block git operations.
pub fn push_metrics(client: &HttpClient, payload: &MetricsPayload) -> Result<(), String> {
    let body = serde_json::to_value(payload).map_err(|e| format!("serialize metrics: {e}"))?;

    let response = client.post_json("/v1/metrics", &body)?;

    match response.status {
        200..=299 => Ok(()),
        other => Err(format!(
            "metrics push failed: HTTP {other}: {}",
            response.body
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_machine_id_is_deterministic() {
        let id1 = compute_machine_id();
        let id2 = compute_machine_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_machine_id_is_sha256_length() {
        let id = compute_machine_id();
        assert_eq!(id.len(), 64);
    }

    #[test]
    fn test_metrics_payload_serialization() {
        let payload = MetricsPayload {
            machine_id: "abc123".to_string(),
            version: "2.0.0".to_string(),
            agents_detected: vec!["claude".to_string(), "cursor".to_string()],
            total_commits_tracked: 42,
        };

        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["machine_id"], "abc123");
        assert_eq!(json["version"], "2.0.0");
        assert_eq!(json["agents_detected"][0], "claude");
        assert_eq!(json["agents_detected"][1], "cursor");
        assert_eq!(json["total_commits_tracked"], 42);
    }
}
