//! End-to-end tests for the `git-ai install` command.
//!
//! Verifies:
//! 1. Basic install creates the post-commit hook with correct content and permissions
//! 2. Trace2 config is set globally after install
//! 3. Idempotent install (running twice doesn't corrupt anything)
//! 4. Socket path resolution follows ~/.git-ai/internal/daemon/trace2.sock convention
//! 5. V1 daemon PID file cleanup when PID doesn't exist
//! 6. Running outside a git repo produces a clear error

use std::fs;
use std::process::Command;

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Create an isolated HOME with a minimal .gitconfig so `git config --global` works.
fn create_isolated_home(base: &std::path::Path) -> std::path::PathBuf {
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    // Create a minimal .gitconfig so git doesn't complain
    let gitconfig = home.join(".gitconfig");
    fs::write(&gitconfig, "[user]\n\tname = Test\n\temail = t@t.com\n").unwrap();
    home
}

/// Create a git repo inside the given base directory with isolated HOME.
fn create_test_repo(base: &std::path::Path, home: &std::path::Path) -> std::path::PathBuf {
    let repo = base.join("repo");
    fs::create_dir_all(&repo).unwrap();

    let git = real_git_executable();
    let output = Command::new(git)
        .args(["init", repo.to_str().unwrap()])
        .env("HOME", home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git init failed");

    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo)
            .args(&args)
            .env("HOME", home)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Create an initial commit so the repo has HEAD
    fs::write(repo.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo)
        .args(["add", "-A"])
        .env("HOME", home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo)
        .args(["commit", "-m", "initial"])
        .env("HOME", home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    repo
}

#[test]
fn test_install_creates_hook_with_correct_content_and_permissions() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());
    let repo = create_test_repo(base.path(), &home);

    let binary = get_binary_path();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "git-ai install failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the hook file exists
    let hook_path = repo.join(".git").join("hooks").join("post-commit");
    assert!(hook_path.exists(), "post-commit hook should exist");

    // Verify hook content
    let content = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        content, "#!/bin/sh\ngit-ai post-commit\n",
        "hook content should be the expected shebang + command"
    );

    // Verify executable permissions (unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&hook_path).unwrap().permissions();
        let mode = perms.mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "hook should have 755 permissions, got {:o}",
            mode
        );
    }

    // Verify the stdout message
    assert!(
        stdout.contains("installed post-commit hook"),
        "should print hook installation message, got: {}",
        stdout
    );
}

#[test]
fn test_install_configures_trace2_global() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());
    let repo = create_test_repo(base.path(), &home);

    let binary = get_binary_path();
    let git = real_git_executable();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check trace2.eventTarget in global config
    let event_target = Command::new(git)
        .args(["config", "--global", "trace2.eventTarget"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        event_target.status.success(),
        "trace2.eventTarget not set in global config"
    );
    let target_value = String::from_utf8_lossy(&event_target.stdout)
        .trim()
        .to_string();
    assert!(
        target_value.starts_with("af_unix:stream:"),
        "trace2.eventTarget should start with af_unix:stream:, got: {}",
        target_value
    );
    assert!(
        target_value.contains("trace2.sock"),
        "trace2.eventTarget should reference trace2.sock, got: {}",
        target_value
    );

    // Check trace2.eventNesting
    let event_nesting = Command::new(git)
        .args(["config", "--global", "trace2.eventNesting"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        event_nesting.status.success(),
        "trace2.eventNesting not set in global config"
    );
    let nesting_value = String::from_utf8_lossy(&event_nesting.stdout)
        .trim()
        .to_string();
    assert_eq!(
        nesting_value, "10",
        "trace2.eventNesting should be 10, got: {}",
        nesting_value
    );

    // Verify stdout message about trace2 config
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("configured trace2 event target"),
        "should print trace2 configuration message, got: {}",
        stdout
    );
}

#[test]
fn test_install_is_idempotent() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());
    let repo = create_test_repo(base.path(), &home);

    let binary = get_binary_path();
    let git = real_git_executable();

    // First install
    let output1 = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output1.status.success(),
        "first install failed: {}",
        String::from_utf8_lossy(&output1.stderr)
    );

    // Capture state after first install
    let hook_path = repo.join(".git").join("hooks").join("post-commit");
    let hook_content_1 = fs::read_to_string(&hook_path).unwrap();
    let target_1 = Command::new(git)
        .args(["config", "--global", "trace2.eventTarget"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let target_value_1 = String::from_utf8_lossy(&target_1.stdout).trim().to_string();

    // Second install
    let output2 = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output2.status.success(),
        "second install failed: {}",
        String::from_utf8_lossy(&output2.stderr)
    );

    // Verify state is identical
    let hook_content_2 = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        hook_content_1, hook_content_2,
        "hook content should be identical after second install"
    );

    let target_2 = Command::new(git)
        .args(["config", "--global", "trace2.eventTarget"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let target_value_2 = String::from_utf8_lossy(&target_2.stdout).trim().to_string();
    assert_eq!(
        target_value_1, target_value_2,
        "trace2.eventTarget should be identical after second install"
    );

    // Verify permissions are still correct
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&hook_path).unwrap().permissions();
        let mode = perms.mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "permissions should still be 755 after re-install"
        );
    }
}

