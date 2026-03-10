//! Exchange install nonce for credentials (auto-login from web install page)
//!
//! This command is called by the install script to exchange a nonce for
//! OAuth credentials.
//!
//! Usage: git-ai exchange-nonce [NONCE]
//!
//! Nonce can be passed as the first argument or via INSTALL_NONCE env var.
//! API base is resolved from API_BASE env var, falling back to the
//! configured api_base_url (config file / GIT_AI_API_BASE_URL / default).
//!
//! On failure, exits with code 1 silently so the install script can fall back
//! to running `git-ai login`. Errors are recorded server-side for debugging.

use crate::auth::CredentialStore;
use crate::auth::client::OAuthClient;
use crate::config;

/// Handle the exchange-nonce command (internal - called by install scripts)
///
/// Exits with code 1 on failure (silently) so install script can run `git-ai login`.
/// Exits with code 0 on success.
pub fn handle_exchange_nonce(args: &[String]) {
    // Nonce: first arg, or INSTALL_NONCE env var
    let nonce = args.first().filter(|s| !s.is_empty()).cloned().or_else(|| {
        std::env::var("INSTALL_NONCE")
            .ok()
            .filter(|s| !s.is_empty())
    });

    // API base: API_BASE env var, or config fallback
    let api_base = std::env::var("API_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| config::Config::get().api_base_url().to_string());

    // If no nonce provided, silently exit success (not an error - just means no auto-login)
    let Some(nonce) = nonce else {
        return;
    };

    // Perform the exchange - exit with failure code on error (silently)
    // The error is already recorded server-side, so no need to print anything
    if exchange_nonce(&nonce, &api_base).is_err() {
        std::process::exit(1);
    }
}

fn exchange_nonce(nonce: &str, api_base: &str) -> Result<(), String> {
    // Create OAuth client with custom base URL
    let client = OAuthClient::with_base_url(api_base)?;

    // Exchange the nonce for credentials
    let credentials = client.exchange_install_nonce(nonce)?;

    // Store credentials
    let store = CredentialStore::new();
    store.store(&credentials)?;

    eprintln!("\x1b[32m✓ Logged in automatically\x1b[0m");
    Ok(())
}
