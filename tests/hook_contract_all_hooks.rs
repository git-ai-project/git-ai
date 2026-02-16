mod repos;

use git_ai::commands::core_hooks::INSTALLED_HOOKS;
use repos::ecosystem::{EcosystemTestbed, assert_all_installed_hooks_present, shell_escape};
use std::fs;

fn previous_hook_script_with_marker(marker: &std::path::Path, exit_code: i32) -> String {
    let escaped = shell_escape(marker);
    format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"$(basename -- $0)\" \"$*\" >> \"{}\"\nexit {}\n",
        escaped, exit_code
    )
}

#[test]
fn hook_contract_all_installed_hooks_chain_success() {
    let testbed = EcosystemTestbed::new("hook-contract-all-chain-success");
    let previous_hooks_dir = testbed.root.join("previous-hooks");
    let marker = testbed.root.join("hook-chain-success.log");

    for hook in INSTALLED_HOOKS {
        testbed.write_hook_script(
            &previous_hooks_dir,
            hook,
            &previous_hook_script_with_marker(&marker, 0),
        );
    }

    testbed.set_global_hooks_path_raw(previous_hooks_dir.to_string_lossy().as_ref());
    testbed.install_hooks();
    assert_all_installed_hooks_present(&testbed);

    for hook in INSTALLED_HOOKS {
        let output = testbed.run_hook_script(hook, &["arg-one", "arg-two"], None, true, hook);
        assert!(
            output.status.success(),
            "{} hook should succeed via chained previous hook",
            hook
        );
    }

    let marker_text = fs::read_to_string(&marker).expect("read marker");
    for hook in INSTALLED_HOOKS {
        assert!(
            marker_text.contains(hook),
            "expected chained marker line for hook {}",
            hook
        );
    }
}

#[test]
fn hook_contract_all_installed_hooks_propagate_failures() {
    let testbed = EcosystemTestbed::new("hook-contract-all-propagate-failure");
    let previous_hooks_dir = testbed.root.join("previous-hooks");

    for hook in INSTALLED_HOOKS {
        testbed.write_hook_script(&previous_hooks_dir, hook, "#!/bin/sh\nexit 23\n");
    }

    testbed.set_global_hooks_path_raw(previous_hooks_dir.to_string_lossy().as_ref());
    testbed.install_hooks();

    for hook in INSTALLED_HOOKS {
        let output = testbed.run_hook_script(hook, &[], None, false, hook);
        assert_eq!(
            output.status.code(),
            Some(23),
            "{} hook should propagate previous hook failure",
            hook
        );
    }
}

#[test]
fn hook_contract_forwards_args_and_stdin_for_streamed_hooks() {
    let testbed = EcosystemTestbed::new("hook-contract-stdin-forwarding");
    let previous_hooks_dir = testbed.root.join("previous-hooks");
    let pre_push_marker = testbed.root.join("pre-push-forward.log");
    let reference_marker = testbed.root.join("reference-forward.log");
    let post_rewrite_marker = testbed.root.join("post-rewrite-forward.log");

    testbed.write_hook_script(
        &previous_hooks_dir,
        "pre-push",
        &format!(
            "#!/bin/sh\nIFS= read -r line\nprintf '%s|%s|%s\\n' \"$1\" \"$2\" \"$line\" >> \"{}\"\nexit 0\n",
            shell_escape(&pre_push_marker)
        ),
    );

    testbed.write_hook_script(
        &previous_hooks_dir,
        "reference-transaction",
        &format!(
            "#!/bin/sh\nIFS= read -r line\nprintf '%s|%s\\n' \"$1\" \"$line\" >> \"{}\"\nexit 0\n",
            shell_escape(&reference_marker)
        ),
    );

    testbed.write_hook_script(
        &previous_hooks_dir,
        "post-rewrite",
        &format!(
            "#!/bin/sh\nIFS= read -r line\nprintf '%s|%s\\n' \"$1\" \"$line\" >> \"{}\"\nexit 0\n",
            shell_escape(&post_rewrite_marker)
        ),
    );

    for hook in INSTALLED_HOOKS {
        if *hook != "pre-push" && *hook != "reference-transaction" && *hook != "post-rewrite" {
            testbed.write_hook_script(&previous_hooks_dir, hook, "#!/bin/sh\nexit 0\n");
        }
    }

    testbed.set_global_hooks_path_raw(previous_hooks_dir.to_string_lossy().as_ref());
    testbed.install_hooks();

    testbed.run_hook_script(
        "pre-push",
        &["origin", "https://example.invalid/repo.git"],
        Some("refs/heads/main 123 refs/heads/main 000\n"),
        true,
        "pre-push-forwarding",
    );

    let pre_push_line = fs::read_to_string(&pre_push_marker).expect("read pre-push marker");
    assert!(pre_push_line.contains("origin|https://example.invalid/repo.git|refs/heads/main"));

    testbed.run_hook_script(
        "reference-transaction",
        &["prepared"],
        Some("000 111 refs/heads/main\n"),
        true,
        "reference-forwarding",
    );

    let reference_line = fs::read_to_string(&reference_marker).expect("read reference marker");
    assert!(reference_line.contains("prepared|000 111 refs/heads/main"));

    testbed.run_hook_script(
        "post-rewrite",
        &["amend"],
        Some("oldsha newsha\n"),
        true,
        "post-rewrite-forwarding",
    );

    let post_rewrite_line =
        fs::read_to_string(&post_rewrite_marker).expect("read post-rewrite marker");
    assert!(post_rewrite_line.contains("amend|oldsha newsha"));
}

