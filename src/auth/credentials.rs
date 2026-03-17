#[cfg(all(not(test), feature = "keyring"))]
use crate::auth::credential_backend::KeyringBackend;
use crate::auth::credential_backend::{CredentialBackend, FileBackend};
use crate::auth::types::StoredCredentials;
#[cfg(not(test))]
use crate::config::Config;
use std::path::PathBuf;

#[cfg(all(not(test), feature = "keyring"))]
const SERVICE_NAME: &str = "git-ai";
#[cfg(all(not(test), feature = "keyring"))]
const USERNAME: &str = "oauth-tokens";

/// Cross-platform credential storage
/// Uses system keyring when available, falls back to file storage
pub struct CredentialStore {
    backend: Box<dyn CredentialBackend>,
}

impl CredentialStore {
    /// Create a new credential store, testing keyring availability
    pub fn new() -> Self {
        // In test builds, always use file-based storage to avoid keyring blocking issues
        #[cfg(test)]
        {
            let path = Self::default_test_path();
            Self {
                backend: Box::new(FileBackend::new(path)),
            }
        }

        // Production build with keyring feature enabled
        #[cfg(all(not(test), feature = "keyring"))]
        {
            let use_keyring = Config::get().get_feature_flags().auth_keyring;

            if use_keyring && KeyringBackend::is_available(SERVICE_NAME) {
                Self {
                    backend: Box::new(KeyringBackend::new(SERVICE_NAME, USERNAME)),
                }
            } else {
                if use_keyring {
                    // User wanted keyring but it's not available
                    eprintln!(
                        "Note: System keyring not available, credentials will be stored in file"
                    );
                }
                Self {
                    backend: Box::new(FileBackend::new(Self::default_production_path())),
                }
            }
        }

        // Production build without keyring feature
        #[cfg(all(not(test), not(feature = "keyring")))]
        {
            let use_keyring = Config::get().get_feature_flags().auth_keyring;

            if use_keyring {
                // User wanted keyring but binary was built without keyring support
                use std::io::IsTerminal;
                if std::io::stderr().is_terminal() {
                    eprintln!(
                        "Note: auth_keyring is enabled but this binary was built without keyring support. Using file-based storage."
                    );
                }
            }
            Self {
                backend: Box::new(FileBackend::new(Self::default_production_path())),
            }
        }
    }

    /// Create a credential store with a custom backend (for testing)
    #[cfg(test)]
    pub fn with_backend(backend: Box<dyn CredentialBackend>) -> Self {
        Self { backend }
    }

