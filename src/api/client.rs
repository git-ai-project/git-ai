use std::process::Command;
use std::thread;
use std::time::Duration;

/// Response from an HTTP request.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Minimal HTTP client that shells out to `curl`.
/// No async runtime or external HTTP crate required.
#[derive(Debug, Clone)]
pub struct HttpClient {
    pub base_url: String,
    pub auth_token: Option<String>,
}

impl HttpClient {
    /// Create a new client targeting the given base URL.
    pub fn new(base_url: &str, auth_token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token,
        }
    }

    /// POST JSON to `{base_url}{path}`.
    /// Retries up to 3 times with exponential backoff (1s, 2s, 4s).
    pub fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<HttpResponse, String> {
        let body_str = serde_json::to_string(body).map_err(|e| format!("json serialize: {e}"))?;
        self.request("POST", path, Some(("application/json", &body_str)))
    }

    /// POST raw body to `{base_url}{path}` with `application/octet-stream` content type.
    /// Used for CAS uploads where the body is the raw note content.
    pub fn post_raw(&self, path: &str, body: &str) -> Result<HttpResponse, String> {
        self.request("POST", path, Some(("application/octet-stream", body)))
    }

    /// GET `{base_url}{path}`.
    /// Retries up to 3 times with exponential backoff (1s, 2s, 4s).
    pub fn get(&self, path: &str) -> Result<HttpResponse, String> {
        self.request("GET", path, None)
    }

    /// Unified request method. `body` is `(content_type, data)` if present.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<(&str, &str)>,
    ) -> Result<HttpResponse, String> {
        let url = format!("{}{}", self.base_url, path);
        let auth_header = self.auth_token.as_ref().map(|t| format!("Authorization: Bearer {t}"));

        self.execute_with_retry(|| {
            let mut cmd = Command::new("curl");
            cmd.args(["-s", "-S", "--max-time", "30"])
                .args(["-o", "-", "-w", "\n%{http_code}"])
                .args(["-X", method]);

            if let Some((content_type, data)) = body {
                cmd.args(["-H", &format!("Content-Type: {content_type}")])
                    .args(["-d", data]);
            }

            if let Some(h) = &auth_header {
                cmd.args(["-H", h]);
            }

            cmd.arg(&url);
            cmd
        })
    }

    /// Execute a curl command with retry logic.
    /// 3 attempts, exponential backoff: 1s, 2s, 4s.
    fn execute_with_retry<F>(&self, build_cmd: F) -> Result<HttpResponse, String>
    where
        F: Fn() -> Command,
    {
        let backoff_durations = [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
        ];
        let max_attempts = 3;
        let mut last_error = String::new();

        for attempt in 0..max_attempts {
            let mut cmd = build_cmd();
            match cmd.output() {
                Ok(output) => {
                    if !output.status.success() && output.stdout.is_empty() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        last_error = format!("curl failed: {stderr}");
                    } else {
                        let raw = String::from_utf8_lossy(&output.stdout).to_string();
                        return parse_curl_output(&raw);
                    }
                }
                Err(e) => {
                    last_error = format!("curl exec error: {e}");
                }
            }

            // Sleep before retry (but not after the last attempt)
            if attempt < max_attempts - 1 {
                thread::sleep(backoff_durations[attempt]);
            }
        }

        Err(last_error)
    }
}

/// Parse curl output formatted with `-w "\n%{http_code}"`.
/// The last line is the HTTP status code; everything before is the response body.
fn parse_curl_output(raw: &str) -> Result<HttpResponse, String> {
    // The status code is the last line
    let raw_trimmed = raw.trim_end();
    match raw_trimmed.rsplit_once('\n') {
        Some((body, status_str)) => {
            let status: u16 = status_str
                .trim()
                .parse()
                .map_err(|e| format!("invalid status code '{status_str}': {e}"))?;
            Ok(HttpResponse {
                status,
                body: body.to_string(),
            })
        }
        None => {
            // No newline — the entire output might be just a status code (empty body)
            let status: u16 = raw_trimmed
                .trim()
                .parse()
                .map_err(|_| format!("unexpected curl output: {raw}"))?;
            Ok(HttpResponse {
                status,
                body: String::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_curl_output_with_body() {
        let raw = "{\"ok\":true}\n200";
        let resp = parse_curl_output(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"ok\":true}");
    }

    #[test]
    fn test_parse_curl_output_empty_body() {
        let raw = "204";
        let resp = parse_curl_output(raw).unwrap();
        assert_eq!(resp.status, 204);
        assert_eq!(resp.body, "");
    }

    #[test]
    fn test_parse_curl_output_multiline_body() {
        let raw = "line1\nline2\n200";
        let resp = parse_curl_output(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "line1\nline2");
    }

    #[test]
    fn test_client_url_construction() {
        let client = HttpClient::new("https://api.example.com/", None);
        assert_eq!(client.base_url, "https://api.example.com");

        let client2 = HttpClient::new("https://api.example.com", None);
        assert_eq!(client2.base_url, "https://api.example.com");
    }
}