#[test]
fn hook_contract_handles_self_reference_without_recursion() {
    let testbed = EcosystemTestbed::new("hook-contract-self-reference");
    let repo_marker = testbed.root.join("repo-hook-ran");

    testbed.install_hooks();
    testbed.write_repo_hook("post-commit", &testbed.marker_hook_script(&repo_marker, 0));
    fs::write(
        testbed.previous_hooks_file(),
        format!("{}/\n", testbed.managed_hooks_dir().display()),
    )
    .expect("set self-referential previous hooks path");

    let output =
        testbed.run_hook_script("post-commit", &[], None, true, "post-commit-self-reference");
    assert!(
        output.status.success(),
        "post-commit should not recurse or hang"
    );
    assert!(
        repo_marker.exists(),
        "repo hook should execute as fallback path"
    );
}

#[test]
fn hook_contract_path_edge_cases_and_local_override_behavior() {
    let testbed = EcosystemTestbed::new("hook-contract-path-edge-cases");

    let previous_with_spaces = testbed.root.join("previous hooks with spaces");
    let marker_spaces = testbed.root.join("space-path.log");
    testbed.write_hook_script(
        &previous_with_spaces,
        "post-commit",
        &testbed.marker_hook_script(&marker_spaces, 0),
    );

    for hook in INSTALLED_HOOKS {
        if *hook != "post-commit" {
            testbed.write_hook_script(&previous_with_spaces, hook, "#!/bin/sh\nexit 0\n");
        }
    }

    testbed.set_global_hooks_path_raw(previous_with_spaces.to_string_lossy().as_ref());
    testbed.install_hooks();
    testbed.run_hook_script("post-commit", &[], None, true, "spaces-path");
    assert!(
        marker_spaces.exists(),
        "chaining should work for spaced paths"
    );

    let tilde_hooks = testbed.home.join("legacy-hooks");
    let tilde_marker = testbed.root.join("tilde-path.log");
    testbed.write_hook_script(
        &tilde_hooks,
        "post-commit",
        &testbed.marker_hook_script(&tilde_marker, 0),
    );
    for hook in INSTALLED_HOOKS {
        if *hook != "post-commit" {
            testbed.write_hook_script(&tilde_hooks, hook, "#!/bin/sh\nexit 0\n");
        }
    }

    testbed.set_global_hooks_path_raw("~/legacy-hooks");
    testbed.install_hooks();
    testbed.run_hook_script("post-commit", &[], None, true, "tilde-path");
    assert!(
        tilde_marker.exists(),
        "tilde paths should be expanded for chaining"
    );

    let local_hooks_dir = testbed.repo.join(".local-hooks");
    let local_marker = testbed.root.join("local-post-commit.log");
    testbed.write_hook_script(
        &local_hooks_dir,
        "post-commit",
        &testbed.marker_hook_script(&local_marker, 0),
    );

    testbed.set_local_hooks_path_raw(".local-hooks");
    let global_marker_before = fs::read_to_string(&tilde_marker).unwrap_or_default();
    testbed.commit_file("local-override.txt", "content\n", "local override commit");
    let global_marker_after = fs::read_to_string(&tilde_marker).unwrap_or_default();

    assert!(
        local_marker.exists(),
        "repo-local core.hooksPath should override managed global hooks"
    );
    assert!(
        global_marker_after == global_marker_before,
        "global managed hook chain should not run when local core.hooksPath is active"
    );
}

