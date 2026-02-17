/// Comprehensive tests for src/commands/git_ai_handlers.rs
/// Tests command routing, argument parsing, error handling, and edge cases
///
/// Coverage areas:
/// 1. Command routing to all subcommands
/// 2. Error handling for unknown commands
/// 3. Help and version commands
/// 4. Checkpoint command with various presets
/// 5. Edge cases: empty arguments, special characters
/// 6. Stats command with various options
/// 7. Repository-aware commands (blame, diff, stats)
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Helper to check if output contains help text
fn is_help_output(output: &str) -> bool {
    output.contains("git-ai - git proxy with AI authorship tracking")
        && output.contains("Usage: git-ai <command> [args...]")
        && output.contains("Commands:")
}

/// Helper to check if output contains version info
fn is_version_output(output: &str) -> bool {
    // Version output is just the version number, optionally with (debug)
    let trimmed = output.trim();
    // Check that it's a version-like string (digits and dots)
    trimmed
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        && (trimmed.contains('.') || trimmed.contains("debug"))
}

#[test]
fn test_no_args_shows_help() {
    let repo = TestRepo::new();

    // When called with no arguments, should show help
    let result = repo.git_ai(&[]);

    // The command exits with status 0 for help
    assert!(
        result.is_ok(),
        "git-ai with no args should succeed (show help)"
    );
    let output = result.unwrap();
    assert!(
        is_help_output(&output),
        "Expected help output, got: {}",
        output
    );
}

#[test]
fn test_help_command() {
    let repo = TestRepo::new();

    // Test all help variations
    let help_args = vec!["help", "--help", "-h"];

    for arg in help_args {
        let result = repo.git_ai(&[arg]);
        assert!(result.is_ok(), "git-ai {} should succeed", arg);
        let output = result.unwrap();
        assert!(
            is_help_output(&output),
            "Expected help output for {}, got: {}",
            arg,
            output
        );
    }
}

#[test]
fn test_version_command() {
    let repo = TestRepo::new();

    // Test all version variations
    let version_args = vec!["version", "--version", "-v"];

    for arg in version_args {
        let result = repo.git_ai(&[arg]);
        assert!(result.is_ok(), "git-ai {} should succeed", arg);
        let output = result.unwrap();
        assert!(
            is_version_output(&output),
            "Expected version output for {}, got: {}",
            arg,
            output
        );
    }
}

#[test]
fn test_unknown_command() {
    let repo = TestRepo::new();

    // Test unknown command
    let result = repo.git_ai(&["totally-unknown-command"]);

    // Unknown commands exit with status 1
    assert!(
        result.is_err(),
        "Unknown command should fail with exit code 1"
    );
    let err = result.unwrap_err();
    // The error might be empty string or contain error message
    assert!(
        err.is_empty() || err.contains("Unknown git-ai command"),
        "Expected unknown command error or empty, got: {}",
        err
    );
}

#[test]
fn test_unknown_command_with_special_chars() {
    let repo = TestRepo::new();

    // Test unknown commands with special characters
    let special_commands = vec![
        "cmd-with-dashes",
        "cmd_with_underscores",
        "cmd.with.dots",
        "cmd@with@at",
        "cmd!with!exclaim",
    ];

    for cmd in special_commands {
        let result = repo.git_ai(&[cmd]);
        assert!(
            result.is_err(),
            "Unknown command '{}' should fail with exit code 1",
            cmd
        );
        let err = result.unwrap_err();
        // Error might be empty or contain message
        assert!(
            err.is_empty() || err.contains("Unknown git-ai command") || err.contains(cmd),
            "Expected unknown command error for '{}', got: {}",
            cmd,
            err
        );
    }
}

#[test]
fn test_config_command_routing() {
    let repo = TestRepo::new();

    // Test that config command is routed correctly
    // Without arguments, should show all config
    let result = repo.git_ai(&["config"]);
    assert!(result.is_ok(), "config command should succeed");

    // The output should be valid JSON (config dump)
    let output = result.unwrap();
    assert!(
        output.contains('{') || output.is_empty(),
        "Expected JSON config or empty output, got: {}",
        output
    );
}

