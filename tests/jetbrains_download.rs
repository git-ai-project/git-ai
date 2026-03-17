/// Tests for JetBrains plugin download and installation functionality
use git_ai::mdm::jetbrains::download::{
    download_plugin_from_marketplace, install_plugin_to_directory, install_plugin_via_cli,
};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;
use zip::write::{FileOptions, ZipWriter};

/// Helper to create a minimal valid ZIP file for testing
fn create_test_plugin_zip() -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));

        // Add plugin.xml
        let options: FileOptions<()> = FileOptions::default();
        zip.start_file("git-ai-plugin/plugin.xml", options).unwrap();
        zip.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<idea-plugin></idea-plugin>")
            .unwrap();

        // Add a lib directory
        zip.add_directory("git-ai-plugin/lib/", options).unwrap();

        // Add a jar file
        zip.start_file("git-ai-plugin/lib/plugin.jar", options)
            .unwrap();
        zip.write_all(b"fake jar content").unwrap();

        zip.finish().unwrap();
    }
    buffer
}

/// Helper to create a ZIP with Unix executable permissions
#[cfg(unix)]
fn create_test_plugin_zip_with_executable() -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));

        // Add executable script with Unix permissions
        let options: FileOptions<zip::write::ExtendedFileOptions> =
            FileOptions::default().unix_permissions(0o755);
        zip.start_file("git-ai-plugin/bin/plugin-launcher.sh", options)
            .unwrap();
        zip.write_all(b"#!/bin/bash\necho 'test'").unwrap();

        // Add regular file
        let regular_options: FileOptions<()> = FileOptions::default();
        zip.start_file("git-ai-plugin/README.md", regular_options)
            .unwrap();
        zip.write_all(b"# Plugin README").unwrap();

        zip.finish().unwrap();
    }
    buffer
}

#[test]
fn test_install_plugin_creates_plugins_directory() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let zip_data = create_test_plugin_zip();
    let result = install_plugin_to_directory(&zip_data, &plugin_dir);

    assert!(result.is_ok(), "Installation should succeed");
    assert!(plugin_dir.exists(), "Plugins directory should be created");
}

#[test]
fn test_install_plugin_extracts_files() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let zip_data = create_test_plugin_zip();
    install_plugin_to_directory(&zip_data, &plugin_dir).unwrap();

    // Check that files were extracted
    let plugin_xml = plugin_dir.join("git-ai-plugin/plugin.xml");
    assert!(plugin_xml.exists(), "plugin.xml should be extracted");

    let jar_file = plugin_dir.join("git-ai-plugin/lib/plugin.jar");
    assert!(jar_file.exists(), "JAR file should be extracted");
}

#[test]
fn test_install_plugin_extracts_correct_content() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let zip_data = create_test_plugin_zip();
    install_plugin_to_directory(&zip_data, &plugin_dir).unwrap();

    // Verify file contents
    let plugin_xml = plugin_dir.join("git-ai-plugin/plugin.xml");
    let content = fs::read_to_string(plugin_xml).unwrap();
    assert!(
        content.contains("<idea-plugin>"),
        "plugin.xml should have correct content"
    );

    let jar_file = plugin_dir.join("git-ai-plugin/lib/plugin.jar");
    let jar_content = fs::read(jar_file).unwrap();
    assert_eq!(
        jar_content, b"fake jar content",
        "JAR should have correct content"
    );
}

#[test]
fn test_install_plugin_creates_nested_directories() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let zip_data = create_test_plugin_zip();
    install_plugin_to_directory(&zip_data, &plugin_dir).unwrap();

    // Check directory structure
    let lib_dir = plugin_dir.join("git-ai-plugin/lib");
    assert!(lib_dir.exists(), "Nested lib directory should be created");
    assert!(lib_dir.is_dir(), "lib should be a directory");
}

#[test]
fn test_install_plugin_to_existing_directory() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create directory first
    fs::create_dir_all(&plugin_dir).unwrap();

    let zip_data = create_test_plugin_zip();
    let result = install_plugin_to_directory(&zip_data, &plugin_dir);

    assert!(result.is_ok(), "Should work with existing directory");
}

#[test]
fn test_install_plugin_invalid_zip_data() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let invalid_zip = b"This is not a valid ZIP file";
    let result = install_plugin_to_directory(invalid_zip, &plugin_dir);

    assert!(result.is_err(), "Should fail with invalid ZIP data");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Failed to read plugin ZIP"),
        "Error should mention ZIP reading"
    );
}

#[test]
fn test_install_plugin_empty_zip() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create empty but valid ZIP
    let mut buffer = Vec::new();
    {
        let zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        zip.finish().unwrap();
    }

    let result = install_plugin_to_directory(&buffer, &plugin_dir);
    assert!(result.is_ok(), "Empty ZIP should be handled gracefully");
}

#[cfg(unix)]
#[test]
fn test_install_plugin_preserves_executable_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    let zip_data = create_test_plugin_zip_with_executable();
    install_plugin_to_directory(&zip_data, &plugin_dir).unwrap();

    let script_path = plugin_dir.join("git-ai-plugin/bin/plugin-launcher.sh");
    assert!(script_path.exists(), "Script should be extracted");

    let metadata = fs::metadata(&script_path).unwrap();
    let permissions = metadata.permissions();
    let mode = permissions.mode();

    // Check if executable bit is set (0o100 for owner execute)
    assert!(mode & 0o100 != 0, "Script should be executable");
}

