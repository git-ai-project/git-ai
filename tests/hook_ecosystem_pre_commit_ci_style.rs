mod repos;

use git_ai::config::Config;
use repos::ecosystem::EcosystemTestbed;

#[test]
fn pre_commit_ci_style_environment_is_compatible_with_git_ai_hooks() {
    let testbed = EcosystemTestbed::new("pre-commit-ci-style");

    let Some(python) = resolve_python(&testbed) else {
        return;
    };
    if !testbed.require_tool("pre-commit") {
        return;
    }

    testbed.write_file(
        "scripts/ci_hook.py",
        "import os\nimport sys\n\nstage = sys.argv[1]\nci = os.environ.get(\"CI\", \"unset\")\nwith open(\".hook-log\", \"a\", encoding=\"utf-8\") as f:\n    f.write(f\"{stage}|CI={ci}\\n\")\n",
    );

    let config = format!(
        "repos:\n  - repo: local\n    hooks:\n      - id: ci-pre-commit\n        name: ci-pre-commit\n        entry: {} scripts/ci_hook.py pre-commit\n        language: system\n        pass_filenames: false\n        stages: [pre-commit]\n      - id: ci-commit-msg\n        name: ci-commit-msg\n        entry: {} scripts/ci_hook.py commit-msg\n        language: system\n        stages: [commit-msg]\n",
        python, python
    );
    testbed.write_file(".pre-commit-config.yaml", &config);

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
    testbed.install_hooks();

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

fn resolve_python(testbed: &EcosystemTestbed) -> Option<&'static str> {
    if testbed.has_command("python3") {
        return Some("python3");
    }
    if testbed.has_command("python") {
        return Some("python");
    }
    if testbed.require_tool("python3") {
        return Some("python3");
    }
    None
}
