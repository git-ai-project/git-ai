use crate::repos::test_repo::get_binary_path;
use std::process::Command;

#[test]
fn daemon_start_refuses_security_sandbox_with_inaccessible_daemon_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let blocked_daemon_home = temp.path().join("daemon-home-file");
    std::fs::write(&blocked_daemon_home, "not a directory").unwrap();

    let output = Command::new(get_binary_path())
        .args(["bg", "start"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("HOMEDRIVE", "")
        .env("HOMEPATH", "")
        .env("GIT_AI_DAEMON_HOME", &blocked_daemon_home)
        .env_remove("GIT_AI_DAEMON_CONTROL_SOCKET")
        .env_remove("GIT_AI_DAEMON_TRACE_SOCKET")
        .env("GIT_AI_DAEMON_LOG_UPLOAD", "0")
        .env("CURSOR_SANDBOX", "native")
        .env_remove("GIT_AI_API_KEY")
        .env_remove("GIT_AI_TEST_CONFIG_PATCH")
        .output()
        .expect("run git-ai bg start");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemon startup refused"),
        "stderr should include refusal message, got: {}",
        stderr
    );
    assert!(
        stderr.contains("Cursor security sandbox"),
        "stderr should identify Cursor sandbox, got: {}",
        stderr
    );
    assert!(
        stderr.contains("GIT_AI_DAEMON_HOME"),
        "stderr should include mitigation, got: {}",
        stderr
    );
}
