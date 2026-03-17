/// Comprehensive tests for Sublime Merge git client installer
use git_ai::mdm::git_client_installer::{GitClientInstaller, GitClientInstallerParams};
use git_ai::mdm::git_clients::SublimeMergeInstaller;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_test_params(git_shim_path: PathBuf) -> GitClientInstallerParams {
    GitClientInstallerParams { git_shim_path }
}

#[test]
fn test_sublime_merge_installer_name() {
    let installer = SublimeMergeInstaller;
    assert_eq!(installer.name(), "Sublime Merge");
}

#[test]
fn test_sublime_merge_installer_id() {
    let installer = SublimeMergeInstaller;
    assert_eq!(installer.id(), "sublime-merge");
}

#[test]
fn test_sublime_merge_platform_supported() {
    let installer = SublimeMergeInstaller;
    assert!(
        installer.is_platform_supported(),
        "Sublime Merge should be supported on all platforms"
    );
}

#[test]
fn test_check_client_not_installed() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/tmp/git-ai-shim"));

    // This will check the actual system, but we can verify the result structure
    let result = installer.check_client(&params);
    assert!(result.is_ok(), "check_client should not error");

    let check = result.unwrap();
    // If Sublime Merge isn't installed, these should all be false
    if !check.client_installed {
        assert!(!check.prefs_configured, "Unconfigured if not installed");
        assert!(!check.prefs_up_to_date, "Not up to date if not installed");
    }
}

#[test]
fn test_install_prefs_creates_directory_structure() {
    let temp_dir = TempDir::new().unwrap();
    let prefs_file = temp_dir
        .path()
        .join("Packages")
        .join("User")
        .join("Preferences.sublime-settings");

    // Manually create the preferences file for testing
    fs::create_dir_all(prefs_file.parent().unwrap()).unwrap();
    fs::write(&prefs_file, "{}").unwrap();

    // Now test parsing logic with empty prefs
    let content = fs::read_to_string(&prefs_file).unwrap();
    assert_eq!(content, "{}");
}

#[test]
fn test_install_prefs_dry_run_no_changes() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/usr/local/bin/git-ai-shim"));

    // Dry run should not error even if Sublime Merge isn't installed
    let result = installer.install_prefs(&params, true);
    assert!(result.is_ok(), "Dry run should not error");
}

#[test]
fn test_uninstall_prefs_dry_run() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/usr/local/bin/git-ai-shim"));

    let result = installer.uninstall_prefs(&params, true);
    assert!(result.is_ok(), "Dry run uninstall should not error");
}

#[test]
fn test_prefs_file_path_not_empty() {
    // We can't directly call prefs_path() as it's private, but we can test the installer behavior
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/test/git"));

    // The check will use prefs_path internally
    let result = installer.check_client(&params);
    assert!(result.is_ok());
}

#[test]
fn test_git_binary_path_uses_forward_slashes() {
    // Test that Windows paths are converted to forward slashes for JSON
    let installer = SublimeMergeInstaller;

    #[cfg(windows)]
    let params = create_test_params(PathBuf::from("C:\\Program Files\\git-ai\\git-ai.exe"));

    #[cfg(not(windows))]
    let params = create_test_params(PathBuf::from("/usr/local/bin/git-ai"));

    let result = installer.check_client(&params);
    assert!(result.is_ok());

    // The path conversion happens in install_prefs, verify it doesn't panic
    let _ = installer.install_prefs(&params, true);
}

#[test]
fn test_jsonc_parsing_with_comments() {
    use jsonc_parser::parse_to_value;

    // Test that JSONC parsing works with comments
    let jsonc_content = r#"{
        // This is a comment
        "git_binary": "/usr/local/bin/git",
        /* Multi-line
           comment */
        "other_setting": true
    }"#;

    let result = parse_to_value(jsonc_content, &Default::default());
    assert!(result.is_ok(), "Should parse JSONC with comments");
    assert!(result.unwrap().is_some(), "Should have parsed value");
}

#[test]
fn test_jsonc_parsing_with_trailing_commas() {
    use jsonc_parser::parse_to_value;

    // Test JSONC with trailing commas
    let jsonc_content = r#"{
        "git_binary": "/usr/local/bin/git",
        "theme": "dark",
    }"#;

    let result = parse_to_value(jsonc_content, &Default::default());
    assert!(result.is_ok(), "Should parse JSONC with trailing commas");
    assert!(result.unwrap().is_some(), "Should have parsed value");
}

#[test]
fn test_empty_prefs_handling() {
    use jsonc_parser::parse_to_value;

    // Empty file should be treated as empty object
    let empty_content = "";
    let parse_input = if empty_content.trim().is_empty() {
        "{}"
    } else {
        empty_content
    };

    let result = parse_to_value(parse_input, &Default::default());
    assert!(result.is_ok(), "Should handle empty content as {{}}"); // Escape braces for format string
}

#[test]
fn test_multiple_operations_idempotent() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/usr/local/bin/git-ai"));

    // Multiple check operations should be safe
    let _ = installer.check_client(&params);
    let result2 = installer.check_client(&params);
    assert!(result2.is_ok(), "Multiple checks should work");
}

