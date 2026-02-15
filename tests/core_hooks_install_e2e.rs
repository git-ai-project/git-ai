#![cfg(feature = "core-hooks-e2e")]

mod repos;

use git_ai::commands::core_hooks::{INSTALLED_HOOKS, PREVIOUS_HOOKS_PATH_FILE};
use repos::test_repo::get_binary_path;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};
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

    fn run_git_in_dir_raw(&self, dir: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(git_ai::config::Config::get().git_cmd());
        command.args(args).current_dir(dir);
        self.apply_env(&mut command);
        command.output().expect("run git in custom dir")
    }

    fn run_git_raw_with_timeout(&self, args: &[&str], timeout: Duration) -> Output {
        let mut command = Command::new(git_ai::config::Config::get().git_cmd());
        command.args(args).current_dir(&self.repo);
        self.apply_env(&mut command);

        let mut child = command.spawn().expect("spawn git");
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("wait on git child") {
                return Output {
                    status,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                };
            }

            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                panic!("git command timed out after {:?}: git {:?}", timeout, args);
            }

            sleep(Duration::from_millis(20));
        }
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

    fn run_git_in_dir_ok(&self, dir: &Path, args: &[&str]) -> String {
        let output = self.run_git_in_dir_raw(dir, args);
        if !output.status.success() {
            panic!(
                "git command failed in {}: git {:?}\n{}",
                dir.display(),
                args,
                combined_output(&output)
            );
        }
        combined_output(&output)
    }

    fn run_git_stdout_ok(&self, args: &[&str]) -> String {
        let output = self.run_git_raw(args);
        if !output.status.success() {
            panic!(
                "git command failed: git {:?}\n{}",
                args,
                combined_output(&output)
            );
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
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

    fn install_hooks_dry_run(&self) {
        self.run_git_ai_ok(&["install-hooks", "--dry-run=true"]);
    }

    fn uninstall_hooks_dry_run(&self) {
        self.run_git_ai_ok(&["uninstall-hooks", "--dry-run=true"]);
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

    fn set_global_hooks_path_raw(&self, hooks_path: &str) {
        self.run_git_ok(&["config", "--global", "core.hooksPath", hooks_path]);
    }

    fn set_local_hooks_path(&self, hooks_dir: &Path) {
        self.run_git_ok(&[
            "config",
            "core.hooksPath",
            hooks_dir.to_str().expect("hooks dir path"),
        ]);
    }

    fn set_local_hooks_path_raw(&self, hooks_path: &str) {
        self.run_git_ok(&["config", "core.hooksPath", hooks_path]);
    }

    fn local_hooks_path(&self) -> Option<String> {
        let output = self.run_git_raw(&["config", "--get", "core.hooksPath"]);
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        } else {
            None
        }
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)
                .expect("hook metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).expect("set hook permissions");
        }
    }

    fn write_repo_hook(&self, hook_name: &str, script_body: &str) {
        let hooks_dir = self.repo.join(".git").join("hooks");
        self.write_hook(&hooks_dir, hook_name, script_body);
    }

    fn last_commit_subject(&self) -> String {
        self.run_git_stdout_ok(&["log", "-1", "--pretty=%s"])
    }

    fn read_previous_hooks_path(&self) -> String {
        fs::read_to_string(self.previous_hooks_file())
            .expect("read previous hooks file")
            .trim()
            .to_string()
    }

    fn core_hook_state_path(&self) -> PathBuf {
        self.repo
            .join(".git")
            .join("ai")
            .join("core_hook_state.json")
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("stdout:\n{}\nstderr:\n{}", stdout, stderr)
}

fn shell_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

fn marker_hook_script(marker: &Path, exit_code: i32) -> String {
    let escaped = shell_escape(marker);
    format!(
        "#!/bin/sh\nprintf '%s\\n' ran >> \"{}\"\nexit {}\n",
        escaped, exit_code
    )
}

fn husky_pre_commit_script(marker: &Path) -> String {
    let escaped = shell_escape(marker);
    format!(
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-ran >> \"{}\"\nexit 0\n",
        escaped
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
fn post_commit_chains_previous_global_hook_with_helper_script() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let helper_dir = previous_hooks_dir.join("_");
    let marker = sandbox.temp.path().join("previous-helper-postcommit-ran");
    let marker_escaped = shell_escape(&marker);

    sandbox.write_hook(
        &helper_dir,
        "helper.sh",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' previous-helper-ran >> \"{}\"\n",
            marker_escaped
        ),
    );
    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/helper.sh\"\nexit 0\n",
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file(
        "previous-helper.txt",
        "helper\n",
        "run chained previous post-commit helper hook",
    );
    assert!(
        marker.exists(),
        "previous global post-commit helper hook did not run"
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
fn pre_push_chains_previous_global_hook_and_forwards_remote_args() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("previous-pre-push-args");
    let marker_escaped = shell_escape(&marker);

    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-push",
        &format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\" >> \"{}\"\nexit 0\n",
            marker_escaped
        ),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let remote_path = sandbox.temp.path().join("remote.git");
    sandbox.run_git_ok(&["init", "--bare", remote_path.to_str().expect("remote path")]);
    sandbox.run_git_ok(&[
        "remote",
        "add",
        "origin",
        remote_path.to_str().expect("remote path"),
    ]);

    sandbox.run_git_ok(&["push", "-u", "origin", "HEAD"]);

    let marker_content = fs::read_to_string(&marker).expect("read pre-push marker");
    let line = marker_content
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("pre-push hook did not capture any args");
    let mut parts = line.splitn(2, '|');
    let remote_name = parts.next().unwrap_or_default();
    let remote_url = parts.next().unwrap_or_default();

    assert_eq!(
        remote_name, "origin",
        "pre-push remote name was not forwarded"
    );
    assert!(
        !remote_url.trim().is_empty(),
        "pre-push remote URL argument was not forwarded"
    );
}

#[test]
fn pre_push_chains_previous_global_hook_and_forwards_stdin_updates() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("previous-pre-push-stdin");
    let marker_escaped = shell_escape(&marker);

    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-push",
        &format!(
            "#!/bin/sh\nIFS= read -r update\nprintf '%s\\n' \"$update\" >> \"{}\"\nexit 0\n",
            marker_escaped
        ),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let remote_path = sandbox.temp.path().join("remote.git");
    sandbox.run_git_ok(&["init", "--bare", remote_path.to_str().expect("remote path")]);
    sandbox.run_git_ok(&[
        "remote",
        "add",
        "origin",
        remote_path.to_str().expect("remote path"),
    ]);

    sandbox.run_git_ok(&["push", "-u", "origin", "HEAD"]);

    let stdin_line = fs::read_to_string(&marker)
        .expect("read pre-push stdin marker")
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("pre-push hook did not capture stdin update line")
        .to_string();
    assert!(
        stdin_line.contains("refs/heads/"),
        "pre-push stdin update line should contain local/remote refs"
    );
}

