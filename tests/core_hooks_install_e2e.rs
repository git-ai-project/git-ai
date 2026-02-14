#![cfg(unix)]

mod repos;

use git_ai::commands::core_hooks::{INSTALLED_HOOKS, PREVIOUS_HOOKS_PATH_FILE};
use repos::test_repo::get_binary_path;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct HookConfigSandbox {
    temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    global_config: PathBuf,
    git_ai_bin: PathBuf,
}

impl HookConfigSandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(home.join(".config")).expect("create xdg config");
        fs::create_dir_all(&repo).expect("create repo");

        let sandbox = Self {
            temp,
            home: home.clone(),
            repo,
            global_config: home.join(".gitconfig"),
            git_ai_bin: get_binary_path().clone(),
        };

        sandbox.init_repo();
        sandbox
    }

    fn managed_hooks_dir(&self) -> PathBuf {
        self.home.join(".git-ai").join("core-hooks")
    }

    fn previous_hooks_file(&self) -> PathBuf {
        self.managed_hooks_dir().join(PREVIOUS_HOOKS_PATH_FILE)
    }

    fn init_repo(&self) {
        self.run_git_ok(&["init"]);
        self.run_git_ok(&["config", "user.name", "Test User"]);
        self.run_git_ok(&["config", "user.email", "test@example.com"]);
        self.write_repo_file("README.md", "init\n");
        self.run_git_ok(&["add", "README.md"]);
        self.run_git_ok(&["commit", "-m", "initial"]);
    }

    fn run_git_raw(&self, args: &[&str]) -> Output {
        let mut command = Command::new(git_ai::config::Config::get().git_cmd());
        command.args(args).current_dir(&self.repo);
        self.apply_env(&mut command);
        command.output().expect("run git")
    }

    fn run_git_ok(&self, args: &[&str]) -> String {
        let output = self.run_git_raw(args);
        if !output.status.success() {
            panic!(
                "git command failed: git {:?}\n{}",
                args,
                combined_output(&output)
            );
        }
        combined_output(&output)
    }

    fn run_git_ai_raw(&self, args: &[&str]) -> Output {
        let mut command = Command::new(&self.git_ai_bin);
        command.args(args).current_dir(&self.repo);
        self.apply_env(&mut command);
        command.output().expect("run git-ai")
    }

    fn run_git_ai_ok(&self, args: &[&str]) -> String {
        let output = self.run_git_ai_raw(args);
        if !output.status.success() {
            panic!(
                "git-ai command failed: git-ai {:?}\n{}",
                args,
                combined_output(&output)
            );
        }
        combined_output(&output)
    }

    fn apply_env(&self, command: &mut Command) {
        command.env("HOME", &self.home);
        command.env("USERPROFILE", &self.home);
        command.env("XDG_CONFIG_HOME", self.home.join(".config"));
        command.env("GIT_CONFIG_GLOBAL", &self.global_config);
        command.env("GIT_CONFIG_NOSYSTEM", "1");
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env_remove("GIT_DIR");
        command.env_remove("GIT_WORK_TREE");
    }

    fn install_hooks(&self) {
        self.run_git_ai_ok(&["install-hooks", "--dry-run=false"]);
    }

    fn uninstall_hooks(&self) {
        self.run_git_ai_ok(&["uninstall-hooks", "--dry-run=false"]);
    }

    fn global_hooks_path(&self) -> Option<String> {
        let output = self.run_git_raw(&["config", "--global", "--get", "core.hooksPath"]);
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        } else {
            None
        }
    }

    fn set_global_hooks_path(&self, hooks_dir: &Path) {
        self.run_git_ok(&[
            "config",
            "--global",
            "core.hooksPath",
            hooks_dir.to_str().expect("hooks dir path"),
        ]);
    }

    fn set_local_hooks_path(&self, hooks_dir: &Path) {
        self.run_git_ok(&[
            "config",
            "core.hooksPath",
            hooks_dir.to_str().expect("hooks dir path"),
        ]);
    }

    fn unset_local_hooks_path(&self) {
        self.run_git_ok(&["config", "--unset", "core.hooksPath"]);
    }

    fn write_repo_file(&self, relative_path: &str, content: &str) {
        let path = self.repo.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, content).expect("write repo file");
    }

    fn commit_file(&self, relative_path: &str, content: &str, message: &str) {
        self.write_repo_file(relative_path, content);
        self.run_git_ok(&["add", relative_path]);
        self.run_git_ok(&["commit", "-m", message]);
    }

    fn write_hook(&self, hooks_dir: &Path, hook_name: &str, script_body: &str) {
        fs::create_dir_all(hooks_dir).expect("create hooks dir");
        let hook_path = hooks_dir.join(hook_name);
        fs::write(&hook_path, script_body).expect("write hook script");

        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path)
            .expect("hook metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).expect("set hook permissions");
    }

    fn write_repo_hook(&self, hook_name: &str, script_body: &str) {
        let hooks_dir = self.repo.join(".git").join("hooks");
        self.write_hook(&hooks_dir, hook_name, script_body);
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("stdout:\n{}\nstderr:\n{}", stdout, stderr)
}

