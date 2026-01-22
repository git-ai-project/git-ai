use crate::auth::{CredentialStore, OAuthClient};
use crate::config;
use crate::error::GitAiError;
use once_cell::sync::Lazy;
use std::sync::Mutex;
use url::Url;

/// Global mutex to prevent multiple threads from refreshing simultaneously.
/// This provides in-process synchronization to avoid thundering herd issues.
/// Note: Cross-process races are acceptable - both processes get valid tokens.
static REFRESH_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Attempt to load stored credentials and refresh if needed.
/// Returns None on any failure (not logged in, expired, refresh failed).
/// Uses in-process Mutex for thread safety during token refresh.
fn try_load_auth_token() -> Option<String> {
    let store = CredentialStore::new();

    let creds = match store.load() {
        Ok(Some(c)) => c,
        _ => return None,
    };

    // If refresh token expired, can't authenticate
    if creds.is_refresh_token_expired() {
        return None;
    }

    // Fast path: if access token is valid (with 5 min buffer), use it directly
    if !creds.is_access_token_expired(300) {
        return Some(creds.access_token);
    }

    // Need to refresh - acquire mutex to prevent thundering herd within this process
    // If mutex is poisoned (previous panic), we return None gracefully
    let _guard = REFRESH_LOCK.lock().ok()?;

    // Re-check credentials after acquiring lock - another thread may have refreshed
    let creds = match store.load() {
        Ok(Some(c)) => c,
        _ => return None,
    };

    // Check again if access token is now valid (another thread may have refreshed)
    if !creds.is_access_token_expired(300) {
        return Some(creds.access_token);
    }

    // Still expired - we need to refresh
    let client = OAuthClient::new();
    match client.refresh_access_token(&creds.refresh_token) {
        Ok(new_creds) => {
            // Store refreshed credentials (ignore errors - we still have the token)
            let _ = store.store(&new_creds);
            Some(new_creds.access_token)
        }
        Err(_) => None,
    }
    // Mutex guard is automatically released when _guard is dropped
}

/// API client context with optional authentication
#[derive(Debug, Clone)]
pub struct ApiContext {
    /// Base URL for the API (e.g., "https://app.com")
    pub base_url: String,
    /// Optional authentication token
    pub auth_token: Option<String>,
    /// Optional API key for X-API-Key header
    pub api_key: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: Option<u64>,
}

impl ApiContext {
    /// Get the default API base URL from config
    fn default_base_url() -> String {
        config::Config::get().api_base_url().to_string()
    }

    /// Create a GET request with common headers (User-Agent, X-Distinct-ID)
    /// Use this for all HTTP GET requests to ensure consistent headers.
    pub fn http_get(url: &str) -> minreq::Request {
        minreq::get(url)
            .with_header(
                "User-Agent",
                format!("git-ai/{}", env!("CARGO_PKG_VERSION")),
            )
            .with_header("X-Distinct-ID", config::get_or_create_distinct_id())
    }

    /// Create a POST request with common headers (User-Agent, X-Distinct-ID)
    /// Use this for all HTTP POST requests to ensure consistent headers.
    pub fn http_post(url: &str) -> minreq::Request {
        minreq::post(url)
            .with_header(
                "User-Agent",
                format!("git-ai/{}", env!("CARGO_PKG_VERSION")),
            )
            .with_header("X-Distinct-ID", config::get_or_create_distinct_id())
    }

    /// Create a new API context, automatically using stored credentials if available
    /// If base_url is None, uses api_base_url from config (which can be set via config file, env var, or defaults)
    pub fn new(base_url: Option<String>) -> Self {
        let cfg = config::Config::get();
        Self {
            base_url: base_url.unwrap_or_else(Self::default_base_url),
            auth_token: try_load_auth_token(),
            api_key: cfg.api_key().map(|s| s.to_string()),
            timeout_secs: Some(30),
        }
    }

    /// Create a new API context explicitly without authentication
    /// Use this when you need to ensure no auth token is sent
    pub fn without_auth(base_url: Option<String>) -> Self {
        let cfg = config::Config::get();
        Self {
            base_url: base_url.unwrap_or_else(Self::default_base_url),
            auth_token: None,
            api_key: cfg.api_key().map(|s| s.to_string()),
            timeout_secs: Some(30),
        }
    }

    /// Create a new API context with authentication
    /// If base_url is None, uses api_base_url from config (which can be set via config file, env var, or defaults)
    pub fn with_auth(base_url: Option<String>, auth_token: String) -> Self {
        let cfg = config::Config::get();
        Self {
            base_url: base_url.unwrap_or_else(Self::default_base_url),
            auth_token: Some(auth_token),
            api_key: cfg.api_key().map(|s| s.to_string()),
            timeout_secs: Some(30),
        }
    }

