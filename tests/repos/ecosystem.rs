#![allow(dead_code)]

use git_ai::commands::core_hooks::{INSTALLED_HOOKS, PREVIOUS_HOOKS_PATH_FILE};
use git_ai::config::Config;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::test_repo::get_binary_path;

const STRICT_TOOL_ENV: &str = "GIT_AI_REQUIRE_ECOSYSTEM_TOOLS";
const PROFILE_ENV: &str = "GIT_AI_ECOSYSTEM_PROFILE";

pub struct EcosystemTestbed {
    _temp: TempDir,
    scenario: String,
    pub root: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub logs_dir: PathBuf,
    global_config: PathBuf,
    git_ai_bin: PathBuf,
}

impl EcosystemTestbed {
    pub fn new(name: &str) -> Self {
        let temp = tempfile::Builder::new()
            .prefix(&format!("git-ai-ecosystem-{}-", sanitize_name(name)))
            .tempdir()
            .expect("create ecosystem tempdir");

        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        let logs_dir = root.join("logs");

        fs::create_dir_all(home.join(".config")).expect("create home config dir");
        fs::create_dir_all(&repo).expect("create repo dir");
        fs::create_dir_all(&logs_dir).expect("create logs dir");

        let testbed = Self {
            _temp: temp,
            scenario: sanitize_name(name),
            root,
            home: home.clone(),
            repo,
            logs_dir,
            global_config: home.join(".gitconfig"),
            git_ai_bin: get_binary_path().clone(),
        };

        testbed.init_repo();
        testbed
    }

    pub fn managed_hooks_dir(&self) -> PathBuf {
        self.home.join(".git-ai").join("core-hooks")
    }

    pub fn previous_hooks_file(&self) -> PathBuf {
        self.managed_hooks_dir().join(PREVIOUS_HOOKS_PATH_FILE)
    }

    pub fn core_hook_state_path(&self) -> PathBuf {
        self.repo
            .join(".git")
            .join("ai")
            .join("core_hook_state.json")
    }

    pub fn hook_log_path(&self) -> PathBuf {
        self.repo.join(".hook-log")
    }

