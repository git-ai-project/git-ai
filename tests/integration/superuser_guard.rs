use crate::repos::test_repo::TestRepo;
#[cfg(unix)]
use crate::repos::test_repo::get_binary_path;
#[cfg(unix)]
use std::process::Command;

#[test]
fn superuser_guard_does_not_block_non_root_invocations() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["version"]);
    assert!(result.is_ok(), "version command should always succeed");
}

#[test]
fn superuser_guard_allow_env_var_is_respected() {
    let repo = TestRepo::new();
    let result = repo.git_ai_with_env(&["version"], &[("GIT_AI_ALLOW_SUPERUSER", "1")]);
    assert!(
        result.is_ok(),
        "version should succeed with GIT_AI_ALLOW_SUPERUSER=1"
    );
}

#[test]
fn superuser_guard_exempt_commands_always_work() {
    let repo = TestRepo::new();
    for cmd in ["version", "--version", "-v", "help", "--help", "-h"] {
        let result = repo.git_ai(&[cmd]);
        assert!(
            result.is_ok(),
            "{cmd} should be exempt from superuser guard"
        );
    }
}

#[test]
#[cfg(unix)]
fn superuser_guard_warns_when_running_as_root_without_opt_in() {
    if unsafe { libc::geteuid() } != 0 {
        // Can't test warning behavior as non-root; skip.
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"]).env_remove("GIT_AI_ALLOW_SUPERUSER");
    remove_all_ci_env_vars(&mut cmd);
    let output = cmd.output().expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("running as superuser (root/Administrator) is not recommended"),
        "should show warning when running as root without opt-in, got: {stderr}"
    );
}

#[cfg(unix)]
fn remove_all_ci_env_vars(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("GITLAB_CI")
        .env_remove("JENKINS_URL")
        .env_remove("BUILDKITE")
        .env_remove("CIRCLECI")
        .env_remove("CODEBUILD_BUILD_ID")
        .env_remove("AGENT_OS")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("GIT_AI_DAEMON_UPGRADE")
        .env_remove("container")
}

#[test]
#[cfg(unix)]
fn superuser_guard_allows_root_with_env_var_opt_in() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"]).env("GIT_AI_ALLOW_SUPERUSER", "1");
    remove_all_ci_env_vars(&mut cmd);
    let output = cmd.output().expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("is not recommended"),
        "should NOT show warning when GIT_AI_ALLOW_SUPERUSER=1 is set, got: {stderr}"
    );
    assert!(
        stderr.contains("warning: running as superuser (GIT_AI_ALLOW_SUPERUSER is set)"),
        "should show opt-in acknowledgment when running as root with GIT_AI_ALLOW_SUPERUSER, got: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn superuser_guard_allows_root_in_ci_environment() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"])
        .env("CI", "true")
        .env_remove("GIT_AI_ALLOW_SUPERUSER");
    remove_all_ci_env_vars(&mut cmd);
    // Re-add just CI=true (remove_all_ci_env_vars removes it)
    cmd.env("CI", "true");
    let output = cmd.output().expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("is not recommended"),
        "should NOT show warning in CI environment, got: {stderr}"
    );
    assert!(
        !stderr.contains("warning: running as superuser"),
        "should NOT warn in CI environment (silent pass), got: {stderr}"
    );
}
