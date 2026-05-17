use git_ai::auth::CredentialStore;
use git_ai::auth::client::OAuthClient;

pub fn handle_exchange_nonce(_args: &[String]) {
    let nonce = std::env::var("INSTALL_NONCE")
        .ok()
        .filter(|s| !s.is_empty());
    let api_base = std::env::var("API_BASE").ok().filter(|s| !s.is_empty());

    let Some(nonce) = nonce else {
        return;
    };

    let Some(api_base) = api_base else {
        std::process::exit(1);
    };

    if exchange_nonce(&nonce, &api_base).is_err() {
        std::process::exit(1);
    }
}

fn exchange_nonce(nonce: &str, api_base: &str) -> Result<(), String> {
    let client = OAuthClient::with_base_url(api_base)?;
    let credentials = client.exchange_install_nonce(nonce)?;

    let store = CredentialStore::new();
    store.store(&credentials)?;

    eprintln!("\x1b[32m✓ Logged in automatically\x1b[0m");
    Ok(())
}