    pub fn append_hook_log(&self, line: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.hook_log_path())
            .expect("open hook log");
        writeln!(file, "{}", line).expect("append hook log");
    }

    pub fn hook_log_lines(&self) -> Vec<String> {
        fs::read_to_string(self.hook_log_path())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn require_tool(&self, tool: &str) -> bool {
        if self.has_command(tool) {
            return true;
        }

        if strict_tool_mode() {
            panic!("required tool '{}' was not found in PATH", tool);
        }

        eprintln!("skipping test: required tool '{}' not found", tool);
        false
    }

    pub fn has_command(&self, tool: &str) -> bool {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let candidate = dir.join(tool);
                if candidate.exists() {
                    return true;
                }

                #[cfg(windows)]
                {
                    for ext in ["exe", "cmd", "bat"] {
                        let candidate = dir.join(format!("{}.{}", tool, ext));
                        if candidate.exists() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn install_hooks(&self) {
        self.run_git_ai_ok(&["install-hooks", "--dry-run=false"], "install-hooks");
    }

    pub fn uninstall_hooks(&self) {
        self.run_git_ai_ok(&["uninstall-hooks", "--dry-run=false"], "uninstall-hooks");
    }

    pub fn run_git_ok(&self, args: &[&str], label: &str) -> String {
        let output = self.run_git(args, label);
        if !output.status.success() {
            panic!(
                "git command failed ({}): git {:?}\n{}",
                label,
                args,
                self.format_output(&output)
            );
        }
        self.output_string(&output)
    }

    pub fn run_git_expect_failure(&self, args: &[&str], label: &str) -> Output {
        let output = self.run_git(args, label);
        if output.status.success() {
            panic!(
                "expected git command to fail ({}): git {:?}\n{}",
                label,
                args,
                self.format_output(&output)
            );
        }
        output
    }

    pub fn run_git_in_dir_ok(&self, cwd: &Path, args: &[&str], label: &str) -> String {
        let output = self.run_command(
            Config::get().git_cmd(),
            args,
            Some(cwd),
            &[],
            None,
            Some(Duration::from_secs(120)),
            label,
        );
        if !output.status.success() {
            panic!(
                "git command failed in {} ({}): git {:?}\n{}",
                cwd.display(),
                label,
                args,
                self.format_output(&output)
            );
        }
        self.output_string(&output)
    }

    pub fn run_git_ai_ok(&self, args: &[&str], label: &str) -> String {
        let output = self.run_command(
            &self.git_ai_bin,
            args,
            Some(&self.repo),
            &[],
            None,
            Some(Duration::from_secs(120)),
            label,
        );
        if !output.status.success() {
            panic!(
                "git-ai command failed ({}): git-ai {:?}\n{}",
                label,
                args,
                self.format_output(&output)
            );
        }
        self.output_string(&output)
    }

    pub fn run_cmd_ok<S: AsRef<OsStr>>(
        &self,
        program: S,
        args: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        label: &str,
    ) -> String {
        let output = self.run_command(
            program,
            args,
            cwd,
            extra_env,
            None,
            Some(Duration::from_secs(240)),
            label,
        );
        if !output.status.success() {
            panic!(
                "command failed ({}): {:?} {:?}\n{}",
                label,
                args,
                cwd.map(|p| p.display().to_string()),
                self.format_output(&output)
            );
        }
        self.output_string(&output)
    }

    pub fn run_cmd_with_stdin<S: AsRef<OsStr>>(
        &self,
        program: S,
        args: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        stdin_data: &str,
        label: &str,
    ) -> Output {
        self.run_command(
            program,
            args,
            cwd,
            extra_env,
            Some(stdin_data.as_bytes()),
            Some(Duration::from_secs(120)),
            label,
        )
    }

    pub fn run_hook_script(
        &self,
        hook_name: &str,
        args: &[&str],
        stdin: Option<&str>,
        expect_success: bool,
        label: &str,
    ) -> Output {
        let hook_path = self.managed_hooks_dir().join(hook_name);
        let hook_path_owned = hook_path.to_string_lossy().to_string();
        let mut hook_args: Vec<&str> = vec![hook_path_owned.as_str()];
        hook_args.extend(args.iter().copied());
        let output = self.run_command(
            "sh",
            &hook_args,
            Some(&self.repo),
            &[],
            stdin.map(str::as_bytes),
            Some(Duration::from_secs(30)),
            label,
        );

        if expect_success && !output.status.success() {
            panic!(
                "hook script failed unexpectedly ({}): {}\n{}",
                label,
                hook_name,
                self.format_output(&output)
            );
        }
        if !expect_success && output.status.success() {
            panic!(
                "hook script unexpectedly succeeded ({}): {}\n{}",
                label,
                hook_name,
                self.format_output(&output)
            );
        }

        output
    }

    pub fn set_global_hooks_path_raw(&self, hooks_path: &str) {
        self.run_git_ok(
            &["config", "--global", "core.hooksPath", hooks_path],
            "set global hooks path",
        );
    }

    pub fn set_local_hooks_path_raw(&self, hooks_path: &str) {
        self.run_git_ok(
            &["config", "core.hooksPath", hooks_path],
            "set local hooks path",
        );
    }

    pub fn unset_local_hooks_path(&self) {
        self.run_git_ok(
            &["config", "--unset", "core.hooksPath"],
            "unset local hooks path",
        );
    }

    pub fn global_hooks_path(&self) -> Option<String> {
        let output = self.run_git(
            &["config", "--global", "--get", "core.hooksPath"],
            "get global hooks",
        );
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        } else {
            None
        }
    }

    pub fn local_hooks_path(&self) -> Option<String> {
        let output = self.run_git(&["config", "--get", "core.hooksPath"], "get local hooks");
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        } else {
            None
        }
    }

    pub fn write_file(&self, rel: &str, content: &str) {
        let path = self.repo.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create file parent");
        }
        fs::write(path, content).expect("write file");
    }

    pub fn write_repo_hook(&self, hook_name: &str, body: &str) {
        let hooks_dir = self.repo.join(".git").join("hooks");
        self.write_hook_script(&hooks_dir, hook_name, body);
    }

    pub fn write_hook_script(&self, hooks_dir: &Path, hook_name: &str, body: &str) {
        fs::create_dir_all(hooks_dir).expect("create hook dir");
        let hook_path = hooks_dir.join(hook_name);
        fs::write(&hook_path, body).expect("write hook script");

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

    pub fn marker_hook_script(&self, marker: &Path, exit_code: i32) -> String {
        let escaped = shell_escape(marker);
        format!(
            "#!/bin/sh\nprintf '%s\\n' ran >> \"{}\"\nexit {}\n",
            escaped, exit_code
        )
    }

    pub fn husky_pre_commit_script(&self, marker: &Path) -> String {
        let escaped = shell_escape(marker);
        format!(
            "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nprintf '%s\\n' husky-ran >> \"{}\"\nexit 0\n",
            escaped
        )
    }

    pub fn commit_file(&self, rel: &str, content: &str, message: &str) {
        self.write_file(rel, content);
        self.run_git_ok(&["add", rel], "stage file");
        self.run_git_ok(&["commit", "-m", message], "commit file");
    }

    pub fn current_branch(&self) -> String {
        self.run_git_ok(&["branch", "--show-current"], "current branch")
            .trim()
            .to_string()
    }

    pub fn init_bare_remote(&self, name: &str) -> PathBuf {
        let remote_path = self.root.join(format!("{}.git", name));
        self.run_git_ok(
            &["init", "--bare", remote_path.to_string_lossy().as_ref()],
            "init bare remote",
        );
        remote_path
    }

    pub fn add_remote_origin(&self, remote_path: &Path) {
        self.run_git_ok(
            &[
                "remote",
                "add",
                "origin",
                remote_path.to_string_lossy().as_ref(),
            ],
            "add origin",
        );
    }

    pub fn push_head_to_origin(&self) {
        self.run_git_ok(&["push", "-u", "origin", "HEAD"], "push head");
    }

    pub fn profile(&self) -> String {
        std::env::var(PROFILE_ENV)
            .unwrap_or_else(|_| "full".to_string())
            .to_ascii_lowercase()
    }

    pub fn is_smoke_profile(&self) -> bool {
        self.profile() == "smoke"
    }

    fn init_repo(&self) {
        self.run_git_ok(&["init"], "git init");
        self.run_git_ok(&["config", "user.name", "Test User"], "set user name");
        self.run_git_ok(
            &["config", "user.email", "test@example.com"],
            "set user email",
        );
        self.write_file("README.md", "init\n");
        self.run_git_ok(&["add", "README.md"], "stage readme");
        self.run_git_ok(&["commit", "-m", "initial"], "initial commit");
    }

    fn run_git(&self, args: &[&str], label: &str) -> Output {
        self.run_command(
            Config::get().git_cmd(),
            args,
            Some(&self.repo),
            &[],
            None,
            Some(Duration::from_secs(120)),
            label,
        )
    }

    fn run_command<S: AsRef<OsStr>>(
        &self,
        program: S,
        args: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        stdin_data: Option<&[u8]>,
        timeout: Option<Duration>,
        label: &str,
    ) -> Output {
        let mut cmd = Command::new(program);
        cmd.args(args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        self.apply_env(&mut cmd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        if stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let timeout = timeout.unwrap_or(Duration::from_secs(120));
        let mut child = cmd.spawn().expect("spawn command");

        if let Some(stdin) = stdin_data
            && let Some(mut child_stdin) = child.stdin.take()
        {
            child_stdin.write_all(stdin).expect("write stdin");
        }

        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("wait for command") {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    out.read_to_end(&mut stdout).expect("read stdout");
                }
                if let Some(mut err) = child.stderr.take() {
                    use std::io::Read;
                    err.read_to_end(&mut stderr).expect("read stderr");
                }
                let output = Output {
                    status,
                    stdout,
                    stderr,
                };
                self.append_command_log(label, args, &output);
                return output;
            }

            if start.elapsed() > timeout {
                let _ = child.kill();
                let output = child.wait_with_output().expect("collect timed out output");
                self.append_command_log(label, args, &output);
                panic!(
                    "command timed out after {:?} ({}): {:?}\n{}",
                    timeout,
                    label,
                    args,
                    self.format_output(&output)
                );
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    fn apply_env(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.home);
        cmd.env("USERPROFILE", &self.home);
        cmd.env("XDG_CONFIG_HOME", self.home.join(".config"));
        cmd.env("GIT_CONFIG_GLOBAL", &self.global_config);
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env_remove("GIT_DIR");
        cmd.env_remove("GIT_WORK_TREE");
    }

    fn append_command_log(&self, label: &str, args: &[&str], output: &Output) {
        let log_path = self.logs_dir.join("commands.log");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .expect("open command log");

        let _ = writeln!(log, "== {} ==", label);
        let _ = writeln!(log, "args: {:?}", args);
        let _ = writeln!(log, "status: {}", output.status);
        let _ = writeln!(log, "stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        let _ = writeln!(log, "stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        let _ = writeln!(log);
    }

    fn format_output(&self, output: &Output) -> String {
        format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    }

    fn output_string(&self, output: &Output) -> String {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stdout.is_empty() {
            stderr.to_string()
        } else if stderr.is_empty() {
            stdout.to_string()
        } else {
            format!("{}{}", stdout, stderr)
        }
    }
}

impl Drop for EcosystemTestbed {
    fn drop(&mut self) {
        let artifact_dir = match std::env::var("GIT_AI_ECOSYSTEM_ARTIFACT_DIR") {
            Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
            _ => return,
        };

        let destination = artifact_dir.join(&self.scenario);
        if fs::create_dir_all(&destination).is_err() {
            return;
        }

        let _ = fs::copy(
            self.logs_dir.join("commands.log"),
            destination.join("commands.log"),
        );
        let _ = fs::copy(self.hook_log_path(), destination.join("hook.log"));
        let _ = fs::copy(
            self.previous_hooks_file(),
            destination.join("previous_hooks_path"),
        );
        let _ = fs::copy(
            self.core_hook_state_path(),
            destination.join("core_hook_state.json"),
        );
    }
}

pub fn strict_tool_mode() -> bool {
    std::env::var(STRICT_TOOL_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn shell_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn assert_all_installed_hooks_present(testbed: &EcosystemTestbed) {
    for hook in INSTALLED_HOOKS {
        let hook_path = testbed.managed_hooks_dir().join(hook);
        assert!(
            hook_path.exists(),
            "managed hook script missing: {}",
            hook_path.display()
        );
    }
}