#[test]
fn test_socket_path_follows_convention() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());
    let repo = create_test_repo(base.path(), &home);

    let binary = get_binary_path();
    let git = real_git_executable();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Read the configured socket path
    let target_output = Command::new(git)
        .args(["config", "--global", "trace2.eventTarget"])
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let target_value = String::from_utf8_lossy(&target_output.stdout)
        .trim()
        .to_string();

    // Strip the af_unix:stream: prefix to get the path
    let socket_path = target_value
        .strip_prefix("af_unix:stream:")
        .expect("should have af_unix:stream: prefix");

    // The socket path should follow the convention:
    // ~/.git-ai/internal/daemon/trace2.sock
    // OR /tmp/git-ai-d-<hash>/trace2.sock (if path too long)
    let expected_conventional = format!("{}/.git-ai/internal/daemon/trace2.sock", home.display());

    if expected_conventional.len() < 100 {
        // Should use the conventional path since it's short enough
        assert_eq!(
            socket_path, expected_conventional,
            "socket path should follow ~/.git-ai/internal/daemon/trace2.sock convention"
        );
    } else {
        // Path was too long, should have hashed to /tmp
        assert!(
            socket_path.starts_with("/tmp/git-ai-d-"),
            "long paths should hash to /tmp/git-ai-d-<hash>/trace2.sock, got: {}",
            socket_path
        );
        assert!(
            socket_path.ends_with("/trace2.sock"),
            "hashed path should end with /trace2.sock, got: {}",
            socket_path
        );
    }
}

#[test]
fn test_socket_path_hashes_to_tmp_when_too_long() {
    let base = tempfile::tempdir().unwrap();

    // Create a very deeply nested HOME so that the socket path exceeds 100 chars
    let long_segment = "a".repeat(80);
    let deep_home = base.path().join(&long_segment).join(&long_segment);
    fs::create_dir_all(&deep_home).unwrap();

    // Create .gitconfig in the deep home
    let gitconfig = deep_home.join(".gitconfig");
    fs::write(&gitconfig, "[user]\n\tname = Test\n\temail = t@t.com\n").unwrap();

    let repo = create_test_repo(base.path(), &deep_home);

    let binary = get_binary_path();
    let git = real_git_executable();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &deep_home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Read the configured socket path
    let target_output = Command::new(git)
        .args(["config", "--global", "trace2.eventTarget"])
        .env("HOME", &deep_home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let target_value = String::from_utf8_lossy(&target_output.stdout)
        .trim()
        .to_string();

    let socket_path = target_value
        .strip_prefix("af_unix:stream:")
        .expect("should have af_unix:stream: prefix");

    // With the deeply nested HOME, the path should exceed 100 chars and hash to /tmp
    let conventional_path = format!(
        "{}/.git-ai/internal/daemon/trace2.sock",
        deep_home.display()
    );

    if conventional_path.len() >= 100 {
        assert!(
            socket_path.starts_with("/tmp/git-ai-d-"),
            "should hash to /tmp when path too long, got: {}",
            socket_path
        );
        assert!(
            socket_path.ends_with("/trace2.sock"),
            "hashed path should end with /trace2.sock, got: {}",
            socket_path
        );
        // Hash should be 16 hex chars (8 bytes of SHA256)
        let hash_part = socket_path
            .strip_prefix("/tmp/git-ai-d-")
            .unwrap()
            .strip_suffix("/trace2.sock")
            .unwrap();
        assert_eq!(
            hash_part.len(),
            16,
            "hash should be 16 hex chars, got: {} (len {})",
            hash_part,
            hash_part.len()
        );
        assert!(
            hash_part.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex, got: {}",
            hash_part
        );
    } else {
        // If our "long" path still wasn't long enough, just verify convention
        assert_eq!(socket_path, conventional_path);
    }
}

#[test]
fn test_v1_daemon_pid_cleanup_nonexistent_pid() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());
    let repo = create_test_repo(base.path(), &home);

    // Create a fake PID file with a non-existent PID
    let daemon_dir = home.join(".git-ai").join("internal").join("daemon");
    fs::create_dir_all(&daemon_dir).unwrap();
    let pid_file = daemon_dir.join("daemon.pid.json");
    // Use PID 99999999 which almost certainly doesn't exist
    fs::write(&pid_file, r#"{"pid": 99999999, "version": "1.0.0"}"#).unwrap();
    assert!(pid_file.exists(), "pid file should exist before install");

    let binary = get_binary_path();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "install should succeed even with stale PID file:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The PID file should be cleaned up since the process doesn't exist
    assert!(
        !pid_file.exists(),
        "stale PID file should be removed after install"
    );

    // Verify install still completed correctly
    let hook_path = repo.join(".git").join("hooks").join("post-commit");
    assert!(hook_path.exists(), "hook should still be installed");
}

#[test]
fn test_install_outside_git_repo_produces_error() {
    let base = tempfile::tempdir().unwrap();
    let home = create_isolated_home(base.path());

    // Create a directory that is NOT a git repo
    let not_a_repo = base.path().join("not-a-repo");
    fs::create_dir_all(&not_a_repo).unwrap();

    let binary = get_binary_path();

    let output = Command::new(binary)
        .args(["install"])
        .current_dir(&not_a_repo)
        .env("HOME", &home)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        // Prevent git from finding a repo in parent directories
        .env("GIT_CEILING_DIRECTORIES", base.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "install should fail outside a git repo"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not in a git repository") || stderr.contains("not a git repository"),
        "error message should mention 'not in a git repository', got: {}",
        stderr
    );
}
