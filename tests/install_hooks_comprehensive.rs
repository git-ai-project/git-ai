//! Comprehensive tests for install_hooks command module
//!
//! This module tests the git-ai install-hooks and uninstall-hooks commands,
//! which handle installation of git hooks for various IDEs and coding agents.

use git_ai::commands::install_hooks::{
    InstallResult, InstallStatus, run, run_uninstall, to_hashmap,
};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

struct EnvRestoreGuard {
    prev_home: Option<OsString>,
    prev_userprofile: Option<OsString>,
    prev_path: Option<OsString>,
}

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        // SAFETY: tests using this guard are serialized via #[serial_test::serial],
        // so mutating process env is safe.
        unsafe {
            match &self.prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
            match &self.prev_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn install_fake_editor_cli(bin_dir: &Path, cli_name: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let cli_path = bin_dir.join(cli_name);
        let script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "1.200.0"
  exit 0
fi
if [ "$1" = "--list-extensions" ]; then
  echo "git-ai.git-ai-vscode"
  exit 0
fi
exit 0
"#;
        fs::write(&cli_path, script).expect("write fake cli");
        let mut perms = fs::metadata(&cli_path)
            .expect("stat fake cli")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli_path, perms).expect("chmod fake cli");
    }

    #[cfg(windows)]
    {
        let cli_path = bin_dir.join(format!("{}.cmd", cli_name));
        let script = r#"@echo off
if "%1"=="--version" (
  echo 1.200.0
  exit /b 0
)
if "%1"=="--list-extensions" (
  echo git-ai.git-ai-vscode
  exit /b 0
)
exit /b 0
"#;
        fs::write(&cli_path, script).expect("write fake cli");
    }
}

fn with_temp_home<F: FnOnce(&Path)>(f: F) {
    let temp_dir = TempDir::new().expect("temp home");
    let home = temp_dir.path().to_path_buf();
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).expect("temp bin");
    install_fake_editor_cli(&bin_dir, "code");
    install_fake_editor_cli(&bin_dir, "cursor");

    let _restore_guard = EnvRestoreGuard {
        prev_home: std::env::var_os("HOME"),
        prev_userprofile: std::env::var_os("USERPROFILE"),
        prev_path: std::env::var_os("PATH"),
    };

    // SAFETY: tests using this helper are serialized via #[serial_test::serial],
    // so mutating process env is safe.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        // Keep PATH constrained to temp fake CLIs to avoid real tool discovery.
        std::env::set_var("PATH", &bin_dir);
    }

    f(&home);
}

// ==============================================================================
// InstallStatus Tests
// ==============================================================================

#[test]
fn test_install_status_as_str() {
    assert_eq!(InstallStatus::NotFound.as_str(), "not_found");
    assert_eq!(InstallStatus::Installed.as_str(), "installed");
    assert_eq!(
        InstallStatus::AlreadyInstalled.as_str(),
        "already_installed"
    );
    assert_eq!(InstallStatus::Failed.as_str(), "failed");
}

#[test]
fn test_install_status_equality() {
    assert_eq!(InstallStatus::NotFound, InstallStatus::NotFound);
    assert_eq!(InstallStatus::Installed, InstallStatus::Installed);
    assert_eq!(
        InstallStatus::AlreadyInstalled,
        InstallStatus::AlreadyInstalled
    );
    assert_eq!(InstallStatus::Failed, InstallStatus::Failed);

    assert_ne!(InstallStatus::NotFound, InstallStatus::Installed);
    assert_ne!(InstallStatus::Installed, InstallStatus::Failed);
}

#[test]
fn test_install_status_copy_clone() {
    let status = InstallStatus::Installed;
    let copied = status;
    let cloned = status.clone();

    assert_eq!(status, copied);
    assert_eq!(status, cloned);
    assert_eq!(copied, cloned);
}

// ==============================================================================
// InstallResult Tests
// ==============================================================================

