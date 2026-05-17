use git_ai::auth::CredentialStore;

pub fn handle_logout(_args: &[String]) {
    let store = CredentialStore::new();

    match store.load() {
        Ok(Some(_)) => {
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
