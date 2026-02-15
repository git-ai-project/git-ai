mod repos;

use repos::ecosystem::EcosystemTestbed;
use std::fs;

#[test]
fn lefthook_real_tooling_flows_are_compatible_with_git_ai_hooks() {
    let testbed = EcosystemTestbed::new("lefthook-real-tool-flows");

    if !testbed.require_tool("node") {
        return;
    }
    if !testbed.require_tool("npm") {
        return;
    }

    testbed.install_hooks();
    setup_lefthook(&testbed);

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

fn setup_lefthook(testbed: &EcosystemTestbed) {
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
        "lefthook.yml",
        "pre-commit:\n  commands:\n    marker:\n      run: sh -c \"if [ -f .block-lefthook ]; then printf '%s\\n' lefthook-pre-commit-blocked >> .hook-log; exit 17; fi; printf '%s\\n' lefthook-pre-commit >> .hook-log\"\ncommit-msg:\n  commands:\n    marker:\n      run: sh -c \"printf '%s\\n' lefthook-commit-msg >> .hook-log\"\npre-push:\n  commands:\n    marker:\n      run: sh -c \"printf '%s\\n' lefthook-pre-push >> .hook-log\"\n",
    );

    testbed.run_cmd_ok(
        "npx",
        &["lefthook", "install"],
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