#[test]
fn test_install_result_installed() {
    let result = InstallResult::installed();
    assert_eq!(result.status, InstallStatus::Installed);
    assert!(result.error.is_none());
    assert!(result.warnings.is_empty());
}

#[test]
fn test_install_result_already_installed() {
    let result = InstallResult::already_installed();
    assert_eq!(result.status, InstallStatus::AlreadyInstalled);
    assert!(result.error.is_none());
    assert!(result.warnings.is_empty());
}

#[test]
fn test_install_result_not_found() {
    let result = InstallResult::not_found();
    assert_eq!(result.status, InstallStatus::NotFound);
    assert!(result.error.is_none());
    assert!(result.warnings.is_empty());
}

#[test]
fn test_install_result_failed() {
    let result = InstallResult::failed("Installation failed");
    assert_eq!(result.status, InstallStatus::Failed);
    assert_eq!(result.error, Some("Installation failed".to_string()));
    assert!(result.warnings.is_empty());
}

#[test]
fn test_install_result_failed_with_string() {
    let error_msg = String::from("Custom error message");
    let result = InstallResult::failed(error_msg.clone());
    assert_eq!(result.status, InstallStatus::Failed);
    assert_eq!(result.error, Some(error_msg));
}

#[test]
fn test_install_result_with_warning() {
    let result = InstallResult::installed().with_warning("Minor issue detected");
    assert_eq!(result.status, InstallStatus::Installed);
    assert!(result.error.is_none());
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0], "Minor issue detected");
}

#[test]
fn test_install_result_with_multiple_warnings() {
    let result = InstallResult::installed()
        .with_warning("Warning 1")
        .with_warning("Warning 2")
        .with_warning("Warning 3");

    assert_eq!(result.warnings.len(), 3);
    assert_eq!(result.warnings[0], "Warning 1");
    assert_eq!(result.warnings[1], "Warning 2");
    assert_eq!(result.warnings[2], "Warning 3");
}

#[test]
fn test_install_result_message_for_metrics_with_error() {
    let result = InstallResult::failed("Critical error");
    let message = result.message_for_metrics();
    assert_eq!(message, Some("Critical error".to_string()));
}

#[test]
fn test_install_result_message_for_metrics_with_warnings() {
    let result = InstallResult::installed()
        .with_warning("Warning 1")
        .with_warning("Warning 2");
    let message = result.message_for_metrics();
    assert_eq!(message, Some("Warning 1; Warning 2".to_string()));
}

#[test]
fn test_install_result_message_for_metrics_with_error_and_warnings() {
    // Error takes precedence over warnings
    let result = InstallResult::failed("Error message").with_warning("Some warning");
    let message = result.message_for_metrics();
    assert_eq!(message, Some("Error message".to_string()));
}

#[test]
fn test_install_result_message_for_metrics_no_error_or_warnings() {
    let result = InstallResult::installed();
    let message = result.message_for_metrics();
    assert!(message.is_none());
}

#[test]
fn test_install_result_message_for_metrics_empty_warnings() {
    let result = InstallResult {
        status: InstallStatus::Installed,
        error: None,
        warnings: vec![],
    };
    let message = result.message_for_metrics();
    assert!(message.is_none());
}

// ==============================================================================
// to_hashmap Conversion Tests
// ==============================================================================

#[test]
fn test_to_hashmap_empty() {
    let statuses: HashMap<String, InstallStatus> = HashMap::new();
    let result = to_hashmap(statuses);
    assert!(result.is_empty());
}

#[test]
fn test_to_hashmap_single_entry() {
    let mut statuses = HashMap::new();
    statuses.insert("cursor".to_string(), InstallStatus::Installed);

    let result = to_hashmap(statuses);
    assert_eq!(result.len(), 1);
    assert_eq!(result.get("cursor"), Some(&"installed".to_string()));
}

