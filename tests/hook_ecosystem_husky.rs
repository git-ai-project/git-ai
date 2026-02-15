mod repos;

use repos::ecosystem::EcosystemTestbed;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy)]
enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
}

impl PackageManager {
    fn label(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
        }
    }

    fn binary(self) -> &'static str {
        self.label()
    }
}

#[test]
fn husky_real_tooling_flows_are_compatible_with_git_ai_hooks() {
    let profile = std::env::var("GIT_AI_ECOSYSTEM_PROFILE")
        .unwrap_or_else(|_| "full".to_string())
        .to_ascii_lowercase();
    let managers = if profile == "smoke" {
        vec![PackageManager::Npm]
    } else {
        vec![
            PackageManager::Npm,
            PackageManager::Pnpm,
            PackageManager::Yarn,
        ]
    };

    for manager in managers {
        let testbed = EcosystemTestbed::new(&format!("husky-real-tool-{}", manager.label()));
        if !testbed.require_tool("node") {
            return;
        }
        if !testbed.require_tool(manager.binary()) {
            continue;
        }

        run_husky_flow_for_manager(&testbed, manager);
    }
}

fn run_husky_flow_for_manager(testbed: &EcosystemTestbed, manager: PackageManager) {
    testbed.install_hooks();
    setup_husky(testbed, manager);
    install_husky_scripts(testbed);

    // Verify local hooks path is set to .husky after real husky setup.
    let local_hooks = testbed.local_hooks_path().unwrap_or_default();
    assert!(
        local_hooks == ".husky" || local_hooks == ".husky/_",
        "husky should configure local core.hooksPath, got '{}'",
        local_hooks
    );

    // Regular commit from repo root.
    testbed.write_file("src/main.ts", "console.log('root commit');\n");
    testbed.run_git_ok(&["add", "src/main.ts"], "husky add root");
    testbed.run_git_ok(&["commit", "-m", "husky root commit"], "husky root commit");

    // Commit from nested subdirectory.
    let nested = testbed.repo.join("packages/app");
    fs::create_dir_all(&nested).expect("create nested directory");
    testbed.write_file("packages/app/index.ts", "export const nested = 1;\n");
    testbed.run_git_in_dir_ok(&nested, &["add", "index.ts"], "husky nested add");
    testbed.run_git_in_dir_ok(
        &nested,
        &["commit", "-m", "husky nested commit"],
        "husky nested commit",
    );

    // Blocking behavior: pre-commit failure must block commit.
    set_husky_hook_script(
        testbed,
        "pre-commit",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-pre-commit-blocked >> .hook-log\nexit 19\n",
    );

    testbed.write_file("blocked.txt", "blocked\n");
    testbed.run_git_ok(&["add", "blocked.txt"], "husky blocked add");
    let blocked = testbed.run_git_expect_failure(
        &["commit", "-m", "blocked by husky"],
        "husky blocked commit",
    );
    assert!(
        !blocked.status.success(),
        "failing husky pre-commit should block commit"
    );

    // Restore passing pre-commit.
    set_husky_hook_script(
        testbed,
        "pre-commit",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-pre-commit >> .hook-log\nexit 0\n",
    );

    testbed.write_file("allowed.txt", "allowed\n");
    testbed.run_git_ok(&["add", "allowed.txt"], "husky allowed add");
    testbed.run_git_ok(
        &["commit", "-m", "allowed by husky"],
        "husky allowed commit",
    );

    // Amend flow.
    testbed.write_file("amend.txt", "amended\n");
    testbed.run_git_ok(&["add", "amend.txt"], "husky amend add");
    testbed.run_git_ok(
        &["commit", "--amend", "-m", "husky amend"],
        "husky amend commit",
    );

    // Rebase flow.
    let default_branch = testbed.current_branch();
    testbed.run_git_ok(
        &["checkout", "-b", "feature-rebase"],
        "create feature branch",
    );
    testbed.write_file("feature.txt", "feature\n");
    testbed.run_git_ok(&["add", "feature.txt"], "feature add");
    testbed.run_git_ok(&["commit", "-m", "feature commit"], "feature commit");

    testbed.run_git_ok(
        &["checkout", &default_branch],
        "checkout default after feature",
    );
    testbed.write_file("main.txt", "main update\n");
    testbed.run_git_ok(&["add", "main.txt"], "main add for rebase");
    testbed.run_git_ok(&["commit", "-m", "main update"], "main update commit");

    testbed.run_git_ok(
        &["checkout", "feature-rebase"],
        "checkout feature for rebase",
    );
    testbed.run_git_ok(&["rebase", &default_branch], "run rebase");

    // Cherry-pick flow.
    testbed.run_git_ok(
        &["checkout", &default_branch],
        "checkout default for cherry-pick",
    );
    testbed.run_git_ok(&["checkout", "-b", "source-branch"], "create source branch");
    testbed.write_file("source.txt", "source\n");
    testbed.run_git_ok(&["add", "source.txt"], "source add");
    testbed.run_git_ok(&["commit", "-m", "source commit"], "source commit");
    let source_sha = testbed
        .run_git_ok(&["rev-parse", "HEAD"], "source sha")
        .trim()
        .to_string();

    testbed.run_git_ok(&["checkout", &default_branch], "checkout default for pick");
    testbed.run_git_ok(&["cherry-pick", &source_sha], "cherry-pick source");

    // Merge flow.
    testbed.run_git_ok(&["checkout", "-b", "merge-branch"], "create merge branch");
    testbed.write_file("merge.txt", "merge\n");
    testbed.run_git_ok(&["add", "merge.txt"], "merge add");
    testbed.run_git_ok(
        &["commit", "-m", "merge source commit"],
        "merge source commit",
    );

    testbed.run_git_ok(&["checkout", &default_branch], "checkout default for merge");
    testbed.run_git_ok(
        &["merge", "--no-ff", "merge-branch", "-m", "merge branch"],
        "merge --no-ff",
    );

    // Push flow.
    let remote_path = testbed.init_bare_remote(&format!("husky-remote-{}", manager.label()));
    testbed.add_remote_origin(&remote_path);
    testbed.push_head_to_origin();

    let log_lines = testbed.hook_log_lines();
    assert_contains_line_prefix(&log_lines, "husky-pre-commit");
    assert_contains_line_prefix(&log_lines, "husky-commit-msg");
    assert_contains_line_prefix(&log_lines, "husky-pre-rebase");
    assert_contains_line_prefix(&log_lines, "husky-pre-push|origin|");
}

