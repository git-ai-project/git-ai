use serde::{Deserialize, Serialize};
use std::fmt;

/// Stored credentials for OAuth tokens
/// NOTE: Debug intentionally redacts tokens to prevent accidental exposure in logs
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredCredentials {
    /// The access token (short-lived, 1 hour)
    pub access_token: String,
    /// The refresh token (long-lived, 90 days)
    pub refresh_token: String,
    /// Unix timestamp when the access token expires
    pub access_token_expires_at: i64,
    /// Unix timestamp when the refresh token expires
    pub refresh_token_expires_at: i64,
}

/// Custom Debug implementation that redacts sensitive token values
impl fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

impl StoredCredentials {
    /// Check if the access token is expired or will expire within the given buffer (seconds)
    pub fn is_access_token_expired(&self, buffer_secs: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.access_token_expires_at <= now + buffer_secs
    }

    /// Check if the refresh token is expired
    pub fn is_refresh_token_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.refresh_token_expires_at <= now
    }
}

/// Response from device authorization endpoint
#[derive(Debug, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u32,
    pub interval: u32,
}

/// Response from token endpoint
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[allow(dead_code)]
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    pub refresh_expires_in: u64,
}

/// OAuth error response
#[derive(Debug, Deserialize)]
pub struct OAuthError {
    pub error: String,
    pub error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_credentials(access_expires_at: i64, refresh_expires_at: i64) -> StoredCredentials {
        StoredCredentials {
            access_token: "test_access_token".to_string(),
            refresh_token: "test_refresh_token".to_string(),
            access_token_expires_at: access_expires_at,
            refresh_token_expires_at: refresh_expires_at,
        }
    }

    // ============= is_access_token_expired() tests =============

    #[test]
    fn test_access_token_not_expired() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now + 86400 * 90); // expires in 1 hour
        assert!(!creds.is_access_token_expired(300)); // 5 min buffer
    }

    #[test]
    fn test_access_token_expired() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now - 3600, now + 86400 * 90); // expired 1 hour ago
        assert!(creds.is_access_token_expired(0));
    }

    #[test]
    fn test_access_token_expires_within_buffer() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 200, now + 86400 * 90); // expires in 200s
        assert!(creds.is_access_token_expired(300)); // 300s buffer - should be "expired"
    }

    #[test]
    fn test_access_token_exactly_at_boundary() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 300, now + 86400 * 90); // expires in exactly 300s
        // now + buffer = now + 300, expires_at = now + 300
        // expires_at <= now + buffer, so should be expired (boundary case)
        assert!(creds.is_access_token_expired(300));
    }

    #[test]
    fn test_access_token_one_second_after_boundary() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 301, now + 86400 * 90); // expires in 301s
        assert!(!creds.is_access_token_expired(300)); // 300s buffer - should NOT be expired
    }

    #[test]
    fn test_access_token_zero_buffer() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 1, now + 86400 * 90); // expires in 1s
        assert!(!creds.is_access_token_expired(0)); // no buffer
    }

    #[test]
    fn test_access_token_negative_buffer() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 100, now + 86400 * 90);
        // Negative buffer effectively means we're more lenient
        assert!(!creds.is_access_token_expired(-50));
    }

    #[test]
    fn test_access_token_large_buffer() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now + 86400 * 90); // expires in 1 hour
        assert!(creds.is_access_token_expired(7200)); // 2 hour buffer - should be "expired"
    }

    // ============= is_refresh_token_expired() tests =============

    #[test]
    fn test_refresh_token_not_expired() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now + 86400 * 90); // refresh expires in 90 days
        assert!(!creds.is_refresh_token_expired());
    }

    #[test]
    fn test_refresh_token_expired() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now - 86400); // refresh expired 1 day ago
        assert!(creds.is_refresh_token_expired());
    }

    #[test]
    fn test_refresh_token_exactly_at_boundary() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now); // refresh expires exactly now
        // expires_at <= now should be true (boundary case)
        assert!(creds.is_refresh_token_expired());
    }

    #[test]
    fn test_refresh_token_one_second_before_expiry() {
        let now = chrono::Utc::now().timestamp();
        let creds = make_credentials(now + 3600, now + 1); // refresh expires in 1s
        assert!(!creds.is_refresh_token_expired());
    }

    // ============= Debug implementation tests =============

    #[test]
    fn test_debug_redacts_access_token() {
        let creds = make_credentials(1000, 2000);
        let debug_output = format!("{:?}", creds);
        assert!(debug_output.contains("[REDACTED]"));
        assert!(!debug_output.contains("test_access_token"));
    }

    #[test]
    fn test_debug_redacts_refresh_token() {
        let creds = make_credentials(1000, 2000);
        let debug_output = format!("{:?}", creds);
        assert!(!debug_output.contains("test_refresh_token"));
    }

    #[test]
    fn test_debug_shows_timestamps() {
        let creds = make_credentials(1234567890, 9876543210);
        let debug_output = format!("{:?}", creds);
        assert!(debug_output.contains("1234567890"));
        assert!(debug_output.contains("9876543210"));
    }
}