    /// Set a custom timeout
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = Some(timeout_secs);
        self
    }

    /// Build the full URL for an endpoint
    fn build_url(&self, endpoint: &str) -> Result<String, GitAiError> {
        let base = Url::parse(&self.base_url)
            .map_err(|e| GitAiError::Generic(format!("Invalid base URL: {}", e)))?;
        let url = base
            .join(endpoint)
            .map_err(|e| GitAiError::Generic(format!("Invalid endpoint URL: {}", e)))?;
        Ok(url.to_string())
    }

    /// Make a POST request with JSON body
    pub fn post_json<T: serde::Serialize>(
        &self,
        endpoint: &str,
        body: &T,
    ) -> Result<minreq::Response, GitAiError> {
        let url = self.build_url(endpoint)?;
        let body_json = serde_json::to_string(body).map_err(GitAiError::JsonError)?;

        let mut request = Self::http_post(&url)
            .with_header("Content-Type", "application/json")
            .with_body(body_json);

        // Add authentication header if token is present
        if let Some(token) = &self.auth_token {
            request = request.with_header("Authorization", format!("Bearer {}", token));
        }

        // Add API key header if present
        if let Some(api_key) = &self.api_key {
            request = request.with_header("X-API-Key", api_key);
        }

        // Set timeout if specified
        if let Some(timeout) = self.timeout_secs {
            request = request.with_timeout(timeout);
        }

        let response = request
            .send()
            .map_err(|e| GitAiError::Generic(format!("HTTP request failed: {}", e)))?;

        Ok(response)
    }

    /// Make a GET request
    pub fn get(&self, endpoint: &str) -> Result<minreq::Response, GitAiError> {
        let url = self.build_url(endpoint)?;

        let mut request = Self::http_get(&url);

        // Add authentication header if token is present
        if let Some(token) = &self.auth_token {
            request = request.with_header("Authorization", format!("Bearer {}", token));
        }

        // Add API key header if present
        if let Some(api_key) = &self.api_key {
            request = request.with_header("X-API-Key", api_key);
        }

        // Set timeout if specified
        if let Some(timeout) = self.timeout_secs {
            request = request.with_timeout(timeout);
        }

        let response = request
            .send()
            .map_err(|e| GitAiError::Generic(format!("HTTP request failed: {}", e)))?;

        Ok(response)
    }
}

/// API client wrapper
#[derive(Debug, Clone)]
pub struct ApiClient {
    context: ApiContext,
}

impl ApiClient {
    /// Create a new API client with the given context
    pub fn new(context: ApiContext) -> Self {
        Self { context }
    }

    /// Get a reference to the API context
    pub fn context(&self) -> &ApiContext {
        &self.context
    }

    /// Get a mutable reference to the API context
    pub fn context_mut(&mut self) -> &mut ApiContext {
        &mut self.context
    }

    /// Check if user is logged in (has an auth token)
    pub fn is_logged_in(&self) -> bool {
        self.context.auth_token.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ============= ApiContext Tests =============

    #[test]
    fn test_api_context_without_auth() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()));
        assert!(ctx.auth_token.is_none());
        assert_eq!(ctx.base_url, "https://example.com");
    }

    #[test]
    fn test_api_context_with_auth() {
        let ctx = ApiContext::with_auth(
            Some("https://example.com".to_string()),
            "test_token".to_string(),
        );
        assert_eq!(ctx.auth_token, Some("test_token".to_string()));
        assert_eq!(ctx.base_url, "https://example.com");
    }

    #[test]
    fn test_api_context_with_timeout() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()))
            .with_timeout(60);
        assert_eq!(ctx.timeout_secs, Some(60));
    }

    #[test]
    fn test_api_context_default_timeout() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()));
        assert_eq!(ctx.timeout_secs, Some(30));
    }

    // ============= ApiClient Tests =============

    #[test]
    fn test_api_client_is_logged_in_true() {
        let ctx = ApiContext::with_auth(
            Some("https://example.com".to_string()),
            "token".to_string(),
        );
        let client = ApiClient::new(ctx);
        assert!(client.is_logged_in());
    }

    #[test]
    fn test_api_client_is_logged_in_false() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()));
        let client = ApiClient::new(ctx);
        assert!(!client.is_logged_in());
    }

    #[test]
    fn test_api_client_context_access() {
        let ctx = ApiContext::with_auth(
            Some("https://example.com".to_string()),
            "token".to_string(),
        );
        let client = ApiClient::new(ctx);
        assert_eq!(client.context().base_url, "https://example.com");
    }

    // ============= URL Building Tests =============

    #[test]
    fn test_build_url_simple() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()));
        let url = ctx.build_url("/api/test").unwrap();
        assert_eq!(url, "https://example.com/api/test");
    }

    #[test]
    fn test_build_url_with_trailing_slash() {
        let ctx = ApiContext::without_auth(Some("https://example.com/".to_string()));
        let url = ctx.build_url("api/test").unwrap();
        assert_eq!(url, "https://example.com/api/test");
    }

    #[test]
    fn test_build_url_invalid_base() {
        let ctx = ApiContext::without_auth(Some("not-a-url".to_string()));
        let result = ctx.build_url("/api/test");
        assert!(result.is_err());
    }

    // ============= Mutex Thread Safety Tests =============

    #[test]
    fn test_mutex_is_accessible() {
        // Simple test to verify the mutex can be locked
        let guard = REFRESH_LOCK.lock();
        assert!(guard.is_ok());
        // Guard drops here, releasing the lock
    }

    #[test]
    fn test_concurrent_access_to_mutex() {
        // Test that multiple threads can safely contend for the mutex
        let counter = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        for _ in 0..5 {
            let counter_clone = counter.clone();
            let handle = std::thread::spawn(move || {
                if let Ok(_guard) = REFRESH_LOCK.lock() {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All threads should have acquired the lock sequentially
        let final_count = counter.load(Ordering::SeqCst);
        assert_eq!(final_count, 5);
    }
}

