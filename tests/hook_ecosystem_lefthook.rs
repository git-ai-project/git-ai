mod repos;

use repos::ecosystem::EcosystemTestbed;
use std::fs;

#[test]
fn lefthook_real_tooling_flows_are_compatible_with_git_ai_hooks() {
    let testbed = EcosystemTestbed::new("lefthook-real-tool-flows");
    let Some(python) = resolve_python(&testbed) else {
        return;
    };

    if !testbed.require_tool("node") {
        return;
    }
    if !testbed.require_tool("npm") {
        return;
    }

    testbed.install_hooks();
    setup_lefthook(&testbed, python);

    // Root commit.
    testbed.write_file("lefthook-root.txt", "root\n");
    testbed.run_git_ok(&["add", "lefthook-root.txt"], "lefthook add root");
    testbed.run_git_ok(
        &["commit", "-m", "lefthook root commit"],
        "lefthook root commit",
    );

    // Monorepo-style nested commit.
    let nested = testbed.repo.join("apps/web");
    fs::create_dir_all(&nested).expect("create monorepo nested dir");
    testbed.write_file("apps/web/app.ts", "export const app = true;\n");
    testbed.run_git_in_dir_ok(&nested, &["add", "app.ts"], "lefthook nested add");
    testbed.run_git_in_dir_ok(
        &nested,
        &["commit", "-m", "lefthook nested commit"],
        "lefthook nested commit",
    );

    // Blocking behavior.
    testbed.write_file(".block-lefthook", "1\n");
    testbed.write_file("blocked-lefthook.txt", "blocked\n");
    testbed.run_git_ok(&["add", "blocked-lefthook.txt"], "lefthook blocked add");
    let blocked = testbed.run_git_expect_failure(
        &["commit", "-m", "lefthook blocked commit"],
        "lefthook blocked commit",
    );
    assert!(
        !blocked.status.success(),
        "lefthook pre-commit should block failing commit"
    );
    fs::remove_file(testbed.repo.join(".block-lefthook")).expect("remove lefthook block");

    testbed.write_file("allowed-lefthook.txt", "allowed\n");
    testbed.run_git_ok(&["add", "allowed-lefthook.txt"], "lefthook allowed add");
    testbed.run_git_ok(
        &["commit", "-m", "lefthook allowed commit"],
        "lefthook allowed commit",
    );

    // Push flow.
    let remote = testbed.init_bare_remote("lefthook-remote");
    testbed.add_remote_origin(&remote);
    testbed.push_head_to_origin();

    let lines = testbed.hook_log_lines();
    assert_contains_prefix(&lines, "lefthook-pre-commit");
    assert_contains_prefix(&lines, "lefthook-commit-msg");
    assert_contains_prefix(&lines, "lefthook-pre-push");
}

fn setup_lefthook(testbed: &EcosystemTestbed, python: &str) {
    testbed.run_cmd_ok(
        "npm",
        &["init", "-y"],
        Some(&testbed.repo),
        &[],
        "lefthook npm init",
    );
    testbed.run_cmd_ok(
        "npm",
        &["install", "--save-dev", "lefthook@1"],
        Some(&testbed.repo),
        &[],
        "lefthook install package",
    );

    testbed.write_file(
        "scripts/lefthook_marker.py",
        "import os\nimport sys\n\nstage = sys.argv[1]\nif stage == \"pre-commit\" and os.path.exists(\".block-lefthook\"):\n    with open(\".hook-log\", \"a\", encoding=\"utf-8\") as f:\n        f.write(\"lefthook-pre-commit-blocked\\n\")\n    raise SystemExit(17)\n\nwith open(\".hook-log\", \"a\", encoding=\"utf-8\") as f:\n    f.write(f\"lefthook-{stage}\\n\")\n",
    );

    let config = format!(
        "pre-commit:\n  commands:\n    marker:\n      run: {} scripts/lefthook_marker.py pre-commit\ncommit-msg:\n  commands:\n    marker:\n      run: {} scripts/lefthook_marker.py commit-msg\npre-push:\n  commands:\n    marker:\n      run: {} scripts/lefthook_marker.py pre-push\n",
        python, python, python
    );
    testbed.write_file("lefthook.yml", &config);

    testbed.run_cmd_ok(
        "npm",
        &["exec", "--", "lefthook", "install"],
        Some(&testbed.repo),
        &[],
        "lefthook install hooks",
    );
}

fn assert_contains_prefix(lines: &[String], prefix: &str) {
    assert!(
        lines.iter().any(|line| line.starts_with(prefix)),
        "hook log missing prefix '{}' in {:?}",
        prefix,
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