#[test]
fn prepare_commit_msg_chains_previous_global_hook_and_can_mutate_subject() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");

    sandbox.write_hook(
        &previous_hooks_dir,
        "prepare-commit-msg",
        "#!/bin/sh\nmsg_file=\"$1\"\nsubject=$(sed -n '1p' \"$msg_file\")\nprintf '%s\\n' \"$subject [prepared]\" > \"$msg_file\"\nexit 0\n",
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file("prepared-message.txt", "prepared\n", "original subject");
    assert_eq!(sandbox.last_commit_subject(), "original subject [prepared]");
}

#[test]
fn commit_msg_chains_previous_global_hook_and_preserves_failure() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("commit-msg-blocked");
    let marker_escaped = shell_escape(&marker);

    sandbox.write_hook(
        &previous_hooks_dir,
        "commit-msg",
        &format!(
            "#!/bin/sh\nmsg_file=\"$1\"\nif grep -q \"forbidden\" \"$msg_file\"; then\n  printf '%s\\n' blocked >> \"{}\"\n  exit 47\nfi\nexit 0\n",
            marker_escaped
        ),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.write_repo_file("blocked-msg.txt", "blocked\n");
    sandbox.run_git_ok(&["add", "blocked-msg.txt"]);

    let output = sandbox.run_git_raw(&["commit", "-m", "forbidden subject"]);
    assert!(
        !output.status.success(),
        "commit unexpectedly succeeded despite commit-msg failure:\n{}",
        combined_output(&output)
    );
    assert!(marker.exists(), "previous commit-msg hook did not run");

    let count = sandbox.run_git_stdout_ok(&["rev-list", "--count", "HEAD"]);
    assert_eq!(
        count, "1",
        "failed commit-msg hook should not create a new commit"
    );
}

