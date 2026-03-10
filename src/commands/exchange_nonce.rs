//! Exchange a nonce for credentials
//!
//! Two modes:
//!   1. Install nonce (default): nonce from INSTALL_NONCE env var
//!   2. Impersonation nonce:     --impersonation-nonce <NONCE>
//!
//! Usage:
//!   git-ai exchange-nonce                          # install nonce from env
//!   git-ai exchange-nonce --impersonation-nonce X  # impersonation nonce
//!
//! API base is resolved from API_BASE env var, falling back to the
//! configured api_base_url (config file / GIT_AI_API_BASE_URL / default).
//!
//! On failure, exits with code 1 silently so the install script can fall back
//! to running `git-ai login`. Errors are recorded server-side for debugging.

use crate::auth::CredentialStore;
use crate::auth::client::OAuthClient;
use crate::config;

/// Handle the exchange-nonce command (internal - called by install scripts and background agents)
///
/// Exits with code 1 on failure (silently) so install script can run `git-ai login`.
/// Exits with code 0 on success.
pub fn handle_exchange_nonce(args: &[String]) {
    // API base: API_BASE env var, or config fallback
    let api_base = std::env::var("API_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| config::Config::get().api_base_url().to_string());

    // Check for --impersonation-nonce flag
    let impersonation_nonce = parse_flag(args, "--impersonation-nonce");

    if let Some(nonce) = impersonation_nonce {
        if exchange_impersonation_nonce(&nonce, &api_base).is_err() {
            std::process::exit(1);
        }
        return;
    }

    // Install nonce from env only
    let nonce = std::env::var("INSTALL_NONCE")
        .ok()
        .filter(|s| !s.is_empty());

    // If no nonce provided, silently exit success (not an error - just means no auto-login)
    let Some(nonce) = nonce else {
        return;
    };

    // Perform the exchange - exit with failure code on error (silently)
    // The error is already recorded server-side, so no need to print anything
    if exchange_install_nonce(&nonce, &api_base).is_err() {
        std::process::exit(1);
    }
}

/// Parse a --flag value pair from args
fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn exchange_install_nonce(nonce: &str, api_base: &str) -> Result<(), String> {
    let client = OAuthClient::with_base_url(api_base)?;
    let credentials = client.exchange_install_nonce(nonce)?;

    let store = CredentialStore::new();
    store.store(&credentials)?;

    eprintln!("\x1b[32m✓ Logged in automatically\x1b[0m");
    Ok(())
}

fn exchange_impersonation_nonce(nonce: &str, api_base: &str) -> Result<(), String> {
    let client = OAuthClient::with_base_url(api_base)?;
    let credentials = client.exchange_impersonation_nonce(nonce)?;

    let store = CredentialStore::new();
    store.store(&credentials)?;

    eprintln!("\x1b[32m✓ Authenticated via impersonation nonce\x1b[0m");
    Ok(())
}