    #[cfg(not(test))]
    fn default_production_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".git-ai")
            .join("internal")
            .join("credentials")
    }

    #[cfg(test)]
    fn default_test_path() -> PathBuf {
        // Use thread ID for unique path per thread (sanitized for filesystem)
        let thread_id = format!("{:?}", std::thread::current().id());
        // Extract just digits from "ThreadId(N)" format
        let thread_num: String = thread_id.chars().filter(|c| c.is_ascii_digit()).collect();
        std::env::temp_dir().join("git-ai-test").join(format!(
            "credentials-{}-{}",
            std::process::id(),
            thread_num
        ))
    }

    /// Store credentials securely
    pub fn store(&self, creds: &StoredCredentials) -> Result<(), String> {
        let json = serde_json::to_string(creds)
            .map_err(|e| format!("Failed to serialize credentials: {}", e))?;

        self.backend.store(&json)
    }

    /// Load stored credentials
    pub fn load(&self) -> Result<Option<StoredCredentials>, String> {
        let json = self.backend.load()?;

        match json {
            Some(json) => {
                let creds: StoredCredentials = serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse credentials: {}", e))?;
                Ok(Some(creds))
            }
            None => Ok(None),
        }
    }

    /// Clear stored credentials
    pub fn clear(&self) -> Result<(), String> {
        self.backend.clear()
    }

    /// Check if credentials are stored
    #[allow(dead_code)]
    pub fn has_credentials(&self) -> bool {
        self.load().map(|c| c.is_some()).unwrap_or(false)
    }

    /// Get the backend name (for logging/debugging)
    #[allow(dead_code)]
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_backend::MockBackend;
    use std::env;
    use std::fs;

    fn make_test_credentials() -> StoredCredentials {
        StoredCredentials {
            access_token: "test_access_token_12345".to_string(),
            refresh_token: "test_refresh_token_67890".to_string(),
            access_token_expires_at: chrono::Utc::now().timestamp() + 3600,
            refresh_token_expires_at: chrono::Utc::now().timestamp() + 86400 * 90,
        }
    }

    // ============= Mock Backend Tests =============
    // These test the CredentialStore logic using MockBackend

    #[test]
    fn test_store_load_clear_with_mock() {
        let store = CredentialStore::with_backend(Box::new(MockBackend::new()));
        let creds = make_test_credentials();

        // Initially empty
        assert!(!store.has_credentials());
        assert!(store.load().unwrap().is_none());

        // Store
        store.store(&creds).unwrap();
        assert!(store.has_credentials());

        // Load and verify
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(loaded.refresh_token, creds.refresh_token);
        assert_eq!(
            loaded.access_token_expires_at,
            creds.access_token_expires_at
        );
        assert_eq!(
            loaded.refresh_token_expires_at,
            creds.refresh_token_expires_at
        );

        // Clear
        store.clear().unwrap();
        assert!(!store.has_credentials());
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn test_overwrite_credentials_with_mock() {
        let store = CredentialStore::with_backend(Box::new(MockBackend::new()));

        let creds1 = make_test_credentials();
        store.store(&creds1).unwrap();

        let mut creds2 = make_test_credentials();
        creds2.access_token = "new_access_token".to_string();
        store.store(&creds2).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.access_token, "new_access_token");
    }

    #[test]
    fn test_load_nonexistent_with_mock() {
        let store = CredentialStore::with_backend(Box::new(MockBackend::new()));

        // Load when nothing stored should return None
        let result = store.load().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_has_credentials_with_mock() {
        let store = CredentialStore::with_backend(Box::new(MockBackend::new()));

        assert!(!store.has_credentials());

        store.store(&make_test_credentials()).unwrap();
        assert!(store.has_credentials());

        store.clear().unwrap();
        assert!(!store.has_credentials());
    }

    // ============= Error Handling Tests with Mock =============

    #[test]
    fn test_store_error_handling() {
        let mock = MockBackend::new().fail_store("Keyring locked by another process");
        let store = CredentialStore::with_backend(Box::new(mock));

        let result = store.store(&make_test_credentials());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Keyring locked"));
    }

    #[test]
    fn test_load_error_handling() {
        let mock = MockBackend::new().fail_load("Access denied");
        let store = CredentialStore::with_backend(Box::new(mock));

        let result = store.load();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Access denied"));
    }

    #[test]
    fn test_clear_error_handling() {
        let mock = MockBackend::new().fail_clear("Permission denied");
        let store = CredentialStore::with_backend(Box::new(mock));

        let result = store.clear();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Permission denied"));
    }

    #[test]
    fn test_has_credentials_returns_false_on_load_error() {
        let mock = MockBackend::new().fail_load("Backend unavailable");
        let store = CredentialStore::with_backend(Box::new(mock));

        // has_credentials should return false when load errors
        assert!(!store.has_credentials());
    }

    // ============= Serialization Tests =============

    #[test]
    fn test_roundtrip_store_load_credentials() {
        let creds = make_test_credentials();

        // Serialize
        let json = serde_json::to_string(&creds).unwrap();

        // Deserialize
        let loaded: StoredCredentials = serde_json::from_str(&json).unwrap();

        // Verify fields match
        assert_eq!(creds.access_token, loaded.access_token);
        assert_eq!(creds.refresh_token, loaded.refresh_token);
        assert_eq!(
            creds.access_token_expires_at,
            loaded.access_token_expires_at
        );
        assert_eq!(
            creds.refresh_token_expires_at,
            loaded.refresh_token_expires_at
        );
    }

    #[test]
    fn test_empty_credentials_file_fails_parse() {
        let result: Result<StoredCredentials, _> = serde_json::from_str("");
        assert!(result.is_err());
    }

    // ============= File Backend Integration Tests =============
    // These test the FileBackend through CredentialStore

    /// Get the fallback path for the current test store
    fn test_fallback_path() -> PathBuf {
        CredentialStore::default_test_path()
    }

    /// Helper struct that cleans up test credential files when dropped
    struct TestCredentialCleanup;

    impl Drop for TestCredentialCleanup {
        fn drop(&mut self) {
            let path = test_fallback_path();
            let _ = fs::remove_file(&path);
        }
    }

    #[test]
    fn test_file_backend_store_load_clear() {
        let _cleanup = TestCredentialCleanup;
        let store = CredentialStore::new(); // Uses FileBackend in test mode
        let creds = make_test_credentials();

        // Clean up before test
        let _ = store.clear();

        // Store
        store.store(&creds).unwrap();

        // Load and verify
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.access_token, creds.access_token);

        // Clear
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn test_file_backend_creates_directory() {
        let temp_dir = env::temp_dir().join("git-ai-test-dir-create");
        let test_path = temp_dir.join("credentials");

        // Clean up before test
        let _ = fs::remove_file(&test_path);
        let _ = fs::remove_dir(&temp_dir);

        // Verify directory doesn't exist
        assert!(!temp_dir.exists());

        let store = CredentialStore::with_backend(Box::new(FileBackend::new(test_path.clone())));
        store.store(&make_test_credentials()).unwrap();

        // Verify directory was created
        assert!(temp_dir.exists());
        assert!(test_path.exists());

        // Clean up
        let _ = fs::remove_file(&test_path);
        let _ = fs::remove_dir(&temp_dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_file_backend_sets_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_path = env::temp_dir()
            .join("git-ai-test-perms")
            .join("credentials");

        let store = CredentialStore::with_backend(Box::new(FileBackend::new(temp_path.clone())));

        // Clean up before test
        let _ = store.clear();

        store.store(&make_test_credentials()).unwrap();

        // Verify permissions
        let perms = fs::metadata(&temp_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);

        // Clean up
        let _ = fs::remove_file(&temp_path);
        if let Some(parent) = temp_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    // ============= Corrupted Data Tests =============

    #[test]
    fn test_corrupted_credentials_truncated_json() {
        let mock = MockBackend::new();
        // Manually store invalid JSON
        mock.store(r#"{"access_token": "test"#).unwrap();

        let store = CredentialStore::with_backend(Box::new(mock));
        let result = store.load();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse"));
    }

    #[test]
    fn test_corrupted_credentials_wrong_schema() {
        let mock = MockBackend::new();
        mock.store(r#"{"username": "test", "password": "secret"}"#)
            .unwrap();

        let store = CredentialStore::with_backend(Box::new(mock));
        let result = store.load();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse"));
    }

    #[test]
    fn test_corrupted_credentials_empty_json_object() {
        let mock = MockBackend::new();
        mock.store("{}").unwrap();

        let store = CredentialStore::with_backend(Box::new(mock));
        let result = store.load();

        assert!(result.is_err());
    }

    #[test]
    fn test_corrupted_credentials_null_json() {
        let mock = MockBackend::new();
        mock.store("null").unwrap();

        let store = CredentialStore::with_backend(Box::new(mock));
        let result = store.load();

        assert!(result.is_err());
    }

    #[test]
    fn test_corrupted_credentials_json_array() {
        let mock = MockBackend::new();
        mock.store(r#"["access_token", "refresh_token"]"#).unwrap();

        let store = CredentialStore::with_backend(Box::new(mock));
        let result = store.load();

        assert!(result.is_err());
    }

    // ============= Path Tests =============

    #[test]
    fn test_fallback_path_is_deterministic() {
        let path1 = test_fallback_path();
        let path2 = test_fallback_path();
        assert_eq!(path1, path2);
    }

    #[test]
    fn test_fallback_path_contains_credentials() {
        let path = test_fallback_path();
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert!(file_name.starts_with("credentials"));
    }

    #[test]
    fn test_fallback_path_contains_git_ai_test_dir() {
        let path = test_fallback_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("git-ai-test"));
    }

    // ============= Backend Name Test =============

    #[test]
    fn test_backend_name() {
        let mock_store = CredentialStore::with_backend(Box::new(MockBackend::new()));
        assert_eq!(mock_store.backend_name(), "mock");

        let file_store = CredentialStore::new();
        assert_eq!(file_store.backend_name(), "file");
    }
}