#[test]
fn global_husky_style_pre_commit_hook_with_helper_is_chained() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-husky-hooks");
    let helper_dir = previous_hooks_dir.join("_");
    let marker = sandbox.temp.path().join("global-husky-precommit-ran");

    sandbox.write_hook(
        &helper_dir,
        "husky.sh",
        "#!/bin/sh\n# global husky helper shim\n",
    );
    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-commit",
        &husky_pre_commit_script(&marker),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    sandbox.commit_file(
        "global-husky.txt",
        "global husky\n",
        "global husky pre-commit",
    );
    assert!(
        marker.exists(),
        "global husky-style pre-commit hook with helper did not run"
    );
}

#[test]
fn pre_applypatch_chains_previous_global_hook_during_git_am() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("pre-applypatch-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-applypatch",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let donor = sandbox.temp.path().join("donor-repo");
    fs::create_dir_all(&donor).expect("create donor repo");
    sandbox.run_git_in_dir_ok(&donor, &["init"]);
    sandbox.run_git_in_dir_ok(&donor, &["config", "user.name", "Patch Author"]);
    sandbox.run_git_in_dir_ok(&donor, &["config", "user.email", "patch@example.com"]);
    fs::write(donor.join("patch-email.txt"), "from patch\n").expect("write donor file");
    sandbox.run_git_in_dir_ok(&donor, &["add", "patch-email.txt"]);
    sandbox.run_git_in_dir_ok(&donor, &["commit", "-m", "patch commit"]);

    let patch_output =
        sandbox.run_git_in_dir_raw(&donor, &["format-patch", "-1", "HEAD", "--stdout"]);
    assert!(
        patch_output.status.success(),
        "failed to create patch:\n{}",
        combined_output(&patch_output)
    );
    let patch_path = sandbox.temp.path().join("change.patch");
    fs::write(&patch_path, &patch_output.stdout).expect("write generated patch");

    sandbox.run_git_ok(&["am", patch_path.to_str().expect("patch path")]);

    assert!(marker.exists(), "previous pre-applypatch hook did not run");
    assert!(
        sandbox.repo.join("patch-email.txt").exists(),
        "patch content was not applied"
    );
}

