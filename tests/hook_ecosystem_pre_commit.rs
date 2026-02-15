mod repos;

use repos::ecosystem::EcosystemTestbed;
use std::fs;

#[test]
fn pre_commit_real_tooling_flows_are_compatible_with_git_ai_hooks() {
    let testbed = EcosystemTestbed::new("pre-commit-real-tool-flows");

    if !testbed.require_tool("python3") {
        return;
    }
    if !testbed.require_tool("pre-commit") {
        return;
    }

    write_pre_commit_scripts_and_config(&testbed);

    testbed.run_cmd_ok(
        "pre-commit",
        &[
            "install",
            "--hook-type",
            "pre-commit",
            "--hook-type",
            "commit-msg",
            "--hook-type",
            "pre-push",
            "--hook-type",
            "pre-rebase",
        ],
        Some(&testbed.repo),
        &[],
        "pre-commit install",
    );
    testbed.install_hooks();

    // Commit from repo root.
    testbed.write_file("pc-root.txt", "root\n");
    testbed.run_git_ok(&["add", "pc-root.txt"], "pre-commit root add");
    testbed.run_git_ok(
        &["commit", "-m", "pre-commit root commit"],
        "pre-commit root commit",
    );

    // Commit from nested directory.
    let nested = testbed.repo.join("sub/dir");
    fs::create_dir_all(&nested).expect("create nested dir");
    testbed.write_file("sub/dir/nested.txt", "nested\n");
    testbed.run_git_in_dir_ok(&nested, &["add", "nested.txt"], "nested add");
    testbed.run_git_in_dir_ok(
        &nested,
        &["commit", "-m", "pre-commit nested commit"],
        "nested commit",
    );

    // Blocking behavior.
    testbed.write_file(".block-precommit", "1\n");
    testbed.write_file("blocked-pre-commit.txt", "blocked\n");
    testbed.run_git_ok(&["add", "blocked-pre-commit.txt"], "blocked add");
    let blocked = testbed.run_git_expect_failure(
        &["commit", "-m", "blocked pre-commit"],
        "blocked pre-commit",
    );
    assert!(
        !blocked.status.success(),
        "failing pre-commit hook must block commit",
    );
    fs::remove_file(testbed.repo.join(".block-precommit")).expect("remove block file");

    testbed.write_file("allowed-pre-commit.txt", "allowed\n");
    testbed.run_git_ok(&["add", "allowed-pre-commit.txt"], "allowed add");
    testbed.run_git_ok(&["commit", "-m", "allowed commit"], "allowed commit");

    // Rebase flow.
    let default_branch = testbed.current_branch();
    testbed.run_git_ok(&["checkout", "-b", "pc-feature"], "create pc feature");
    testbed.write_file("pc-feature.txt", "feature\n");
    testbed.run_git_ok(&["add", "pc-feature.txt"], "feature add");
    testbed.run_git_ok(&["commit", "-m", "feature commit"], "feature commit");

    testbed.run_git_ok(&["checkout", &default_branch], "checkout default");
    testbed.write_file("pc-main.txt", "main\n");
    testbed.run_git_ok(&["add", "pc-main.txt"], "main add");
    testbed.run_git_ok(&["commit", "-m", "main commit"], "main commit");

    testbed.run_git_ok(&["checkout", "pc-feature"], "checkout feature");
    testbed.run_git_ok(&["rebase", &default_branch], "rebase feature");

    // Push flow.
    testbed.run_git_ok(&["checkout", &default_branch], "checkout default for push");
    let remote_path = testbed.init_bare_remote("pre-commit-remote");
    testbed.add_remote_origin(&remote_path);
    testbed.push_head_to_origin();

    let log_lines = testbed.hook_log_lines();
    assert_contains_prefix(&log_lines, "pre-commit");
    assert_contains_prefix(&log_lines, "commit-msg|");
    assert_contains_prefix(&log_lines, "pre-rebase|");
    assert_contains_prefix(&log_lines, "pre-push|");
}

fn write_pre_commit_scripts_and_config(testbed: &EcosystemTestbed) {
    testbed.write_file(
        "scripts/log-hook.sh",
        "#!/bin/sh\nset -eu\nstage=\"$1\"\nshift || true\nif [ \"$stage\" = \"pre-commit\" ] && [ -f .block-precommit ]; then\n  printf '%s\\n' pre-commit-blocked >> .hook-log\n  exit 19\nfi\nif [ \"$stage\" = \"pre-push\" ]; then\n  remote_name=\"${1:-}\"\n  IFS= read -r line || true\n  printf '%s|%s|%s\\n' pre-push \"$remote_name\" \"$line\" >> .hook-log\n  exit 0\nfi\nif [ \"$stage\" = \"commit-msg\" ]; then\n  msg_file=\"${1:-}\"\n  printf '%s|%s\\n' commit-msg \"$msg_file\" >> .hook-log\n  exit 0\nfi\nif [ \"$stage\" = \"pre-rebase\" ]; then\n  upstream=\"${1:-}\"\n  printf '%s|%s\\n' pre-rebase \"$upstream\" >> .hook-log\n  exit 0\nfi\nprintf '%s\\n' \"$stage\" >> .hook-log\n",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = testbed.repo.join("scripts/log-hook.sh");
        let mut perms = fs::metadata(&path)
            .expect("metadata for log hook")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("set log hook executable");
    }

    testbed.write_file(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: local\n    hooks:\n      - id: precommit-marker\n        name: precommit-marker\n        entry: scripts/log-hook.sh pre-commit\n        language: system\n        pass_filenames: false\n        stages: [pre-commit]\n      - id: commitmsg-marker\n        name: commitmsg-marker\n        entry: scripts/log-hook.sh commit-msg\n        language: system\n        stages: [commit-msg]\n      - id: prepush-marker\n        name: prepush-marker\n        entry: scripts/log-hook.sh pre-push\n        language: system\n        pass_filenames: false\n        stages: [pre-push]\n      - id: prerebase-marker\n        name: prerebase-marker\n        entry: scripts/log-hook.sh pre-rebase\n        language: system\n        pass_filenames: false\n        stages: [pre-rebase]\n",
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
