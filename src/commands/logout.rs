use crate::auth::CredentialStore;

/// Handle the `git-ai logout` command
pub fn handle_logout(_args: &[String]) {
    let store = CredentialStore::new();

    // Check if currently logged in
    match store.load() {
        Ok(Some(_)) => {
            // Clear credentials
            if let Err(e) = store.clear() {
                eprintln!("Failed to clear credentials: {}", e);
                std::process::exit(1);
            }
            eprintln!("Successfully logged out.");
        }
        Ok(None) => {
            eprintln!("Not currently logged in.");
        }
        Err(e) => {
            eprintln!("Error checking credentials: {}", e);
            std::process::exit(1);
        }
    }
}
