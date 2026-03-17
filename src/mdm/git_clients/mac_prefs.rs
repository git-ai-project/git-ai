//! macOS preferences utilities for git client installers.
//!
//! Provides a clean API for reading and writing macOS preferences
//! via the `defaults` command.

use crate::error::GitAiError;
use std::path::PathBuf;
use std::process::Command;

/// Find an application by its bundle identifier.
///
/// Returns the path to the app bundle if found.
pub fn find_app_by_bundle_id(bundle_id: &str) -> Option<PathBuf> {
    let query = format!("kMDItemCFBundleIdentifier == '{}'", bundle_id);
    let output = Command::new("mdfind").arg(&query).output().ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    None
}

/// Check if a preferences domain exists (i.e., has any preferences set).
#[allow(dead_code)]
pub fn domain_exists(domain: &str) -> bool {
    Command::new("defaults")
        .args(["read", domain])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A handle for reading and writing preferences for a specific domain.
///
/// # Example
/// ```ignore
/// let prefs = Preferences::new("com.DanPristupov.Fork");
/// if prefs.read_int("gitInstanceType") != Some(2) {
///     prefs.write_int("gitInstanceType", 2)?;
/// }
/// ```
pub struct Preferences {
    domain: String,
}

impl Preferences {
    /// Create a new preferences handle for the given domain.
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
        }
    }

    /// Check if this preferences domain exists.
    #[allow(dead_code)]
    pub fn exists(&self) -> bool {
        domain_exists(&self.domain)
    }

    /// Read a string preference.
    pub fn read_string(&self, key: &str) -> Option<String> {
        let output = Command::new("defaults")
            .args(["read", &self.domain, key])
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    }

    /// Read an integer preference.
    pub fn read_int(&self, key: &str) -> Option<i32> {
        self.read_string(key)?.parse().ok()
    }

    /// Read a boolean preference.
    #[allow(dead_code)]
    pub fn read_bool(&self, key: &str) -> Option<bool> {
        let value = self.read_string(key)?;
        match value.as_str() {
            "1" | "true" | "YES" => Some(true),
            "0" | "false" | "NO" => Some(false),
            _ => None,
        }
    }

    /// Write a string preference.
    pub fn write_string(&self, key: &str, value: &str) -> Result<(), GitAiError> {
        let status = Command::new("defaults")
            .args(["write", &self.domain, key, "-string", value])
            .status()
            .map_err(|e| GitAiError::Generic(format!("Failed to write preference: {}", e)))?;

        if status.success() {
            Ok(())
        } else {
            Err(GitAiError::Generic(format!(
                "defaults write failed for {}.{}",
                self.domain, key
            )))
        }
    }

    /// Write an integer preference.
    pub fn write_int(&self, key: &str, value: i32) -> Result<(), GitAiError> {
        let status = Command::new("defaults")
            .args(["write", &self.domain, key, "-int", &value.to_string()])
            .status()
            .map_err(|e| GitAiError::Generic(format!("Failed to write preference: {}", e)))?;

        if status.success() {
            Ok(())
        } else {
            Err(GitAiError::Generic(format!(
                "defaults write failed for {}.{}",
                self.domain, key
            )))
        }
    }

    /// Write a boolean preference.
    #[allow(dead_code)]
    pub fn write_bool(&self, key: &str, value: bool) -> Result<(), GitAiError> {
        let status = Command::new("defaults")
            .args([
                "write",
                &self.domain,
                key,
                "-bool",
                if value { "YES" } else { "NO" },
            ])
            .status()
            .map_err(|e| GitAiError::Generic(format!("Failed to write preference: {}", e)))?;

        if status.success() {
            Ok(())
        } else {
            Err(GitAiError::Generic(format!(
                "defaults write failed for {}.{}",
                self.domain, key
            )))
        }
    }

    /// Delete a preference key.
    pub fn delete(&self, key: &str) -> Result<(), GitAiError> {
        let status = Command::new("defaults")
            .args(["delete", &self.domain, key])
            .status()
            .map_err(|e| GitAiError::Generic(format!("Failed to delete preference: {}", e)))?;

        // Success if deleted OR if key didn't exist
        if status.success() || self.read_string(key).is_none() {
            Ok(())
        } else {
            Err(GitAiError::Generic(format!(
                "defaults delete failed for {}.{}",
                self.domain, key
            )))
        }
    }

    /// Get the domain name.
    #[allow(dead_code)]
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preferences_new() {
        let prefs = Preferences::new("com.example.test");
        assert_eq!(prefs.domain(), "com.example.test");
    }
}
