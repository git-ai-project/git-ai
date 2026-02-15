mod repos;

use git_ai::config::Config;
use repos::ecosystem::EcosystemTestbed;

#[test]
fn pre_commit_ci_style_environment_is_compatible_with_git_ai_hooks() {
    let testbed = EcosystemTestbed::new("pre-commit-ci-style");

    if !testbed.require_tool("python3") {
        return;
    }
    if !testbed.require_tool("pre-commit") {
        return;
    }

    testbed.install_hooks();

    testbed.write_file(
        "scripts/ci-hook.sh",
        "#!/bin/sh\nset -eu\nstage=\"$1\"\nshift || true\nprintf '%s|CI=%s\\n' \"$stage\" \"${CI:-unset}\" >> .hook-log\n",
    );

    #[cfg(unix)]
    {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let path = testbed.repo.join("scripts/ci-hook.sh");
        let mut perms = fs::metadata(&path).expect("ci hook metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("set ci hook executable");
    }

    testbed.write_file(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: local\n    hooks:\n      - id: ci-pre-commit\n        name: ci-pre-commit\n        entry: scripts/ci-hook.sh pre-commit\n        language: system\n        pass_filenames: false\n        stages: [pre-commit]\n      - id: ci-commit-msg\n        name: ci-commit-msg\n        entry: scripts/ci-hook.sh commit-msg\n        language: system\n        stages: [commit-msg]\n",
    );

    let pre_commit_home = testbed.root.join("ci-pre-commit-home");
    let xdg_cache_home = testbed.root.join("ci-xdg-cache");
    let pre_commit_home_owned = pre_commit_home.to_string_lossy().to_string();
    let xdg_cache_home_owned = xdg_cache_home.to_string_lossy().to_string();

    let ci_env = [
        ("CI", "true"),
        ("PRE_COMMIT_HOME", pre_commit_home_owned.as_str()),
        ("XDG_CACHE_HOME", xdg_cache_home_owned.as_str()),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
    ];

    testbed.run_cmd_ok(
        "pre-commit",
        &[
            "install",
            "--hook-type",
            "pre-commit",
            "--hook-type",
            "commit-msg",
        ],
        Some(&testbed.repo),
        &ci_env,
        "pre-commit install ci-style",
    );

    testbed.write_file("ci-style.txt", "ci style\n");
    testbed.run_cmd_ok(
        Config::get().git_cmd(),
        &["add", "ci-style.txt"],
        Some(&testbed.repo),
        &ci_env,
        "ci-style add",
    );
    testbed.run_cmd_ok(
        Config::get().git_cmd(),
        &["commit", "-m", "ci style commit"],
        Some(&testbed.repo),
        &ci_env,
        "ci-style commit",
    );

    testbed.run_cmd_ok(
        "pre-commit",
        &["run", "--all-files"],
        Some(&testbed.repo),
        &ci_env,
        "pre-commit run all files ci-style",
    );

    assert!(
        pre_commit_home.exists(),
        "pre-commit home should be created"
    );
    let lines = testbed.hook_log_lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("pre-commit|CI=true")),
        "expected pre-commit hook log in CI mode, got {:?}",
        lines
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("commit-msg|CI=true")),
        "expected commit-msg hook log in CI mode, got {:?}",
        lines
    );
}