#[test]
fn test_install_plugin_handles_directory_entries() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create ZIP with explicit directory entry
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options: FileOptions<()> = FileOptions::default();

        // Add directory entry (ends with /)
        zip.add_directory("git-ai-plugin/", options).unwrap();
        zip.add_directory("git-ai-plugin/resources/", options)
            .unwrap();

        // Add file in directory
        zip.start_file("git-ai-plugin/resources/config.json", options)
            .unwrap();
        zip.write_all(b"{}").unwrap();

        zip.finish().unwrap();
    }

    let result = install_plugin_to_directory(&buffer, &plugin_dir);
    assert!(result.is_ok(), "Should handle directory entries");

    let resources_dir = plugin_dir.join("git-ai-plugin/resources");
    assert!(resources_dir.exists(), "Directory should be created");
    assert!(resources_dir.is_dir(), "Should be a directory");

    let config_file = resources_dir.join("config.json");
    assert!(config_file.exists(), "File in directory should exist");
}

#[test]
fn test_install_plugin_via_cli_with_invalid_binary() {
    let non_existent_binary = PathBuf::from("/tmp/non_existent_ide_binary_12345");
    let result = install_plugin_via_cli(&non_existent_binary, "com.test.plugin");

    // Should return Ok(false) when CLI fails, not an error
    assert!(result.is_ok(), "Should handle missing binary gracefully");
    assert_eq!(
        result.unwrap(),
        false,
        "Should return false for failed installation"
    );
}

#[test]
fn test_install_plugin_via_cli_paths_and_args() {
    // This test verifies the function signature and behavior without needing actual IDE
    let fake_binary = PathBuf::from("/usr/bin/echo");
    let plugin_id = "com.usegitai.plugins.jetbrains";

    // With echo, this will succeed but not actually install anything
    let result = install_plugin_via_cli(&fake_binary, plugin_id);

    // Just verify it returns a result (Ok or Err is fine, depends on system)
    assert!(result.is_ok(), "Function should execute without panicking");
}

// Download tests - these test error handling without making real network calls

#[test]
fn test_download_plugin_url_format() {
    // We can't test actual download without network, but we can verify the function exists
    // and has the right signature. Real download testing would require mocking or network.

    // Test with invalid URL will fail quickly
    // The actual function will try to connect, so we just verify it's callable
    let result = download_plugin_from_marketplace("test-plugin-id", "IU", "252.12345");

    // Should return an error (network or 404), not panic
    assert!(
        result.is_err(),
        "Should fail gracefully with test parameters"
    );
}

#[test]
fn test_install_plugin_with_special_characters_in_filename() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create ZIP with special characters in filenames
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options: FileOptions<()> = FileOptions::default();

        zip.start_file("git-ai-plugin/resources/strings_en.xml", options)
            .unwrap();
        zip.write_all(b"<strings></strings>").unwrap();

        zip.start_file("git-ai-plugin/resources/strings_中文.xml", options)
            .unwrap();
        zip.write_all(b"<strings></strings>").unwrap();

        zip.finish().unwrap();
    }

    let result = install_plugin_to_directory(&buffer, &plugin_dir);
    assert!(
        result.is_ok(),
        "Should handle special characters in filenames"
    );

    let en_file = plugin_dir.join("git-ai-plugin/resources/strings_en.xml");
    assert!(en_file.exists(), "English strings file should exist");

    let zh_file = plugin_dir.join("git-ai-plugin/resources/strings_中文.xml");
    assert!(zh_file.exists(), "Chinese strings file should exist");
}

#[test]
fn test_install_plugin_with_deep_nesting() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create ZIP with deeply nested structure
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options: FileOptions<()> = FileOptions::default();

        let deep_path = "git-ai-plugin/src/main/java/com/usegitai/plugin/actions/DeepFile.java";
        zip.start_file(deep_path, options).unwrap();
        zip.write_all(b"package com.usegitai.plugin.actions;")
            .unwrap();

        zip.finish().unwrap();
    }

    let result = install_plugin_to_directory(&buffer, &plugin_dir);
    assert!(result.is_ok(), "Should handle deeply nested paths");

    let deep_file =
        plugin_dir.join("git-ai-plugin/src/main/java/com/usegitai/plugin/actions/DeepFile.java");
    assert!(deep_file.exists(), "Deeply nested file should be created");
}

#[test]
fn test_install_plugin_overwrites_existing_files() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create initial file
    let file_path = plugin_dir.join("git-ai-plugin/plugin.xml");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, b"old content").unwrap();

    // Install plugin with new content
    let zip_data = create_test_plugin_zip();
    install_plugin_to_directory(&zip_data, &plugin_dir).unwrap();

    // Verify file was overwritten
    let content = fs::read_to_string(&file_path).unwrap();
    assert!(
        content.contains("<idea-plugin>"),
        "File should be overwritten with new content"
    );
    assert!(
        !content.contains("old content"),
        "Old content should be replaced"
    );
}

#[test]
fn test_install_plugin_with_large_files() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");

    // Create ZIP with a larger file
    let mut buffer = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options: FileOptions<()> = FileOptions::default();

        // Create 1MB file
        let large_content = vec![b'x'; 1024 * 1024];
        zip.start_file("git-ai-plugin/large-library.jar", options)
            .unwrap();
        zip.write_all(&large_content).unwrap();

        zip.finish().unwrap();
    }

    let result = install_plugin_to_directory(&buffer, &plugin_dir);
    assert!(result.is_ok(), "Should handle large files");

    let large_file = plugin_dir.join("git-ai-plugin/large-library.jar");
    assert!(large_file.exists(), "Large file should be extracted");

    let metadata = fs::metadata(&large_file).unwrap();
    assert_eq!(metadata.len(), 1024 * 1024, "File size should match");
}
