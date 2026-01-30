//! Exchange install nonce for credentials (auto-login from web install page)
//!
//! This command is called by the install script to exchange a nonce for
//! OAuth credentials. It reads INSTALL_NONCE and API_BASE from environment
//! variables and stores credentials in ~/.git-ai/internal/credentials.

use crate::api::client::ApiContext;
use crate::auth::types::StoredCredentials;
use crate::auth::CredentialStore;

use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// Token response from the OAuth endpoint
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: Option<u64>,
    refresh_expires_in: Option<u64>,
}

/// Error response from the OAuth endpoint
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

/// Handle the exchange-nonce command (internal - called by install scripts)
pub fn handle_exchange_nonce(_args: &[String]) {
    // Read from environment variables (injected by install script)
    let nonce = std::env::var("INSTALL_NONCE").ok().filter(|s| !s.is_empty());
    let api_base = std::env::var("API_BASE").ok().filter(|s| !s.is_empty());
    let install_page_url = std::env::var("INSTALL_PAGE_URL").ok().filter(|s| !s.is_empty());

    // If no nonce provided, silently exit (not an error - just means no auto-login)
    let Some(nonce) = nonce else {
        return;
    };

    let Some(api_base) = api_base else {
        eprintln!("\x1b[33mWarning: INSTALL_NONCE set but API_BASE missing\x1b[0m");
        return;
    };

    // Perform the exchange
    if let Err(e) = exchange_nonce(&nonce, &api_base, install_page_url.as_deref()) {
        eprintln!("{}", e);
        // Don't exit with error - install should continue even if login fails
    }
}

fn exchange_nonce(nonce: &str, api_base: &str, install_page_url: Option<&str>) -> Result<(), String> {
    eprintln!("Exchanging install nonce for credentials...");

    // Build the token request
    let url = format!("{}/worker/oauth/token", api_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "grant_type": "install_nonce",
        "install_nonce": nonce,
        "client_id": "git-ai-cli"
    });

    // Make the HTTP request using the existing ApiContext
    let response = ApiContext::http_post(&url)
        .with_header("Content-Type", "application/json")
        .with_body(body.to_string())
        .with_timeout(30)
        .send()
        .map_err(|e| format_error("Failed to connect to server", e.to_string(), install_page_url))?;

    let response_text = response
        .as_str()
        .map_err(|e| format_error("Failed to read response", e.to_string(), install_page_url))?;

    // Check for error response
    if response.status_code != 200 {
        // Try to parse as error response
        if let Ok(err) = serde_json::from_str::<ErrorResponse>(response_text) {
            let msg = err.error_description.unwrap_or(err.error);
            return Err(format_error("Nonce exchange failed", msg, install_page_url));
        }
        return Err(format_error(
            "Server error",
            format!("Status {}", response.status_code),
            install_page_url,
        ));
    }

    // Parse success response
    let token_response: TokenResponse = serde_json::from_str(response_text)
        .map_err(|e| format_error("Invalid server response", e.to_string(), install_page_url))?;

    // Calculate expiry timestamps
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let access_expires = now + token_response.expires_in.unwrap_or(3600) as i64;
    let refresh_expires = now + token_response.refresh_expires_in.unwrap_or(7776000) as i64;

    // Build credentials using the existing StoredCredentials type
    let credentials = StoredCredentials {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        access_token_expires_at: access_expires,
        refresh_token_expires_at: refresh_expires,
    };

    // Store using the existing CredentialStore
    let store = CredentialStore::new();
    store.store(&credentials)
        .map_err(|e| format!("\x1b[33mWarning: Failed to store credentials: {}\x1b[0m", e))?;

    eprintln!("\x1b[32mSuccessfully logged in\x1b[0m");
    Ok(())
}

fn format_error(_prefix: &str, _detail: String, install_page_url: Option<&str>) -> String {
    if let Some(url) = install_page_url {
        format!(
            "\x1b[33mNonce exchange failed. Get a new install command from:\n  {}\x1b[0m",
            url
        )
    } else {
        "\x1b[33mNonce exchange failed. Get a new install command from your organization's install page.\x1b[0m".to_string()
    }
}