fn setup_husky(testbed: &EcosystemTestbed, manager: PackageManager) {
    match manager {
        PackageManager::Npm => {
            testbed.run_cmd_ok("npm", &["init", "-y"], Some(&testbed.repo), &[], "npm init");
            testbed.run_cmd_ok(
                "npm",
                &["install", "--save-dev", "husky@9"],
                Some(&testbed.repo),
                &[],
                "npm install husky",
            );
            testbed.run_cmd_ok(
                "npm",
                &["exec", "husky", "init"],
                Some(&testbed.repo),
                &[],
                "npm husky init",
            );
        }
        PackageManager::Pnpm => {
            testbed.run_cmd_ok(
                "pnpm",
                &["init", "--yes"],
                Some(&testbed.repo),
                &[],
                "pnpm init",
            );
            testbed.run_cmd_ok(
                "pnpm",
                &["add", "-D", "husky@9"],
                Some(&testbed.repo),
                &[],
                "pnpm add husky",
            );
            testbed.run_cmd_ok(
                "pnpm",
                &["exec", "husky", "init"],
                Some(&testbed.repo),
                &[],
                "pnpm husky init",
            );
        }
        PackageManager::Yarn => {
            testbed.run_cmd_ok(
                "yarn",
                &["init", "-y"],
                Some(&testbed.repo),
                &[],
                "yarn init",
            );
            testbed.run_cmd_ok(
                "yarn",
                &["add", "-D", "husky@9"],
                Some(&testbed.repo),
                &[],
                "yarn add husky",
            );
            testbed.run_cmd_ok(
                "yarn",
                &["husky", "init"],
                Some(&testbed.repo),
                &[],
                "yarn husky init",
            );
        }
    }
}

fn install_husky_scripts(testbed: &EcosystemTestbed) {
    set_husky_hook_script(
        testbed,
        "pre-commit",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-pre-commit >> .hook-log\nexit 0\n",
    );
    set_husky_hook_script(
        testbed,
        "commit-msg",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-commit-msg >> .hook-log\nexit 0\n",
    );
    set_husky_hook_script(
        testbed,
        "pre-rebase",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-pre-rebase >> .hook-log\nexit 0\n",
    );
    set_husky_hook_script(
        testbed,
        "pre-push",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nIFS= read -r line\nprintf '%s|%s|%s\\n' husky-pre-push \"$1\" \"$line\" >> .hook-log\nexit 0\n",
    );
}

fn set_husky_hook_script(testbed: &EcosystemTestbed, hook_name: &str, script: &str) {
    let hook_path = testbed.repo.join(".husky").join(hook_name);
    if let Some(parent) = hook_path.parent() {
        fs::create_dir_all(parent).expect("create husky hook parent");
    }
    fs::write(&hook_path, script).expect("write husky hook");
    make_executable(&hook_path);
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("hook metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("set executable");
    }
}

fn assert_contains_line_prefix(lines: &[String], expected_prefix: &str) {
    assert!(
        lines.iter().any(|line| line.starts_with(expected_prefix)),
        "hook log did not contain prefix '{}' in {:?}",
        expected_prefix,
        lines
    );
}
