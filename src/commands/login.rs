use crate::auth::{CredentialStore, OAuthClient};
use crate::commands::flush_metrics_db::spawn_background_metrics_db_flush;
use crate::metrics::db::MetricsDatabase;

/// Handle the `git-ai login` command
pub fn handle_login(_args: &[String]) {
    let store = CredentialStore::new();

    // Check if already logged in
    if let Ok(Some(creds)) = store.load()
        && !creds.is_refresh_token_expired() {
            eprintln!("Already logged in. Use 'git-ai logout' to log out first.");
            std::process::exit(0);
        }

    let client = OAuthClient::new();

    // Start device flow
    eprintln!("Starting device authorization...\n");

    let auth_response = match client.start_device_flow() {
        Ok(response) => response,
        Err(e) => {
            eprintln!("Failed to start authorization: {}", e);
            std::process::exit(1);
        }
    };

    // Build the display URL
    let display_url = auth_response
        .verification_uri_complete
        .as_ref()
        .unwrap_or(&auth_response.verification_uri);

    // Display instructions
    eprintln!("To authorize this device:");
    eprintln!("  1. Open this URL in your browser:");
    eprintln!("     {}", display_url);
    eprintln!();
    eprintln!("  2. Enter this code when prompted:");
    eprintln!("     {}", auth_response.user_code);
    eprintln!();

    // Try to open browser automatically
    if open_browser(display_url).is_err() {
        eprintln!("  (Could not open browser automatically)");
        eprintln!();
    }

    eprintln!("Waiting for authorization...");

    // Poll for token
    match client.poll_for_token(
        &auth_response.device_code,
        auth_response.interval,
        auth_response.expires_in,
    ) {
        Ok(creds) => {
            // Store credentials
            if let Err(e) = store.store(&creds) {
                eprintln!("\nWarning: Failed to store credentials: {}", e);
                eprintln!("You may need to log in again next time.");
            }

            eprintln!("\nSuccessfully logged in!");

            // Check if there's queued metrics data to sync
            if let Ok(db) = MetricsDatabase::global()
                && let Ok(db_lock) = db.lock()
                    && let Ok(count) = db_lock.count()
                        && count > 0 {
                            // Spawn background metrics flush now that we're logged in
                            spawn_background_metrics_db_flush();
                            // Inform the user
                            eprintln!("Syncing your Git AI dashboard in the background...");
                        }
        }
        Err(e) => {
            eprintln!("\nAuthorization failed: {}", e);
            std::process::exit(1);
        }
    }
}

/// Attempt to open a URL in the system's default browser
fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