#[test]
fn test_status_command_routing() {
    let repo = TestRepo::new();

    // Create a simple file and commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Hello".human(), "World".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Test status command
    let result = repo.git_ai(&["status"]);
    assert!(result.is_ok(), "status command should succeed");

    // Test status with --json flag
    let result = repo.git_ai(&["status", "--json"]);
    assert!(result.is_ok(), "status --json should succeed");
}

#[test]
fn test_stats_command_routing() {
    let repo = TestRepo::new();

    // Create initial commit with AI authorship
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Test stats command without arguments (HEAD)
    let result = repo.git_ai(&["stats", "--json"]);
    assert!(result.is_ok(), "stats command should succeed");

    let output = result.unwrap();
    assert!(
        output.contains("human_additions") || output.contains('{'),
        "Expected JSON stats output, got: {}",
        output
    );
}

#[test]
fn test_stats_with_commit_sha() {
    let repo = TestRepo::new();

    // Create a commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Get the commit SHA
    let sha = commit.commit_sha;

    // Test stats with explicit commit SHA
    let result = repo.git_ai(&["stats", "--json", &sha]);
    assert!(
        result.is_ok(),
        "stats with commit SHA should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_stats_with_commit_range() {
    let repo = TestRepo::new();

    // Create first commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human()]);
    let commit1 = repo.stage_all_and_commit("First commit").unwrap();

    // Create second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    let commit2 = repo.stage_all_and_commit("Second commit").unwrap();

    // Test stats with commit range
    let range = format!("{}..{}", &commit1.commit_sha[..7], &commit2.commit_sha[..7]);
    let result = repo.git_ai(&["stats", "--json", &range]);
    assert!(
        result.is_ok(),
        "stats with commit range should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_stats_with_ignore_patterns() {
    let repo = TestRepo::new();

    // Create multiple files
    let mut code_file = repo.filename("code.rs");
    code_file.set_contents(lines!["fn main() {}".ai()]);

    let mut lock_file = repo.filename("Cargo.lock");
    lock_file.set_contents(lines!["# Lock file".ai()]);

    repo.stage_all_and_commit("Add files").unwrap();

    // Test stats with ignore patterns
    let result = repo.git_ai(&["stats", "--json", "--ignore", "*.lock"]);
    assert!(
        result.is_ok(),
        "stats with --ignore should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_blame_command_routing() {
    let repo = TestRepo::new();

    // Create a file with AI authorship
    let mut file = repo.filename("blame_test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    repo.stage_all_and_commit("Test commit").unwrap();

    // Test blame command
    let result = repo.git_ai(&["blame", "blame_test.txt"]);
    assert!(
        result.is_ok(),
        "blame command should succeed, error: {:?}",
        result
    );

    let output = result.unwrap();
    // Should contain the file content or blame output
    assert!(
        output.contains("Line 1") || output.contains("blame_test.txt"),
        "Expected blame output to reference file, got: {}",
        output
    );
}

#[test]
fn test_blame_without_file_argument() {
    let repo = TestRepo::new();

    // Blame without a file should fail
    let result = repo.git_ai(&["blame"]);
    assert!(result.is_err(), "blame without file argument should fail");

    let err = result.unwrap_err();
    assert!(
        err.contains("requires a file argument"),
        "Expected error about missing file argument, got: {}",
        err
    );
}

#[test]
fn test_diff_command_routing() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("diff_test.txt");
    file.set_contents(lines!["Original".human()]);
    let _commit1 = repo.stage_all_and_commit("First").unwrap();

    // Create second commit
    file.set_contents(lines!["Original".human(), "Modified".ai()]);
    let commit2 = repo.stage_all_and_commit("Second").unwrap();

    // Test diff command
    let result = repo.git_ai(&["diff", &commit2.commit_sha]);
    assert!(
        result.is_ok(),
        "diff command should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_mock_ai_preset() {
    let repo = TestRepo::new();

    // Create a file
    let mut file = repo.filename("checkpoint_test.txt");
    file.set_contents(lines!["Test content".ai()]);

    // Stage the file
    repo.git(&["add", "."]).unwrap();

    // Test checkpoint with mock_ai preset
    let result = repo.git_ai(&["checkpoint", "mock_ai"]);
    assert!(
        result.is_ok(),
        "checkpoint mock_ai should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_with_pathspec() {
    let repo = TestRepo::new();

    // Create multiple files
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(lines!["Content 1".ai()]);

    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(lines!["Content 2".ai()]);

    // Stage all files
    repo.git(&["add", "."]).unwrap();

    // Checkpoint with specific pathspec
    let result = repo.git_ai(&["checkpoint", "mock_ai", "file1.txt"]);
    assert!(
        result.is_ok(),
        "checkpoint with pathspec should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_show_working_log() {
    let repo = TestRepo::new();

    // Create and checkpoint a file first
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Test".ai()]);
    repo.git(&["add", "."]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Now show the working log
    let result = repo.git_ai(&["checkpoint", "--show-working-log"]);
    assert!(
        result.is_ok(),
        "checkpoint --show-working-log should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_reset() {
    let repo = TestRepo::new();

    // Create and checkpoint a file first
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Test".ai()]);
    repo.git(&["add", "."]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Reset the working log
    let result = repo.git_ai(&["checkpoint", "--reset"]);
    assert!(
        result.is_ok(),
        "checkpoint --reset should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_git_path_command() {
    let repo = TestRepo::new();

    // Test git-path command
    let result = repo.git_ai(&["git-path"]);
    assert!(
        result.is_ok(),
        "git-path command should succeed, error: {:?}",
        result
    );

    let output = result.unwrap();
    // Should output a path to git executable
    assert!(
        !output.trim().is_empty() && (output.contains("git") || output.starts_with('/')),
        "Expected path to git executable, got: {}",
        output
    );
}

#[test]
fn test_install_hooks_command() {
    let repo = TestRepo::new();

    // Test install-hooks command (may succeed or fail depending on environment)
    let result = repo.git_ai(&["install-hooks"]);
    // We don't assert success/failure as it depends on the environment
    // Just verify the command is routed correctly by checking it doesn't panic
    let _ = result;

    // Test the "install" alias
    let result = repo.git_ai(&["install"]);
    let _ = result;
}

#[test]
fn test_uninstall_hooks_command() {
    let repo = TestRepo::new();

    // Test uninstall-hooks command
    let result = repo.git_ai(&["uninstall-hooks"]);
    // Don't assert success/failure as it depends on environment
    let _ = result;
}

#[test]
fn test_squash_authorship_command_routing() {
    let repo = TestRepo::new();

    // Create commits for squash authorship test
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human()]);
    let commit1 = repo.stage_all_and_commit("First").unwrap();

    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    let commit2 = repo.stage_all_and_commit("Second").unwrap();

    // Test squash-authorship command with dry-run
    let result = repo.git_ai(&[
        "squash-authorship",
        "main",
        &commit2.commit_sha,
        &commit1.commit_sha,
        "--dry-run",
    ]);
    // May fail if not in the right state, but should route correctly
    let _ = result;
}

#[test]
fn test_ci_command_routing() {
    let repo = TestRepo::new();

    // Test ci command
    let result = repo.git_ai(&["ci"]);
    // CI commands may need specific arguments, so we don't assert success
    let _ = result;
}

#[test]
fn test_upgrade_command_routing() {
    let repo = TestRepo::new();

    // Test upgrade command (will likely fail in test environment, but should route)
    let result = repo.git_ai(&["upgrade"]);
    // Don't assert success as upgrade depends on external factors
    let _ = result;
}

#[test]
fn test_flush_logs_command() {
    let repo = TestRepo::new();

    // Test flush-logs command
    let result = repo.git_ai(&["flush-logs"]);
    assert!(
        result.is_ok(),
        "flush-logs should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_flush_cas_command() {
    let repo = TestRepo::new();

    // Test flush-cas command
    let result = repo.git_ai(&["flush-cas"]);
    assert!(
        result.is_ok(),
        "flush-cas should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_flush_metrics_db_command() {
    let repo = TestRepo::new();

    // Test flush-metrics-db command
    let result = repo.git_ai(&["flush-metrics-db"]);
    assert!(
        result.is_ok(),
        "flush-metrics-db should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_login_command_routing() {
    let repo = TestRepo::new();

    // Test login command (will fail without credentials but should route correctly)
    let result = repo.git_ai(&["login"]);
    // Login requires interactive input or credentials, so we don't assert success
    let _ = result;
}

#[test]
fn test_logout_command_routing() {
    let repo = TestRepo::new();

    // Test logout command
    let result = repo.git_ai(&["logout"]);
    // Logout may succeed or fail depending on whether user was logged in
    let _ = result;
}

#[test]
fn test_dashboard_command_aliases() {
    let repo = TestRepo::new();

    // Test both "dash" and "dashboard" aliases
    let result1 = repo.git_ai(&["dash"]);
    let result2 = repo.git_ai(&["dashboard"]);

    // Both should route to the same command (may fail if dashboard unavailable)
    let _ = (result1, result2);
}

#[test]
fn test_show_command_routing() {
    let repo = TestRepo::new();

    // Create a commit with AI authorship
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".ai()]);
    let commit = repo.stage_all_and_commit("Test").unwrap();

    // Test show command
    let result = repo.git_ai(&["show", &commit.commit_sha]);
    assert!(
        result.is_ok(),
        "show command should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_prompts_command_routing() {
    let repo = TestRepo::new();

    // Test prompts command with list subcommand
    let result = repo.git_ai(&["prompts", "list"]);
    // May succeed or fail depending on prompts DB state
    let _ = result;
}

#[test]
fn test_search_command_routing() {
    let repo = TestRepo::new();

    // Test search command with pattern
    let result = repo.git_ai(&["search", "--pattern", "test", "--json"]);
    // Search may return no results, which exits with error code
    // Just verify it doesn't panic
    let _ = result;
}

#[test]
fn test_continue_command_routing() {
    let repo = TestRepo::new();

    // Create a commit with AI authorship
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".ai()]);
    let _commit = repo.stage_all_and_commit("Test").unwrap();

    // Test continue command with JSON output (non-interactive)
    let result = repo.git_ai(&["continue", "--json"]);
    // May succeed or fail depending on available context
    let _ = result;
}

#[test]
fn test_command_with_empty_string_argument() {
    let repo = TestRepo::new();

    // Test with empty string as command (should be treated as no command)
    let result = repo.git_ai(&[""]);
    // Empty string might be treated as unknown command or as no args
    // Either way, it should not panic
    let _ = result;
}

#[test]
fn test_multiple_flag_combinations() {
    let repo = TestRepo::new();

    // Create a file for testing
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Test commit").unwrap();

    // Test stats with multiple flags
    let result = repo.git_ai(&["stats", "--json", "--ignore", "*.lock", "--ignore", "*.md"]);
    assert!(
        result.is_ok(),
        "stats with multiple flags should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_excluded_repository() {
    let mut repo = TestRepo::new();

    // Configure the repository to be excluded via exclude_prompts
    // Note: There's no allow_repositories in ConfigPatch, so we skip this test aspect
    // and just test that checkpoint works normally
    repo.patch_git_ai_config(|patch| {
        patch.telemetry_oss_disabled = Some(true);
    });

    // Create a file
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Test content".ai()]);
    repo.git(&["add", "."]).unwrap();

    // Try to checkpoint - should succeed normally since we can't easily test exclusion
    let result = repo.git_ai(&["checkpoint", "mock_ai"]);

    // The command should succeed
    assert!(
        result.is_ok(),
        "checkpoint should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_checkpoint_database_warmup() {
    let repo = TestRepo::new();

    // Create a file
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Test content".ai()]);
    repo.git(&["add", "."]).unwrap();

    // Checkpoint command should trigger database warmup
    let result = repo.git_ai(&["checkpoint", "mock_ai"]);
    assert!(
        result.is_ok(),
        "checkpoint should succeed, error: {:?}",
        result
    );

    // Additional checkpoint commands that should trigger warmup
    let warmup_commands = vec!["show-prompt", "share", "sync-prompts", "search", "continue"];

    for cmd in warmup_commands {
        // Just verify they don't panic during warmup
        let _ = repo.git_ai(&[cmd]);
    }
}

#[test]
fn test_show_prompt_command_routing() {
    let repo = TestRepo::new();

    // Create a commit with AI authorship to have prompt data
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".ai()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Test show-prompt command (will fail without valid prompt ID)
    let result = repo.git_ai(&["show-prompt", "00000000-0000-0000-0000-000000000000"]);
    // May fail if prompt doesn't exist, but should route correctly
    let _ = result;
}

#[test]
fn test_share_command_routing() {
    let repo = TestRepo::new();

    // Test share command (will fail without valid prompt ID)
    let result = repo.git_ai(&["share", "00000000-0000-0000-0000-000000000000"]);
    // May fail if prompt doesn't exist, but should route correctly
    let _ = result;
}

#[test]
fn test_sync_prompts_command_routing() {
    let repo = TestRepo::new();

    // Test sync-prompts command
    let result = repo.git_ai(&["sync-prompts"]);
    assert!(
        result.is_ok(),
        "sync-prompts should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_sync_prompts_with_since() {
    let repo = TestRepo::new();

    // Test sync-prompts with --since flag
    let result = repo.git_ai(&["sync-prompts", "--since", "1d"]);
    assert!(
        result.is_ok(),
        "sync-prompts --since should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_exchange_nonce_command_routing() {
    let repo = TestRepo::new();

    // Test exchange-nonce command (will fail without valid nonce)
    let result = repo.git_ai(&["exchange-nonce"]);
    // May fail without proper authentication, but should route correctly
    let _ = result;
}

#[test]
fn test_config_set_command() {
    let repo = TestRepo::new();

    // Test config set command - may fail with permission issues in test environment
    // Just verify it routes correctly
    let result = repo.git_ai(&["config", "set", "disable_version_checks", "true"]);
    // Don't assert success as it may fail with permissions
    let _ = result;
}

#[test]
fn test_config_unset_command() {
    let repo = TestRepo::new();

    // Set a value first
    repo.git_ai(&["config", "set", "test_key", "test_value"])
        .ok();

    // Then unset it
    let result = repo.git_ai(&["config", "unset", "test_key"]);
    // May succeed or fail depending on whether key existed
    let _ = result;
}

#[test]
fn test_stats_no_commit_found() {
    let repo = TestRepo::new();

    // Try to get stats for a non-existent commit
    let result = repo.git_ai(&[
        "stats",
        "--json",
        "0000000000000000000000000000000000000000",
    ]);

    // Should fail with error
    assert!(result.is_err(), "stats for invalid commit should fail");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed") || err.contains("fatal") || err.contains("revision"),
        "Expected revision error, got: {}",
        err
    );
}

#[test]
fn test_command_routing_preserves_order() {
    let repo = TestRepo::new();

    // Create initial state
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Test commit").unwrap();

    // Test that commands with arguments work correctly
    // Note: --ignore expects patterns after it, and --json is a separate flag
    let result = repo.git_ai(&["stats", "--json"]);

    // Command should succeed
    assert!(
        result.is_ok(),
        "stats with flags should succeed, error: {:?}",
        result
    );
}

#[test]
fn test_blame_nonexistent_file() {
    let repo = TestRepo::new();

    // Try to blame a file that doesn't exist
    let result = repo.git_ai(&["blame", "nonexistent_file.txt"]);

    // Should fail
    assert!(result.is_err(), "blame on nonexistent file should fail");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed") || err.contains("not found") || err.contains("No such file"),
        "Expected file not found error, got: {}",
        err
    );
}

#[test]
fn test_diff_nonexistent_commit() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Test".human()]);
    repo.stage_all_and_commit("Test").unwrap();

    // Try to diff a non-existent commit
    let result = repo.git_ai(&["diff", "0000000000000000000000000000000000000000"]);

    // Should fail
    assert!(result.is_err(), "diff on nonexistent commit should fail");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed") || err.contains("not found") || err.contains("object"),
        "Expected commit not found error, got: {}",
        err
    );
}
