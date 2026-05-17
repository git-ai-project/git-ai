use crate::api::client::HttpClient;
use crate::auth::types::{DeviceAuthResponse, OAuthError, StoredCredentials, TokenResponse};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Default API base URL
const DEFAULT_API_BASE_URL: &str = "https://api.git-ai.com";

/// OAuth client for device authorization flow
pub struct OAuthClient {
    base_url: String,
}

/// Get the user's home directory.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

/// Read the API base URL from `~/.git-ai/config.json`.
/// Falls back to the default if the config doesn't exist or the field is absent.
fn read_api_base_url() -> String {
    let path = match home_dir() {
        Some(h) => h.join(".git-ai").join("config.json"),
        None => return DEFAULT_API_BASE_URL.to_string(),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return DEFAULT_API_BASE_URL.to_string(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return DEFAULT_API_BASE_URL.to_string(),
    };
    parsed
        .get("api_base_url")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_API_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

/// Validate that a URL uses HTTPS (security requirement for OAuth)
/// In release builds, only HTTPS is accepted.
/// In debug builds, HTTP is also allowed for local development.
#[cfg(not(debug_assertions))]
fn validate_https_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!(
            "Security error: OAuth requires HTTPS. URL '{}' is not secure.",
            url
        ));
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn validate_https_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("Invalid URL scheme: {}", url));
    }
    Ok(())
}

impl OAuthClient {
    /// Create a new OAuth client using the current config
    pub fn new() -> Self {
        let base_url = read_api_base_url();

        // Validate HTTPS in release mode (panics on invalid URL - fail-safe)
        if let Err(e) = validate_https_url(&base_url) {
            panic!("{}", e);
        }

        Self { base_url }
    }

    /// Create an OAuthClient with a custom base URL (for install script flow)
    pub fn with_base_url(base_url: &str) -> Result<Self, String> {
        validate_https_url(base_url)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Create an HttpClient for making requests (no auth token needed for OAuth endpoints)
    fn http_client(&self) -> HttpClient {
        HttpClient::new(&self.base_url, None)
    }

    /// Common token exchange logic - POST to /worker/oauth/token with given body
    fn exchange_token(&self, body: serde_json::Value) -> Result<StoredCredentials, String> {
        let client = self.http_client();
        let response = client.post_json("/worker/oauth/token", &body)?;

        if response.status != 200 {
            let error: OAuthError = serde_json::from_str(&response.body).unwrap_or(OAuthError {
                error: "unknown_error".to_string(),
                error_description: None,
            });

            let msg = error
                .error_description
                .unwrap_or_else(|| error.error.clone());
            return Err(msg);
        }

        let token_response: TokenResponse = serde_json::from_str(&response.body)
            .map_err(|e| format!("Invalid token response: {}", e))?;

        let now = chrono::Utc::now().timestamp();
        Ok(StoredCredentials {
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            access_token_expires_at: now + token_response.expires_in as i64,
            refresh_token_expires_at: now + token_response.refresh_expires_in as i64,
        })
    }

    /// Start the device authorization flow
    /// Returns (device_code, user_code, verification_url, expires_in, interval)
    pub fn start_device_flow(&self) -> Result<DeviceAuthResponse, String> {
        let client = self.http_client();
        let body = serde_json::json!({});
        let response = client.post_json("/worker/oauth/device/code", &body)?;

        if response.status != 200 {
            return Err(format!(
                "Server error ({}): {}",
                response.status, response.body
            ));
        }

        serde_json::from_str::<DeviceAuthResponse>(&response.body)
            .map_err(|e| format!("Invalid response from server: {}", e))
    }

    /// Poll for token with the given device code
    /// Implements RFC 8628 polling with proper error handling
    pub fn poll_for_token(
        &self,
        device_code: &str,
        interval: u32,
        expires_in: u32,
    ) -> Result<StoredCredentials, String> {
        let mut elapsed = 0u32;
        let mut current_interval = interval;

        while elapsed < expires_in {
            // Wait before polling
            thread::sleep(Duration::from_secs(current_interval as u64));
            elapsed += current_interval;

            let body = serde_json::json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": device_code,
                "client_id": "git-ai-cli"
            });

            let client = self.http_client();
            let response = client.post_json("/worker/oauth/token", &body)?;

            if response.status == 200 {
                let token_response: TokenResponse = serde_json::from_str(&response.body)
                    .map_err(|e| format!("Invalid token response: {}", e))?;

                let now = chrono::Utc::now().timestamp();
                return Ok(StoredCredentials {
                    access_token: token_response.access_token,
                    refresh_token: token_response.refresh_token,
                    access_token_expires_at: now + token_response.expires_in as i64,
                    refresh_token_expires_at: now + token_response.refresh_expires_in as i64,
                });
            }

            // Parse error response
            let error: OAuthError = match serde_json::from_str(&response.body) {
                Ok(e) => e,
                Err(_) => {
                    return Err(format!("Server error ({})", response.status));
                }
            };

            match error.error.as_str() {
                "authorization_pending" => {
                    // Keep polling - user hasn't approved yet
                    continue;
                }
                "slow_down" => {
                    // Increase interval by 5 seconds per RFC 8628
                    current_interval += 5;
                    continue;
                }
                "access_denied" => {
                    return Err("Authorization was denied".to_string());
                }
                "expired_token" => {
                    return Err("Device code expired. Please try again.".to_string());
                }
                _ => {
                    let msg = error
                        .error_description
                        .unwrap_or_else(|| error.error.clone());
                    return Err(format!("Authorization failed: {}", msg));
                }
            }
        }

