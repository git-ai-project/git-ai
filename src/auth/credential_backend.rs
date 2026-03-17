use std::fs;
use std::path::PathBuf;

/// Trait for credential storage backends
pub trait CredentialBackend: Send + Sync {
    /// Store a value
    fn store(&self, value: &str) -> Result<(), String>;

    /// Load a stored value, returns None if not found
    fn load(&self) -> Result<Option<String>, String>;

    /// Clear stored value
    fn clear(&self) -> Result<(), String>;

    /// Backend name for logging/debugging
    fn name(&self) -> &'static str;
}

/// Keyring-based credential storage using system keychain
#[cfg(all(not(test), feature = "keyring"))]
pub struct KeyringBackend {
    service_name: String,
    username: String,
}

#[cfg(all(not(test), feature = "keyring"))]
impl KeyringBackend {
    pub fn new(service_name: &str, username: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
            username: username.to_string(),
        }
    }

    /// Test if keyring is available by attempting a store/delete cycle
    pub fn is_available(service_name: &str) -> bool {
        let test_entry = match keyring::Entry::new(service_name, "test-availability") {
            Ok(entry) => entry,
            Err(_) => return false,
        };

        if test_entry.set_password("test").is_err() {
            return false;
        }

        let _ = test_entry.delete_credential();
        true
    }
}

#[cfg(all(not(test), feature = "keyring"))]
impl CredentialBackend for KeyringBackend {
    fn store(&self, value: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(&self.service_name, &self.username)
            .map_err(|e| format!("Keyring error: {}", e))?;
        entry
            .set_password(value)
            .map_err(|e| format!("Failed to store in keyring: {}", e))
    }

    fn load(&self) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(&self.service_name, &self.username)
            .map_err(|e| format!("Keyring error: {}", e))?;

        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("Failed to read from keyring: {}", e)),
        }
    }

    fn clear(&self) -> Result<(), String> {
        let entry = keyring::Entry::new(&self.service_name, &self.username)
            .map_err(|e| format!("Keyring error: {}", e))?;

        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()), // Already cleared
            Err(e) => Err(format!("Failed to clear keyring: {}", e)),
        }
    }

    fn name(&self) -> &'static str {
        "keyring"
    }
}

/// File-based credential storage as fallback
pub struct FileBackend {
    path: PathBuf,
}

impl FileBackend {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(windows)]
    fn set_file_protection(path: &std::path::Path) -> Result<(), String> {
        use std::process::Command;

        let path_str = path
            .to_str()
            .ok_or_else(|| "Invalid path encoding".to_string())?;

        let username = std::env::var("USERNAME")
            .map_err(|_| "Could not determine current user".to_string())?;

        let output = Command::new("icacls")
            .args([
                path_str,
                "/inheritance:r",
                "/grant:r",
                &format!("{}:F", username),
            ])
            .output()
            .map_err(|e| format!("Failed to run icacls: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "Warning: Could not set restrictive permissions on credentials file: {}",
                stderr
            );
        }

        let _ = Command::new("attrib").args(["+H", path_str]).output();

        Ok(())
    }
}

impl CredentialBackend for FileBackend {
    fn store(&self, value: &str) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        fs::write(&self.path, value)
            .map_err(|e| format!("Failed to write credentials file: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Failed to set file permissions: {}", e))?;
        }

        #[cfg(windows)]
        {
            Self::set_file_protection(&self.path)?;
        }

        Ok(())
    }

    fn load(&self) -> Result<Option<String>, String> {
        match fs::read_to_string(&self.path) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to read credentials file: {}", e)),
        }
    }

    fn clear(&self) -> Result<(), String> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to remove credentials file: {}", e)),
        }
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

// ============= Test Mock Backend =============

#[cfg(test)]
pub use mock::*;

#[cfg(test)]
mod mock {
    use super::CredentialBackend;
    use std::cell::RefCell;

    /// Configurable failure modes for MockBackend
    #[derive(Clone, Debug)]
    pub enum MockFailure {
        Store(String),
        Load(String),
        Clear(String),
    }

    /// Mock backend for testing - stores in memory, can simulate failures
    pub struct MockBackend {
        storage: RefCell<Option<String>>,
        failure: RefCell<Option<MockFailure>>,
    }

    impl MockBackend {
        pub fn new() -> Self {
            Self {
                storage: RefCell::new(None),
                failure: RefCell::new(None),
            }
        }