fn shell_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn marker_hook_script(marker: &Path, exit_code: i32) -> String {
    let escaped = shell_escape(marker);
    format!(
        "#!/bin/sh\nprintf '%s\\n' ran >> \"{}\"\nexit {}\n",
        escaped, exit_code
    )
}

#[test]
fn install_hooks_sets_managed_global_path_and_writes_scripts() {
    let sandbox = HookConfigSandbox::new();

    assert_eq!(sandbox.global_hooks_path(), None);
    sandbox.install_hooks();

    let managed_dir = sandbox.managed_hooks_dir();
    assert_eq!(
        sandbox.global_hooks_path(),
        Some(managed_dir.to_string_lossy().to_string())
    );

    let expected_binary = sandbox.git_ai_bin.to_string_lossy().replace('\\', "/");

    for hook in INSTALLED_HOOKS {
        let hook_path = managed_dir.join(hook);
        assert!(
            hook_path.exists(),
            "missing hook script: {}",
            hook_path.display()
        );

        let content = fs::read_to_string(&hook_path).expect("read hook script");
        assert!(
            content.contains(&format!("hook {}", hook)),
            "hook script missing dispatch for {}",
            hook
        );
        assert!(
            content.contains(&expected_binary),
            "hook script missing binary path"
        );
        assert!(
            content.contains(PREVIOUS_HOOKS_PATH_FILE),
            "hook script missing previous-hooks chaining logic"
        );
    }

    assert!(sandbox.previous_hooks_file().exists());
}

#[test]
fn install_uninstall_preserves_existing_global_hooks_path() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-global-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(sandbox.managed_hooks_dir().to_string_lossy().to_string())
    );
    assert_eq!(
        fs::read_to_string(sandbox.previous_hooks_file())
            .expect("read previous hooks file")
            .trim(),
        previous_hooks_dir.to_string_lossy()
    );

    sandbox.uninstall_hooks();
    assert_eq!(
        sandbox.global_hooks_path(),
        Some(previous_hooks_dir.to_string_lossy().to_string())
    );
    assert!(!sandbox.managed_hooks_dir().exists());
}

#[test]
fn reinstall_refreshes_previous_hooks_path_after_user_reconfigure() {
    let sandbox = HookConfigSandbox::new();
    let old_hooks_dir = sandbox.temp.path().join("old-hooks");
    let new_hooks_dir = sandbox.temp.path().join("new-hooks");
    fs::create_dir_all(&old_hooks_dir).expect("create old hooks dir");
    fs::create_dir_all(&new_hooks_dir).expect("create new hooks dir");

    sandbox.set_global_hooks_path(&old_hooks_dir);
    sandbox.install_hooks();

    // Simulate user reconfiguring global core.hooksPath while git-ai is installed.
    sandbox.set_global_hooks_path(&new_hooks_dir);
    sandbox.install_hooks();

    assert_eq!(
        fs::read_to_string(sandbox.previous_hooks_file())
            .expect("read previous hooks file")
            .trim(),
        new_hooks_dir.to_string_lossy()
    );

    sandbox.uninstall_hooks();
    assert_eq!(
        sandbox.global_hooks_path(),
        Some(new_hooks_dir.to_string_lossy().to_string())
    );
}

#[test]
fn pre_commit_chains_previous_global_hook_and_preserves_failure() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("previous-precommit-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-commit",
        &marker_hook_script(&marker, 23),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.write_repo_file("blocked.txt", "blocked\n");
    sandbox.run_git_ok(&["add", "blocked.txt"]);

    let commit_output = sandbox.run_git_raw(&["commit", "-m", "blocked commit"]);
    assert!(
        !commit_output.status.success(),
        "commit unexpectedly succeeded:\n{}",
        combined_output(&commit_output)
    );
    assert!(
        marker.exists(),
        "previous global pre-commit hook did not run"
    );

    let count_output = sandbox.run_git_raw(&["rev-list", "--count", "HEAD"]);
    assert!(
        count_output.status.success(),
        "failed to read commit count:\n{}",
        combined_output(&count_output)
    );
    let count = String::from_utf8_lossy(&count_output.stdout)
        .trim()
        .to_string();
    assert_eq!(count, "1", "failed pre-commit created a commit");
}

#[test]
fn post_commit_chains_previous_global_hook() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("previous-postcommit-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file("chained.txt", "chained\n", "run chained post-commit");
    assert!(
        marker.exists(),
        "previous global post-commit hook did not run"
    );
}