        Err("Device code expired. Please try again.".to_string())
    }

    /// Refresh the access token using a refresh token
    pub fn refresh_access_token(&self, refresh_token: &str) -> Result<StoredCredentials, String> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": "git-ai-cli"
        });

        self.exchange_token(body)
            .map_err(|e| format!("Token refresh failed: {}", e))
    }

    /// Exchange an install nonce for credentials (auto-login from web install page)
    pub fn exchange_install_nonce(&self, nonce: &str) -> Result<StoredCredentials, String> {
        let body = serde_json::json!({
            "grant_type": "install_nonce",
            "install_nonce": nonce,
            "client_id": "git-ai-cli"
        });

        self.exchange_token(body)
            .map_err(|e| format!("Nonce exchange failed: {}", e))
    }
}

impl Default for OAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============= URL Validation Tests =============

    #[test]
    fn test_validate_https_url_valid() {
        let result = validate_https_url("https://example.com");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_https_url_with_path() {
        let result = validate_https_url("https://example.com/api/v1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_https_url_with_port() {
        let result = validate_https_url("https://example.com:8443/api");
        assert!(result.is_ok());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_validate_https_url_http_allowed_in_debug() {
        let result = validate_https_url("http://localhost:8080");
        assert!(result.is_ok());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn test_validate_https_url_http_rejected_in_release() {
        let result = validate_https_url("http://example.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn test_validate_https_url_invalid_scheme() {
        let result = validate_https_url("ftp://example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_https_url_no_scheme() {
        let result = validate_https_url("example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_https_url_empty() {
        let result = validate_https_url("");
        assert!(result.is_err());
    }

    // ============= Token Response Parsing Tests =============

    #[test]
    fn test_parse_token_response_valid() {
        let json = r#"{
            "access_token": "test_access",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "test_refresh",
            "refresh_expires_in": 7776000
        }"#;

        let result: Result<TokenResponse, _> = serde_json::from_str(json);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.access_token, "test_access");
        assert_eq!(response.refresh_token, "test_refresh");
        assert_eq!(response.expires_in, 3600);
        assert_eq!(response.refresh_expires_in, 7776000);
    }

    #[test]
    fn test_parse_token_response_missing_field() {
        let json = r#"{
            "access_token": "test_access",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "test_refresh"
        }"#;

        let result: Result<TokenResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_token_response_wrong_types() {
        let json = r#"{
            "access_token": "test_access",
            "token_type": "Bearer",
            "expires_in": "3600",
            "refresh_token": "test_refresh",
            "refresh_expires_in": 7776000
        }"#;

        let result: Result<TokenResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ============= OAuth Error Parsing Tests =============

    #[test]
    fn test_parse_oauth_error_with_description() {
        let json = r#"{
            "error": "invalid_grant",
            "error_description": "The refresh token is expired"
        }"#;

        let result: Result<OAuthError, _> = serde_json::from_str(json);
        assert!(result.is_ok());

        let error = result.unwrap();
        assert_eq!(error.error, "invalid_grant");
        assert_eq!(
            error.error_description,
            Some("The refresh token is expired".to_string())
        );
    }

    #[test]
    fn test_parse_oauth_error_without_description() {
        let json = r#"{"error": "access_denied"}"#;

        let result: Result<OAuthError, _> = serde_json::from_str(json);
        assert!(result.is_ok());

        let error = result.unwrap();
        assert_eq!(error.error, "access_denied");
        assert!(error.error_description.is_none());
    }

    // ============= Device Auth Response Parsing Tests =============

    #[test]
    fn test_parse_device_auth_response_valid() {
        let json = r#"{
            "device_code": "abc123",
            "user_code": "WXYZ-1234",
            "verification_uri": "https://example.com/verify",
            "verification_uri_complete": "https://example.com/verify?code=WXYZ-1234",
            "expires_in": 900,
            "interval": 5
        }"#;

        let result: Result<DeviceAuthResponse, _> = serde_json::from_str(json);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.device_code, "abc123");
        assert_eq!(response.user_code, "WXYZ-1234");
        assert_eq!(response.interval, 5);
        assert_eq!(response.expires_in, 900);
    }

    #[test]
    fn test_parse_device_auth_response_without_optional_uri() {
        let json = r#"{
            "device_code": "abc123",
            "user_code": "WXYZ-1234",
            "verification_uri": "https://example.com/verify",
            "expires_in": 900,
            "interval": 5
        }"#;

        let result: Result<DeviceAuthResponse, _> = serde_json::from_str(json);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.verification_uri_complete.is_none());
    }

    // ============= Credentials Calculation Tests =============

    #[test]
    fn test_credentials_expiry_calculation() {
        let now = chrono::Utc::now().timestamp();
        let expires_in: u64 = 3600;
        let refresh_expires_in: u64 = 7776000;

        let creds = StoredCredentials {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            access_token_expires_at: now + expires_in as i64,
            refresh_token_expires_at: now + refresh_expires_in as i64,
        };

        assert!(creds.access_token_expires_at > now);
        assert!(creds.access_token_expires_at <= now + 3601);

        assert!(creds.refresh_token_expires_at > now + 86400 * 89);
        assert!(creds.refresh_token_expires_at <= now + 86400 * 91);
    }

    // ============= with_base_url Tests =============

    #[test]
    fn test_with_base_url_https() {
        let client = OAuthClient::with_base_url("https://example.com/api/");
        assert!(client.is_ok());
        assert_eq!(client.unwrap().base_url, "https://example.com/api");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_with_base_url_http_debug() {
        let client = OAuthClient::with_base_url("http://localhost:8080");
        assert!(client.is_ok());
    }

    #[test]
    fn test_with_base_url_invalid() {
        let client = OAuthClient::with_base_url("ftp://example.com");
        assert!(client.is_err());
    }
}
