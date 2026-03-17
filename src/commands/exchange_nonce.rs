//! Exchange install nonce for credentials (auto-login from web install page)
//!
//! This command is called by the install script to exchange a nonce for
//! OAuth credentials. It reads INSTALL_NONCE and API_BASE from environment
//! variables and stores credentials in ~/.git-ai/internal/credentials.
//!
//! On failure, exits with code 1 silently so the install script can fall back
//! to running `git-ai login`. Errors are recorded server-side for debugging.

use crate::auth::CredentialStore;
use crate::auth::client::OAuthClient;

/// Handle the exchange-nonce command (internal - called by install scripts)
///
/// Exits with code 1 on failure (silently) so install script can run `git-ai login`.
/// Exits with code 0 on success.
pub fn handle_exchange_nonce(_args: &[String]) {
    // Read from environment variables (injected by install script)
    let nonce = std::env::var("INSTALL_NONCE")
        .ok()
        .filter(|s| !s.is_empty());
    let api_base = std::env::var("API_BASE").ok().filter(|s| !s.is_empty());

    // If no nonce provided, silently exit success (not an error - just means no auto-login)
    let Some(nonce) = nonce else {
        return;
    };

    // If API_BASE missing, exit with failure so login runs
    let Some(api_base) = api_base else {
        std::process::exit(1);
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