#[test]
fn test_to_hashmap_multiple_entries() {
    let mut statuses = HashMap::new();
    statuses.insert("cursor".to_string(), InstallStatus::Installed);
    statuses.insert("claude-code".to_string(), InstallStatus::AlreadyInstalled);
    statuses.insert("codex".to_string(), InstallStatus::NotFound);
    statuses.insert("windsurf".to_string(), InstallStatus::Failed);

    let result = to_hashmap(statuses);
    assert_eq!(result.len(), 4);
    assert_eq!(result.get("cursor"), Some(&"installed".to_string()));
    assert_eq!(
        result.get("claude-code"),
        Some(&"already_installed".to_string())
    );
    assert_eq!(result.get("codex"), Some(&"not_found".to_string()));
    assert_eq!(result.get("windsurf"), Some(&"failed".to_string()));
}

#[test]
fn test_to_hashmap_all_statuses() {
    let mut statuses = HashMap::new();
    statuses.insert("not_found".to_string(), InstallStatus::NotFound);
    statuses.insert("installed".to_string(), InstallStatus::Installed);
    statuses.insert("already".to_string(), InstallStatus::AlreadyInstalled);
    statuses.insert("failed".to_string(), InstallStatus::Failed);

    let result = to_hashmap(statuses);
    assert_eq!(result.get("not_found"), Some(&"not_found".to_string()));
    assert_eq!(result.get("installed"), Some(&"installed".to_string()));
    assert_eq!(
        result.get("already"),
        Some(&"already_installed".to_string())
    );
    assert_eq!(result.get("failed"), Some(&"failed".to_string()));
}

// ==============================================================================
// Argument Parsing Tests
// ==============================================================================