#[test]
fn pre_merge_commit_chains_previous_global_hook_when_creating_merge_commit() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("pre-merge-commit-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "pre-merge-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let default_branch = sandbox.run_git_stdout_ok(&["branch", "--show-current"]);
    sandbox.run_git_ok(&["checkout", "-b", "feature"]);
    sandbox.commit_file("feature.txt", "feature\n", "feature commit");

    sandbox.run_git_ok(&["checkout", &default_branch]);
    sandbox.commit_file("main.txt", "main\n", "main commit");
    sandbox.run_git_ok(&["merge", "--no-ff", "feature", "-m", "merge feature"]);

    assert!(
        marker.exists(),
        "previous pre-merge-commit hook did not run"
    );
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

#[test]
fn husky_relative_core_hooks_path_overrides_managed_global_hooks() {
    let sandbox = HookConfigSandbox::new();
    let husky_dir = sandbox.repo.join(".husky");
    let marker = sandbox.temp.path().join("husky-precommit-ran");

    sandbox.install_hooks();
    sandbox.write_hook(&husky_dir, "pre-commit", &marker_hook_script(&marker, 0));
    sandbox.set_local_hooks_path_raw(".husky");

    sandbox.commit_file("husky.txt", "husky\n", "husky relative pre-commit");

    assert!(
        marker.exists(),
        "relative .husky pre-commit did not run when local core.hooksPath was set"
    );
    assert!(
        !sandbox.core_hook_state_path().exists(),
        "managed core hooks should not run when local core.hooksPath overrides global hooks"
    );
}

#[test]
fn husky_style_hook_with_helper_script_runs() {
    let sandbox = HookConfigSandbox::new();
    let husky_dir = sandbox.repo.join(".husky");
    let husky_helper_dir = husky_dir.join("_");
    let marker = sandbox.temp.path().join("husky-helper-ran");

    sandbox.install_hooks();
    sandbox.write_hook(
        &husky_helper_dir,
        "husky.sh",
        "#!/bin/sh\n# husky helper shim\n",
    );
    sandbox.write_hook(&husky_dir, "pre-commit", &husky_pre_commit_script(&marker));
    sandbox.set_local_hooks_path_raw(".husky");

    sandbox.commit_file("husky-helper.txt", "husky helper\n", "husky helper commit");

    assert!(
        marker.exists(),
        "husky-like pre-commit with helper shim did not run"
    );
}

#[test]
fn previous_global_hooks_path_with_tilde_is_expanded_for_chaining() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.home.join("legacy-hooks");
    let marker = sandbox.temp.path().join("tilde-hooks-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path_raw("~/legacy-hooks");
    sandbox.install_hooks();

    sandbox.commit_file("tilde.txt", "tilde\n", "tilde core.hooksPath commit");

    assert!(
        marker.exists(),
        "hook from tilde-based previous core.hooksPath did not run"
    );
}

#[test]
fn previous_hooks_file_with_crlf_is_parsed_correctly() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("previous-hooks");
    let marker = sandbox.temp.path().join("crlf-hook-ran");

    sandbox.write_hook(
        &previous_hooks_dir,
        "post-commit",
        &marker_hook_script(&marker, 0),
    );
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    fs::write(
        sandbox.previous_hooks_file(),
        format!("{}\r\n", previous_hooks_dir.display()),
    )
    .expect("rewrite previous_hooks_path with CRLF");

    sandbox.commit_file("crlf.txt", "crlf\n", "crlf previous hooks path");
    assert!(
        marker.exists(),
        "CRLF-terminated previous hooks path should still chain to the previous hook"
    );
}

#[test]
fn reinstall_with_managed_path_alias_preserves_previous_hooks_target() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let managed_alias = format!("{}/", sandbox.managed_hooks_dir().display());
    sandbox.set_global_hooks_path_raw(&managed_alias);
    sandbox.install_hooks();

    assert_eq!(
        sandbox.read_previous_hooks_path(),
        previous_hooks_dir.to_string_lossy(),
        "reinstall must not overwrite previous hooks target with a managed path alias"
    );

    sandbox.uninstall_hooks();
    assert_eq!(
        sandbox.global_hooks_path(),
        Some(previous_hooks_dir.to_string_lossy().to_string())
    );
}

#[test]
fn uninstall_restores_previous_hooks_path_even_when_config_uses_managed_alias() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let managed_alias = format!("{}/", sandbox.managed_hooks_dir().display());
    sandbox.set_global_hooks_path_raw(&managed_alias);
    sandbox.uninstall_hooks();

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(previous_hooks_dir.to_string_lossy().to_string()),
        "uninstall should restore original hooks path when managed path is represented as an alias"
    );
}

#[test]
fn uninstall_self_heals_when_previous_hooks_metadata_is_missing() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    fs::remove_file(sandbox.previous_hooks_file()).expect("remove previous hooks metadata");
    sandbox.uninstall_hooks();

    assert_eq!(
        sandbox.global_hooks_path(),
        None,
        "when previous hooks metadata is missing, uninstall should unset core.hooksPath"
    );
    assert!(
        !sandbox.managed_hooks_dir().exists(),
        "uninstall should remove managed hook scripts even if metadata is missing"
    );
}

#[test]
fn install_hooks_dry_run_does_not_mutate_global_config_or_files() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");
    sandbox.set_global_hooks_path(&previous_hooks_dir);

    sandbox.install_hooks_dry_run();

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(previous_hooks_dir.to_string_lossy().to_string()),
        "dry-run install must not modify global core.hooksPath"
    );
    assert!(
        !sandbox.managed_hooks_dir().exists(),
        "dry-run install must not create managed hook scripts"
    );
}

