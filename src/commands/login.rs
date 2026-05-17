use git_ai::auth::{CredentialStore, OAuthClient};

pub fn handle_login(_args: &[String]) {
    let store = CredentialStore::new();

    if let Ok(Some(creds)) = store.load()
        && !creds.is_refresh_token_expired()
    {
        eprintln!("Already logged in. Use 'git-ai logout' to log out first.");
        std::process::exit(0);
    }

    let client = OAuthClient::new();

    eprintln!("Starting device authorization...\n");

    let auth_response = match client.start_device_flow() {
        Ok(response) => response,
        Err(e) => {
            eprintln!("Failed to start authorization: {}", e);
            std::process::exit(1);
        }
    };

    let display_url = auth_response
        .verification_uri_complete
        .as_ref()
        .unwrap_or(&auth_response.verification_uri);

    eprintln!("To authorize this device:");
    eprintln!("  1. Open this URL in your browser:");
    eprintln!("     {}", display_url);
    eprintln!();
    eprintln!("  2. Enter this code when prompted:");
    eprintln!("     {}", auth_response.user_code);
    eprintln!();

    if open_browser(display_url).is_err() {
        eprintln!("  (Could not open browser automatically)");
        eprintln!();
    }

    eprintln!("Waiting for authorization...");

    match client.poll_for_token(
        &auth_response.device_code,
        auth_response.interval,
        auth_response.expires_in,
    ) {
        Ok(creds) => {
            if let Err(e) = store.store(&creds) {
                eprintln!("\nWarning: Failed to store credentials: {}", e);
                eprintln!("You may need to log in again next time.");
            }
            eprintln!("\nSuccessfully logged in!");
        }
        Err(e) => {
            eprintln!("\nAuthorization failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(url);
        cmd
    };

    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(url);
        cmd
    };

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start", "", url]);
        cmd
    };

    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}
