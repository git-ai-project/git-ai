#![cfg(windows)]

#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use repos::test_repo::{GitTestMode, TestRepo, get_binary_path, real_git_executable};
use serde_json::Value;
use serial_test::serial;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct CommandResult {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn install_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.ps1")
}

fn installed_git_ai_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("bin")
        .join("git-ai.exe")
}

fn installed_git_wrapper_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("bin")
        .join("git.exe")
}

fn foreground_daemon_stdout_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path().join("foreground-daemon.stdout.log")
}

fn foreground_daemon_stderr_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path().join("foreground-daemon.stderr.log")
}

fn foreground_daemon_logs(repo: &TestRepo) -> (String, String) {
    let stdout = fs::read_to_string(foreground_daemon_stdout_path(repo)).unwrap_or_default();
    let stderr = fs::read_to_string(foreground_daemon_stderr_path(repo)).unwrap_or_default();
    (stdout, stderr)
}

fn daemon_log_dir(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("logs")
}

fn wait_for_daemon_log_file(repo: &TestRepo, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    let pid_meta_path = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("daemon.pid.json");

    loop {
        if pid_meta_path.exists() {
            let meta_raw =
                fs::read_to_string(&pid_meta_path).expect("failed to read daemon pid metadata");
            let meta: Value =
                serde_json::from_str(&meta_raw).expect("failed to parse daemon pid metadata");
            let pid = meta
                .get("pid")
                .and_then(Value::as_u64)
                .expect("daemon pid metadata missing pid");
            let log_path = daemon_log_dir(repo).join(format!("{}.log", pid));
            if log_path.exists() {
                return log_path;
            }
        }

        if Instant::now() >= deadline {
            panic!(
                "daemon log file was not created within {:?} under {}",
                timeout,
                daemon_log_dir(repo).display()
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_file_to_contain(path: &PathBuf, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let contents = fs::read_to_string(path).unwrap_or_default();
        if contents.contains(needle) {
            return;
        }

        if Instant::now() >= deadline {
            panic!(
                "file {} did not contain {:?} within {:?}\ncontents:\n{}",
                path.display(),
                needle,
                timeout,
                contents
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn configure_install_env(command: &mut Command, repo: &TestRepo) {
    let home = repo.test_home_path().to_string_lossy().to_string();
    let (home_drive, home_path) = if home.len() >= 2 && home.as_bytes()[1] == b':' {
        (home[..2].to_string(), home[2..].to_string())
    } else {
        ("".to_string(), home.clone())
    };
    let git_dir = PathBuf::from(real_git_executable())
        .parent()
        .expect("real git executable should have a parent")
        .to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path_with_git = if path.is_empty() {
        git_dir.into_os_string()
    } else {
        let mut combined = git_dir.into_os_string();
        combined.push(";");
        combined.push(path);
        combined
    };

    command.env("GIT_AI_LOCAL_BINARY", get_binary_path());
    command.env("GIT_AI_SKIP_PATH_UPDATE", "1");
    command.env("PATH", path_with_git);
    command.env("HOME", repo.test_home_path());
    command.env("USERPROFILE", repo.test_home_path());
    command.env("HOMEDRIVE", home_drive);
    command.env("HOMEPATH", home_path);
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env(
        "APPDATA",
        repo.test_home_path().join("AppData").join("Roaming"),
    );
    command.env(
        "LOCALAPPDATA",
        repo.test_home_path().join("AppData").join("Local"),
    );
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path());
    command.env("GITAI_TEST_DB_PATH", repo.test_db_path());
    command.env("GIT_AI_DAEMON_HOME", repo.daemon_home_path());
    command.env(
        "GIT_AI_DAEMON_CONTROL_SOCKET",
        repo.daemon_control_socket_path(),
    );
    command.env(
        "GIT_AI_DAEMON_TRACE_SOCKET",
        repo.daemon_trace_socket_path(),
    );
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> CommandResult {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("failed to spawn command");
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait().expect("failed to poll child status") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut handle) = child.stdout.take() {
                    let _ = handle.read_to_string(&mut stdout);
                }
                if let Some(mut handle) = child.stderr.take() {
                    let _ = handle.read_to_string(&mut stderr);
                }
                return CommandResult {
                    status,
                    stdout,
                    stderr,
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();

                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut handle) = child.stdout.take() {
                    let _ = handle.read_to_string(&mut stdout);
                }
                if let Some(mut handle) = child.stderr.take() {
                    let _ = handle.read_to_string(&mut stderr);
                }

                panic!(
                    "command timed out after {:?}\nstdout:\n{}\nstderr:\n{}",
                    timeout, stdout, stderr
                );
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn run_install_script(repo: &TestRepo, timeout: Duration) -> CommandResult {
    let mut command = Command::new("powershell");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(install_script_path())
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    configure_install_env(&mut command, repo);
    run_command_with_timeout(&mut command, timeout)
}

fn run_installed_git_ai(repo: &TestRepo, args: &[&str], timeout: Duration) -> CommandResult {
    let mut command = Command::new(installed_git_ai_path(repo));
    command.args(args).current_dir(repo.test_home_path());
    configure_install_env(&mut command, repo);
    run_command_with_timeout(&mut command, timeout)
}

fn run_installed_git_wrapper(repo: &TestRepo, args: &[&str], timeout: Duration) -> CommandResult {
    let mut command = Command::new(installed_git_wrapper_path(repo));
    command.args(args).current_dir(repo.path());
    configure_install_env(&mut command, repo);
    run_command_with_timeout(&mut command, timeout)
}

fn spawn_installed_daemon(repo: &TestRepo) -> Child {
    let stdout_log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(foreground_daemon_stdout_path(repo))
        .expect("failed to create daemon stdout log");
    let stderr_log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(foreground_daemon_stderr_path(repo))
        .expect("failed to create daemon stderr log");
    let mut command = Command::new(installed_git_ai_path(repo));
    command
        .args(["bg", "run"])
        .current_dir(repo.test_home_path())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));
    configure_install_env(&mut command, repo);
    command.spawn().expect("failed to spawn installed daemon")
}

fn kill_installed_processes(repo: &TestRepo) {
    let script = format!(
        "$targets = @('{}','{}'); \
         Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
         Where-Object {{ $_.ExecutablePath -and ($targets -contains $_.ExecutablePath) }} | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
        installed_git_ai_path(repo).display(),
        repo.test_home_path()
            .join(".git-ai")
            .join("bin")
            .join("git.exe")
            .display()
    );
    let _ = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn wait_for_child_to_stay_alive(repo: &TestRepo, child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("failed to poll foreground daemon") {
            Some(status) => {
                let (stdout, stderr) = foreground_daemon_logs(repo);
                panic!(
                    "foreground daemon exited before reinstall started: {}\nstdout:\n{}\nstderr:\n{}",
                    status, stdout, stderr
                );
            }
            None if Instant::now() >= deadline => return,
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn wait_for_child_exit(repo: &TestRepo, child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("failed to poll foreground daemon") {
            Some(_) => return,
            None if Instant::now() >= deadline => {
                let (stdout, stderr) = foreground_daemon_logs(repo);
                panic!(
                    "foreground daemon was not stopped by reinstall within {:?}\nstdout:\n{}\nstderr:\n{}",
                    timeout, stdout, stderr
                );
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[test]
#[serial]
fn windows_install_script_reinstall_stops_running_daemon() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let installed_git_ai = installed_git_ai_path(&repo);
    assert!(
        installed_git_ai.exists(),
        "git-ai.exe should be installed at {}",
        installed_git_ai.display()
    );

    let mut daemon = spawn_installed_daemon(&repo);
    wait_for_child_to_stay_alive(&repo, &mut daemon, Duration::from_secs(2));

    let reinstall = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        reinstall.status.success(),
        "reinstall with daemon running should succeed\nstdout:\n{}\nstderr:\n{}",
        reinstall.stdout,
        reinstall.stderr
    );

    wait_for_child_exit(&repo, &mut daemon, Duration::from_secs(20));

    let version = run_installed_git_ai(&repo, &["--version"], Duration::from_secs(15));
    assert!(
        version.status.success(),
        "installed git-ai should remain usable after reinstall\nstdout:\n{}\nstderr:\n{}",
        version.stdout,
        version.stderr
    );

    kill_installed_processes(&repo);
}

#[test]
#[serial]
fn windows_daemon_creates_log_file() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let mut daemon = spawn_installed_daemon(&repo);
    wait_for_child_to_stay_alive(&repo, &mut daemon, Duration::from_secs(2));

    let log_path = wait_for_daemon_log_file(&repo, Duration::from_secs(15));
    wait_for_file_to_contain(&log_path, "daemon log initialized", Duration::from_secs(15));

    kill_installed_processes(&repo);
    let _ = daemon.wait();
}

#[test]
#[serial]
fn windows_git_extension_upgrade_requires_direct_git_ai_binary() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let result = run_installed_git_wrapper(
        &repo,
        &["ai", "upgrade", "--force"],
        Duration::from_secs(15),
    );
    let combined = format!("{}{}", result.stdout, result.stderr);

    assert!(
        !result.status.success(),
        "`git ai upgrade` should fail fast on Windows\nstdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );
    assert!(
        combined.contains("`git ai upgrade` is not supported on Windows"),
        "expected Windows upgrade guard message, got:\n{}",
        combined
    );
    assert!(
        combined.contains("git-ai upgrade"),
        "expected direct command hint, got:\n{}",
        combined
    );
}