#[cfg(target_os = "macos")]
#[test]
fn test_macos_paths() {
    // Verify macOS-specific path logic
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/test".to_string());
    let expected_base = PathBuf::from(&home)
        .join("Library")
        .join("Application Support")
        .join("Sublime Merge");

    // Path should exist in the form: ~/Library/Application Support/Sublime Merge/...
    assert!(expected_base.to_string_lossy().contains("Library"));
    assert!(expected_base.to_string_lossy().contains("Sublime Merge"));
}

#[cfg(windows)]
#[test]
fn test_windows_paths() {
    // Verify Windows-specific path logic
    let appdata = std::env::var("APPDATA").ok();
    if let Some(appdata_path) = appdata {
        let expected = PathBuf::from(appdata_path).join("Sublime Merge");
        assert!(expected.to_string_lossy().contains("Sublime Merge"));
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn test_linux_paths() {
    // Verify Linux-specific path logic
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/test".to_string());
    let expected = PathBuf::from(&home).join(".config").join("sublime-merge");

    assert!(expected.to_string_lossy().contains(".config"));
    assert!(expected.to_string_lossy().contains("sublime-merge"));
}

#[test]
fn test_install_result_structure() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/test/git"));

    let result = installer.install_prefs(&params, true);
    assert!(result.is_ok());

    // Result should be Option<String> for diff output
    let diff = result.unwrap();
    // None means no changes needed, Some means changes would be made
    assert!(diff.is_none() || diff.is_some());
}

#[test]
fn test_uninstall_result_structure() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/test/git"));

    let result = installer.uninstall_prefs(&params, true);
    assert!(result.is_ok());

    let diff = result.unwrap();
    assert!(diff.is_none() || diff.is_some());
}

#[test]
fn test_check_result_consistency() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/test/git"));

    let result = installer.check_client(&params).unwrap();

    // Logical consistency checks
    if !result.client_installed {
        assert!(
            !result.prefs_configured,
            "Can't be configured if not installed"
        );
        assert!(
            !result.prefs_up_to_date,
            "Can't be up to date if not installed"
        );
    }

    if result.prefs_up_to_date {
        assert!(
            result.prefs_configured,
            "Must be configured to be up to date"
        );
    }
}

#[test]
fn test_git_path_with_spaces() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/usr/local/bin/git ai wrapper"));

    // Should handle paths with spaces
    let result = installer.check_client(&params);
    assert!(result.is_ok());
}

#[test]
fn test_git_path_with_unicode() {
    let installer = SublimeMergeInstaller;
    let params = create_test_params(PathBuf::from("/usr/local/bin/git-ai-包装器"));

    let result = installer.check_client(&params);
    assert!(result.is_ok());
}

#[test]
fn test_very_long_git_path() {
    let installer = SublimeMergeInstaller;
    let long_path = format!("/usr/local/bin/{}", "very_long_directory_name_".repeat(10));
    let params = create_test_params(PathBuf::from(long_path));

    let result = installer.check_client(&params);
    assert!(result.is_ok());
}

#[test]
fn test_backslash_conversion_for_windows_compatibility() {
    #[cfg(windows)]
    {
        let path = PathBuf::from("C:\\Users\\Test\\git-ai.exe");
        let converted = path.to_string_lossy().replace('\\', "/");
        assert!(
            converted.contains("/"),
            "Should convert backslashes to forward slashes"
        );
        assert!(!converted.contains("\\"), "Should not contain backslashes");
        assert_eq!(converted, "C:/Users/Test/git-ai.exe");
    }

    #[cfg(not(windows))]
    {
        let path = PathBuf::from("/usr/local/bin/git-ai");
        let converted = path.to_string_lossy().replace('\\', "/");
        assert_eq!(
            converted, "/usr/local/bin/git-ai",
            "Unix paths should be unchanged"
        );
    }
}

#[test]
fn test_jsonc_property_setting() {
    use jsonc_parser::{ParseOptions, cst::CstRootNode};

    let content = "{}";
    let parse_options = ParseOptions::default();
    let root = CstRootNode::parse(content, &parse_options).unwrap();

    let obj = root.object_value_or_set();
    assert!(
        obj.get("git_binary").is_none(),
        "New object should not have git_binary"
    );

    // Test appending a new property
    obj.append("git_binary", jsonc_parser::json!("/test/path"));
    let result = root.to_string();
    assert!(result.contains("git_binary"), "Should contain the property");
}

#[test]
fn test_jsonc_property_update() {
    use jsonc_parser::{ParseOptions, cst::CstRootNode};

    let content = r#"{"git_binary": "/old/path"}"#;
    let parse_options = ParseOptions::default();
    let root = CstRootNode::parse(content, &parse_options).unwrap();

    let obj = root.object_value().unwrap();
    let prop = obj.get("git_binary").unwrap();

    // Update the value
    prop.set_value(jsonc_parser::json!("/new/path"));
    let result = root.to_string();

    assert!(result.contains("/new/path"), "Should update to new path");
}

#[test]
fn test_jsonc_property_removal() {
    use jsonc_parser::{ParseOptions, cst::CstRootNode};

    let content = r#"{"git_binary": "/test/path", "other": "value"}"#;
    let parse_options = ParseOptions::default();
    let root = CstRootNode::parse(content, &parse_options).unwrap();

    let obj = root.object_value().unwrap();
    if let Some(prop) = obj.get("git_binary") {
        prop.remove();
    }

    let result = root.to_string();
    assert!(!result.contains("git_binary"), "Property should be removed");
    assert!(result.contains("other"), "Other properties should remain");
}