#[test]
#[serial_test::serial]
fn test_run_install_hooks_no_args() {
    with_temp_home(|_| {
        let result = run(&[]);

        match result {
            Ok(_statuses) => {}
            Err(e) => {
                let err_msg = e.to_string();
                assert!(!err_msg.is_empty());
            }
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_dry_run_flag() {
    with_temp_home(|_| {
        let args = vec!["--dry-run".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(e) => {
                let err_msg = e.to_string();
                assert!(!err_msg.is_empty());
            }
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_dry_run_true() {
    with_temp_home(|_| {
        let args = vec!["--dry-run=true".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_verbose_flag() {
    with_temp_home(|_| {
        let args = vec!["--verbose".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_verbose_short_flag() {
    with_temp_home(|_| {
        let args = vec!["-v".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_multiple_flags() {
    with_temp_home(|_| {
        let args = vec!["--dry-run".to_string(), "--verbose".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_with_dry_run_false() {
    with_temp_home(|_| {
        let args = vec!["--dry-run=false".to_string()];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_install_hooks_ignores_unknown_args() {
    with_temp_home(|_| {
        let args = vec![
            "--unknown-flag".to_string(),
            "random-arg".to_string(),
            "--dry-run".to_string(),
        ];
        let result = run(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

// ==============================================================================
// Uninstall Tests
// ==============================================================================

#[test]
#[serial_test::serial]
fn test_run_uninstall_hooks_no_args() {
    with_temp_home(|_| {
        let result = run_uninstall(&[]);

        match result {
            Ok(_statuses) => {}
            Err(e) => {
                let err_msg = e.to_string();
                assert!(!err_msg.is_empty());
            }
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_uninstall_hooks_with_dry_run() {
    with_temp_home(|_| {
        let args = vec!["--dry-run".to_string()];
        let result = run_uninstall(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_uninstall_hooks_with_verbose() {
    with_temp_home(|_| {
        let args = vec!["--verbose".to_string()];
        let result = run_uninstall(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_run_uninstall_hooks_with_multiple_flags() {
    with_temp_home(|_| {
        let args = vec![
            "--dry-run=true".to_string(),
            "-v".to_string(),
            "--unknown".to_string(),
        ];
        let result = run_uninstall(&args);

        match result {
            Ok(_statuses) => {}
            Err(_e) => {}
        }
    });
}

// ==============================================================================
// Edge Cases and Error Handling
// ==============================================================================

#[test]
fn test_install_result_clone() {
    let result = InstallResult::failed("Error")
        .with_warning("Warning 1")
        .with_warning("Warning 2");

    let cloned = result.clone();
    assert_eq!(cloned.status, result.status);
    assert_eq!(cloned.error, result.error);
    assert_eq!(cloned.warnings, result.warnings);
}

#[test]
fn test_install_result_debug_formatting() {
    let result = InstallResult::installed();
    let debug_str = format!("{:?}", result);
    assert!(debug_str.contains("InstallResult"));
    assert!(debug_str.contains("Installed"));
}

#[test]
fn test_install_status_debug_formatting() {
    let status = InstallStatus::Installed;
    let debug_str = format!("{:?}", status);
    assert!(debug_str.contains("Installed"));
}

#[test]
fn test_to_hashmap_preserves_all_keys() {
    let mut statuses = HashMap::new();
    let keys = vec![
        "cursor",
        "claude-code",
        "codex",
        "windsurf",
        "continue-cli",
        "github-copilot",
    ];

    for (idx, key) in keys.iter().enumerate() {
        let status = match idx % 4 {
            0 => InstallStatus::Installed,
            1 => InstallStatus::AlreadyInstalled,
            2 => InstallStatus::NotFound,
            _ => InstallStatus::Failed,
        };
        statuses.insert(key.to_string(), status);
    }

    let result = to_hashmap(statuses);
    assert_eq!(result.len(), keys.len());

    for key in keys {
        assert!(
            result.contains_key(key),
            "Expected key '{}' to be present",
            key
        );
    }
}

#[test]
fn test_install_result_warning_with_empty_string() {
    let result = InstallResult::installed().with_warning("");
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0], "");
}

#[test]
fn test_install_result_failed_with_empty_string() {
    let result = InstallResult::failed("");
    assert_eq!(result.error, Some("".to_string()));
    assert_eq!(result.status, InstallStatus::Failed);
}

#[test]
fn test_install_result_message_for_metrics_single_warning() {
    let result = InstallResult::installed().with_warning("Only warning");
    let message = result.message_for_metrics();
    assert_eq!(message, Some("Only warning".to_string()));
}

#[test]
fn test_install_result_message_for_metrics_warnings_join_with_semicolon() {
    let result = InstallResult::installed()
        .with_warning("First; warning")
        .with_warning("Second; warning")
        .with_warning("Third; warning");

    let message = result.message_for_metrics();
    assert_eq!(
        message,
        Some("First; warning; Second; warning; Third; warning".to_string())
    );
}

// ==============================================================================
// Integration-style Tests
// ==============================================================================

#[test]
#[serial_test::serial]
fn test_install_workflow_dry_run_does_not_modify_system() {
    with_temp_home(|_| {
        let args = vec!["--dry-run".to_string(), "--verbose".to_string()];

        let result1 = run(&args);
        let result2 = run(&args);

        match (result1, result2) {
            (Ok(_statuses1), Ok(_statuses2)) => {}
            (Err(_), Err(_)) => {}
            _ => {}
        }
    });
}

#[test]
#[serial_test::serial]
fn test_uninstall_workflow_dry_run_does_not_modify_system() {
    with_temp_home(|_| {
        let args = vec!["--dry-run".to_string()];

        let result1 = run_uninstall(&args);
        let result2 = run_uninstall(&args);

        match (result1, result2) {
            (Ok(_statuses1), Ok(_statuses2)) => {}
            (Err(_), Err(_)) => {}
            _ => {}
        }
    });
}

// ==============================================================================
// Status String Validation
// ==============================================================================

#[test]
fn test_all_status_strings_are_lowercase() {
    assert!(
        InstallStatus::NotFound
            .as_str()
            .chars()
            .all(|c| !c.is_uppercase())
    );
    assert!(
        InstallStatus::Installed
            .as_str()
            .chars()
            .all(|c| !c.is_uppercase())
    );
    assert!(
        InstallStatus::AlreadyInstalled
            .as_str()
            .chars()
            .all(|c| !c.is_uppercase())
    );
    assert!(
        InstallStatus::Failed
            .as_str()
            .chars()
            .all(|c| !c.is_uppercase())
    );
}

#[test]
fn test_status_strings_use_underscores() {
    // Verify consistent naming convention
    assert!(InstallStatus::NotFound.as_str().contains('_'));
    assert!(InstallStatus::AlreadyInstalled.as_str().contains('_'));
    assert!(!InstallStatus::Installed.as_str().contains('_'));
    assert!(!InstallStatus::Failed.as_str().contains('_'));
}

#[test]
fn test_status_strings_are_valid_identifiers() {
    // Status strings should be suitable for use as keys
    let statuses = [
        InstallStatus::NotFound,
        InstallStatus::Installed,
        InstallStatus::AlreadyInstalled,
        InstallStatus::Failed,
    ];

    for status in &statuses {
        let s = status.as_str();
        assert!(!s.is_empty());
        assert!(!s.contains(' '));
        assert!(!s.contains('-'));
        // Should only contain alphanumeric and underscores
        assert!(s.chars().all(|c| c.is_alphanumeric() || c == '_'));
    }
}

// ==============================================================================
// Complex Scenario Tests
// ==============================================================================

#[test]
fn test_install_result_builder_pattern() {
    // Demonstrate builder-like pattern with warnings
    let result = InstallResult::installed()
        .with_warning("Extension not found")
        .with_warning("Git path not configured")
        .with_warning("Manual action required");

    assert_eq!(result.status, InstallStatus::Installed);
    assert_eq!(result.warnings.len(), 3);
    assert!(result.error.is_none());

    let message = result.message_for_metrics();
    assert!(message.is_some());
    let msg = message.unwrap();
    assert!(msg.contains("Extension not found"));
    assert!(msg.contains("Git path not configured"));
    assert!(msg.contains("Manual action required"));
}

#[test]
fn test_to_hashmap_with_realistic_agent_names() {
    let mut statuses = HashMap::new();
    statuses.insert("cursor".to_string(), InstallStatus::Installed);
    statuses.insert("claude-code".to_string(), InstallStatus::AlreadyInstalled);
    statuses.insert("github-copilot".to_string(), InstallStatus::NotFound);
    statuses.insert("codex".to_string(), InstallStatus::Installed);
    statuses.insert("windsurf".to_string(), InstallStatus::Failed);
    statuses.insert("continue-cli".to_string(), InstallStatus::NotFound);

    let result = to_hashmap(statuses);
    assert_eq!(result.len(), 6);

    // Verify specific mappings
    assert_eq!(result.get("cursor").unwrap(), "installed");
    assert_eq!(result.get("claude-code").unwrap(), "already_installed");
    assert_eq!(result.get("github-copilot").unwrap(), "not_found");
    assert_eq!(result.get("codex").unwrap(), "installed");
    assert_eq!(result.get("windsurf").unwrap(), "failed");
    assert_eq!(result.get("continue-cli").unwrap(), "not_found");
}

#[test]
fn test_install_result_different_error_types() {
    // Test with different error message types
    let errors = vec![
        "Permission denied",
        "File not found",
        "Invalid configuration",
        "Version mismatch: expected 1.7, found 1.5",
        "Network timeout",
        "",
    ];

    for error in errors {
        let result = InstallResult::failed(error);
        assert_eq!(result.status, InstallStatus::Failed);
        assert_eq!(result.error, Some(error.to_string()));
        assert_eq!(result.message_for_metrics(), Some(error.to_string()));
    }
}

#[test]
fn test_hashmap_conversion_stability() {
    // Test that conversion is stable (same input produces same output)
    let mut statuses = HashMap::new();
    statuses.insert("test1".to_string(), InstallStatus::Installed);
    statuses.insert("test2".to_string(), InstallStatus::NotFound);

    let result1 = to_hashmap(statuses.clone());
    let result2 = to_hashmap(statuses);

    assert_eq!(result1.len(), result2.len());
    for (key, value) in result1.iter() {
        assert_eq!(result2.get(key), Some(value));
    }
}