#[test]
fn falls_back_to_repo_dot_git_hooks_when_no_previous_global_path() {
    let sandbox = HookConfigSandbox::new();
    let marker = sandbox.temp.path().join("repo-postcommit-ran");

    sandbox.write_repo_hook("post-commit", &marker_hook_script(&marker, 0));
    sandbox.install_hooks();

    sandbox.commit_file("repo-hook.txt", "repo hook\n", "run repo post-commit");
    assert!(marker.exists(), "repo .git/hooks/post-commit did not run");
}

#[test]
fn does_not_run_repo_dot_git_hook_when_previous_global_path_exists() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let global_marker = sandbox.temp.path().join("global-postcommit-ran");
    let repo_marker = sandbox.temp.path().join("repo-postcommit-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        &marker_hook_script(&global_marker, 0),
    );
    sandbox.write_repo_hook("post-commit", &marker_hook_script(&repo_marker, 0));

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file("precedence.txt", "precedence\n", "post-commit precedence");

    assert!(
        global_marker.exists(),
        "previous global post-commit hook should run"
    );
    assert!(
        !repo_marker.exists(),
        "repo .git/hooks/post-commit should not run when previous global hooks path exists"
    );
}

#[test]
fn reinstall_self_heals_corrupted_or_missing_managed_hook_scripts() {
    let sandbox = HookConfigSandbox::new();
    sandbox.install_hooks();

    let managed_dir = sandbox.managed_hooks_dir();
    let pre_commit = managed_dir.join("pre-commit");
    let post_commit = managed_dir.join("post-commit");

    fs::write(&pre_commit, "#!/bin/sh\necho broken\n").expect("corrupt pre-commit");
    fs::remove_file(&post_commit).expect("remove post-commit");

    sandbox.install_hooks();

    let expected_binary = sandbox.git_ai_bin.to_string_lossy().replace('\\', "/");

    let healed_pre = fs::read_to_string(&pre_commit).expect("read healed pre-commit");
    assert!(healed_pre.contains("hook pre-commit"));
    assert!(healed_pre.contains(&expected_binary));

    let healed_post = fs::read_to_string(&post_commit).expect("read healed post-commit");
    assert!(healed_post.contains("hook post-commit"));
    assert!(healed_post.contains(&expected_binary));

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(managed_dir.to_string_lossy().to_string())
    );
}

#[test]
fn uninstall_does_not_override_user_changed_global_hooks_path() {
    let sandbox = HookConfigSandbox::new();
    let user_hooks_dir = sandbox.temp.path().join("user-hooks");
    fs::create_dir_all(&user_hooks_dir).expect("create user hooks dir");

    sandbox.install_hooks();
    sandbox.set_global_hooks_path(&user_hooks_dir);

    sandbox.uninstall_hooks();

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(user_hooks_dir.to_string_lossy().to_string())
    );
    assert!(!sandbox.managed_hooks_dir().exists());
}

#[test]
fn supports_previous_global_hooks_path_with_spaces() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("hooks with spaces");
    let marker = sandbox.temp.path().join("spaced-path-hook-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file("spaced.txt", "spaced\n", "post-commit with spaced path");
    assert!(
        marker.exists(),
        "hook from spaced previous global path did not run"
    );
}

#[test]
fn repo_local_core_hooks_path_overrides_managed_global_hooks() {
    let sandbox = HookConfigSandbox::new();
    let local_hooks_dir = sandbox.repo.join(".local-hooks");
    let marker = sandbox.temp.path().join("local-core-hooks-ran");

    sandbox.install_hooks();
    sandbox.write_hook(
        &local_hooks_dir,
        "post-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_local_hooks_path(&local_hooks_dir);

    sandbox.commit_file("local-hook.txt", "local\n", "local core.hooksPath hook");
    assert!(marker.exists(), "local core.hooksPath hook did not run");
}

#[test]
fn removing_repo_local_hooks_path_reverts_to_managed_behavior() {
    let sandbox = HookConfigSandbox::new();
    let local_hooks_dir = sandbox.repo.join(".local-hooks");
    let local_marker = sandbox.temp.path().join("local-core-hooks-ran");
    let repo_marker = sandbox.temp.path().join("repo-hooks-ran");

    sandbox.install_hooks();

    sandbox.write_hook(
        &local_hooks_dir,
        "post-commit",
        &marker_hook_script(&local_marker, 0),
    );
    sandbox.set_local_hooks_path(&local_hooks_dir);
    sandbox.commit_file("local-first.txt", "local\n", "local hook commit");
    assert!(
        local_marker.exists(),
        "local core.hooksPath hook did not run"
    );

    sandbox.unset_local_hooks_path();
    sandbox.write_repo_hook("post-commit", &marker_hook_script(&repo_marker, 0));
    sandbox.commit_file("repo-second.txt", "repo\n", "repo hook fallback commit");

    assert!(
        repo_marker.exists(),
        "repo .git/hooks hook did not run after local core.hooksPath was removed"
    );
}