        /// Configure to fail on store with given error message
        pub fn fail_store(self, msg: &str) -> Self {
            *self.failure.borrow_mut() = Some(MockFailure::Store(msg.to_string()));
            self
        }

        /// Configure to fail on load with given error message
        pub fn fail_load(self, msg: &str) -> Self {
            *self.failure.borrow_mut() = Some(MockFailure::Load(msg.to_string()));
            self
        }

        /// Configure to fail on clear with given error message
        pub fn fail_clear(self, msg: &str) -> Self {
            *self.failure.borrow_mut() = Some(MockFailure::Clear(msg.to_string()));
            self
        }

        /// Check if storage contains a value
        pub fn has_value(&self) -> bool {
            self.storage.borrow().is_some()
        }

        /// Get the stored value (for test assertions)
        pub fn get_value(&self) -> Option<String> {
            self.storage.borrow().clone()
        }
    }

    impl Default for MockBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    // MockBackend is not Send+Sync due to RefCell, but that's fine for single-threaded tests
    // We implement the trait without the Send+Sync bounds for the mock
    unsafe impl Send for MockBackend {}
    unsafe impl Sync for MockBackend {}

    impl CredentialBackend for MockBackend {
        fn store(&self, value: &str) -> Result<(), String> {
            if let Some(MockFailure::Store(msg)) = self.failure.borrow().as_ref() {
                return Err(msg.clone());
            }
            *self.storage.borrow_mut() = Some(value.to_string());
            Ok(())
        }

        fn load(&self) -> Result<Option<String>, String> {
            if let Some(MockFailure::Load(msg)) = self.failure.borrow().as_ref() {
                return Err(msg.clone());
            }
            Ok(self.storage.borrow().clone())
        }

        fn clear(&self) -> Result<(), String> {
            if let Some(MockFailure::Clear(msg)) = self.failure.borrow().as_ref() {
                return Err(msg.clone());
            }
            *self.storage.borrow_mut() = None;
            Ok(())
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_backend_store_load_clear() {
        let backend = MockBackend::new();

        // Initially empty
        assert!(!backend.has_value());
        assert_eq!(backend.load().unwrap(), None);

        // Store
        backend.store("test-value").unwrap();
        assert!(backend.has_value());
        assert_eq!(backend.load().unwrap(), Some("test-value".to_string()));

        // Clear
        backend.clear().unwrap();
        assert!(!backend.has_value());
        assert_eq!(backend.load().unwrap(), None);
    }

    #[test]
    fn test_mock_backend_fail_store() {
        let backend = MockBackend::new().fail_store("Keyring locked");

        let result = backend.store("value");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Keyring locked");
    }

    #[test]
    fn test_mock_backend_fail_load() {
        let backend = MockBackend::new().fail_load("Access denied");

        let result = backend.load();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Access denied");
    }

    #[test]
    fn test_mock_backend_fail_clear() {
        let backend = MockBackend::new().fail_clear("Permission denied");

        let result = backend.clear();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Permission denied");
    }

    #[test]
    fn test_mock_backend_overwrite() {
        let backend = MockBackend::new();

        backend.store("first").unwrap();
        assert_eq!(backend.get_value(), Some("first".to_string()));

        backend.store("second").unwrap();
        assert_eq!(backend.get_value(), Some("second".to_string()));
    }

    #[test]
    fn test_file_backend_roundtrip() {
        let temp_path = std::env::temp_dir()
            .join("git-ai-test-backend")
            .join(format!("creds-{}", std::process::id()));

        let backend = FileBackend::new(temp_path.clone());

        // Clean up before test
        let _ = backend.clear();

        // Initially empty
        assert_eq!(backend.load().unwrap(), None);

        // Store
        backend.store("test-credentials").unwrap();
        assert_eq!(
            backend.load().unwrap(),
            Some("test-credentials".to_string())
        );

        // Clear
        backend.clear().unwrap();
        assert_eq!(backend.load().unwrap(), None);

        // Clean up
        if let Some(parent) = temp_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn test_file_backend_clear_nonexistent() {
        let temp_path = std::env::temp_dir()
            .join("git-ai-test-backend-nonexistent")
            .join("creds");

        let backend = FileBackend::new(temp_path);

        // Should succeed even if file doesn't exist
        assert!(backend.clear().is_ok());
    }
}