#[test]
fn hook_contract_backslash_previous_hooks_path_chains_on_shell_hooks() {
    let testbed = EcosystemTestbed::new("hook-contract-backslash-previous-hooks-path");
    let previous_hooks_dir = testbed.root.join("previous-hooks");
    let applypatch_marker = testbed.root.join("applypatch-backslash.log");
    let reference_marker = testbed.root.join("reference-backslash.log");

    testbed.write_hook_script(
        &previous_hooks_dir,
        "applypatch-msg",
        &testbed.marker_hook_script(&applypatch_marker, 0),
    );
    testbed.write_hook_script(
        &previous_hooks_dir,
        "reference-transaction",
        &format!(
            "#!/bin/sh\nIFS= read -r line\nprintf '%s|%s\\n' \"$1\" \"$line\" >> \"{}\"\nexit 0\n",
            shell_escape(&reference_marker)
        ),
    );

    for hook in INSTALLED_HOOKS {
        if *hook != "applypatch-msg" && *hook != "reference-transaction" {
            testbed.write_hook_script(&previous_hooks_dir, hook, "#!/bin/sh\nexit 0\n");
        }
    }

    testbed.set_global_hooks_path_raw(previous_hooks_dir.to_string_lossy().as_ref());
    testbed.install_hooks();

    let backslash_previous_hooks = previous_hooks_dir.to_string_lossy().replace('/', "\\");
    fs::write(testbed.previous_hooks_file(), backslash_previous_hooks)
        .expect("write backslash previous_hooks_path");

    testbed.run_hook_script(
        "applypatch-msg",
        &["foo"],
        None,
        true,
        "applypatch-backslash",
    );
    testbed.run_hook_script(
        "reference-transaction",
        &["prepared"],
        Some("000 111 refs/heads/main\n"),
        true,
        "reference-backslash",
    );

    assert!(
        applypatch_marker.exists(),
        "applypatch previous hook should run"
    );
    let reference_line = fs::read_to_string(reference_marker).expect("read reference marker");
    assert!(
        reference_line.contains("prepared|000 111 refs/heads/main"),
        "reference-transaction previous hook should receive args and stdin"
    );
}

#[test]
fn hook_contract_streamed_pre_push_propagates_chained_failure_with_large_stdin() {
    let testbed = EcosystemTestbed::new("hook-contract-streamed-pre-push-failure");
    let previous_hooks_dir = testbed.root.join("previous-hooks");

    testbed.write_hook_script(&previous_hooks_dir, "pre-push", "#!/bin/sh\nexit 23\n");
    for hook in INSTALLED_HOOKS {
        if *hook != "pre-push" {
            testbed.write_hook_script(&previous_hooks_dir, hook, "#!/bin/sh\nexit 0\n");
        }
    }

    testbed.set_global_hooks_path_raw(previous_hooks_dir.to_string_lossy().as_ref());
    testbed.install_hooks();

    let large_stdin = "refs/heads/main 123 refs/heads/main 000\n".repeat(20_000);
    let output = testbed.run_hook_script(
        "pre-push",
        &["origin", "https://example.invalid/repo.git"],
        Some(&large_stdin),
        false,
        "pre-push-large-stdin-failure",
    );
    assert_eq!(
        output.status.code(),
        Some(23),
        "pre-push should propagate chained hook failure even with large streamed stdin"
    );
}