#[test]
fn uninstall_hooks_dry_run_does_not_mutate_global_config_or_files() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    fs::create_dir_all(&previous_hooks_dir).expect("create previous hooks dir");
    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();

    let managed_path = sandbox.managed_hooks_dir().to_string_lossy().to_string();
    assert!(sandbox.managed_hooks_dir().exists());

    sandbox.uninstall_hooks_dry_run();

    assert_eq!(
        sandbox.global_hooks_path(),
        Some(managed_path),
        "dry-run uninstall must not modify global core.hooksPath"
    );
    assert!(
        sandbox.managed_hooks_dir().exists(),
        "dry-run uninstall must not remove managed hook scripts"
    );
}

#[test]
fn local_core_hooks_path_survives_global_install_uninstall_cycles() {
    let sandbox = HookConfigSandbox::new();
    sandbox.set_local_hooks_path_raw(".husky");

    sandbox.install_hooks();
    sandbox.uninstall_hooks();

    assert_eq!(
        sandbox.local_hooks_path(),
        Some(".husky".to_string()),
        "install/uninstall must not clobber repository-level core.hooksPath"
    );
}

#[test]
fn local_core_hooks_path_remains_functional_after_global_uninstall() {
    let sandbox = HookConfigSandbox::new();
    let husky_dir = sandbox.repo.join(".husky");
    let marker = sandbox.temp.path().join("local-husky-after-uninstall-ran");

    sandbox.write_hook(&husky_dir, "pre-commit", &marker_hook_script(&marker, 0));
    sandbox.set_local_hooks_path_raw(".husky");

    sandbox.install_hooks();
    sandbox.uninstall_hooks();

    sandbox.commit_file(
        "local-after-uninstall.txt",
        "local hook\n",
        "local hook should still run after uninstall",
    );
    assert!(
        marker.exists(),
        "repo-local hooks must keep working after global managed hooks are uninstalled"
    );
}

#[test]
fn previous_hooks_self_reference_does_not_recurse_or_hang() {
    let sandbox = HookConfigSandbox::new();
    let repo_marker = sandbox.temp.path().join("repo-postcommit-ran");

    sandbox.install_hooks();
    sandbox.write_repo_hook("post-commit", &marker_hook_script(&repo_marker, 0));
    fs::write(
        sandbox.previous_hooks_file(),
        format!("{}/\n", sandbox.managed_hooks_dir().display()),
    )
    .expect("rewrite previous hooks file as self reference");

    sandbox.write_repo_file("self-ref.txt", "self\n");
    sandbox.run_git_ok(&["add", "self-ref.txt"]);
    let output = sandbox.run_git_raw_with_timeout(
        &["commit", "-m", "self reference does not recurse"],
        Duration::from_secs(30),
    );
    assert!(
        output.status.success(),
        "commit should not hang or fail when previous hooks path points to managed hooks dir:\n{}",
        combined_output(&output)
    );
    assert!(
        repo_marker.exists(),
        "self-referenced previous hooks dir should fall back to repo .git/hooks"
    );
}

#[test]
#[cfg(unix)]
fn non_executable_previous_pre_commit_hook_is_skipped() {
    let sandbox = HookConfigSandbox::new();
    let previous_hooks_dir = sandbox.temp.path().join("legacy-hooks");
    let marker = sandbox.temp.path().join("non-executable-hook-ran");

    fs::create_dir_all(&previous_hooks_dir).expect("create legacy hooks dir");
    let pre_commit_path = previous_hooks_dir.join("pre-commit");
    fs::write(&pre_commit_path, marker_hook_script(&marker, 0)).expect("write legacy pre-commit");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&pre_commit_path)
            .expect("legacy pre-commit metadata")
            .permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&pre_commit_path, perms).expect("set non executable permissions");
    }

    sandbox.set_global_hooks_path(&previous_hooks_dir);
    sandbox.install_hooks();
    sandbox.commit_file(
        "non-exec.txt",
        "non exec\n",
        "non executable previous pre-commit should be ignored",
    );

    assert!(
        !marker.exists(),
        "non-executable previous pre-commit hook must not be executed"
    );
}
