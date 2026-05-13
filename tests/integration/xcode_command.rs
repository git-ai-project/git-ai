#![cfg(target_os = "macos")]

use crate::repos::test_repo::get_binary_path;
use serde_json::Value;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir;

fn write_launchctl_stub(bin_dir: &Path, exit_code: i32, log_path: &Path) {
    let stub_path = bin_dir.join("launchctl");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit {}\n",
        log_path.display(),
        exit_code
    );
    fs::write(&stub_path, script).expect("write launchctl stub");
    #[cfg(unix)]
    fs::set_permissions(&stub_path, fs::Permissions::from_mode(0o755))
        .expect("chmod launchctl stub");
}

fn write_watcher_binary(home: &Path) -> PathBuf {
    let watcher = home.join(".git-ai/bin/git-ai-xcode-watcher");
    fs::create_dir_all(watcher.parent().expect("watcher parent")).expect("create watcher dir");
    fs::write(&watcher, "#!/bin/sh\n").expect("write watcher stub");
    #[cfg(unix)]
    fs::set_permissions(&watcher, fs::Permissions::from_mode(0o755)).expect("chmod watcher stub");
    watcher
}

fn run_git_ai(home: &Path, fake_bin: &Path, args: &[&str]) -> Output {
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let path_with_stub = format!("{}:{}", fake_bin.display(), existing_path);

    let mut command = Command::new(get_binary_path());
    command
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", path_with_stub);

    command.output().expect("run git-ai command")
}

#[test]
fn test_xcode_add_path_command_updates_config_and_launch_agent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().to_path_buf();
    let fake_bin = home.join("fake-bin");
    let workspace = home.join("work/ios/AppA");
    let launchctl_log = home.join("launchctl.log");

    fs::create_dir_all(&fake_bin).expect("create fake bin");
    fs::create_dir_all(&workspace).expect("create workspace");
    write_launchctl_stub(&fake_bin, 0, &launchctl_log);
    let watcher = write_watcher_binary(&home);

    let output = run_git_ai(
        &home,
        &fake_bin,
        &["xcode", "add-path", workspace.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let canonical_workspace = fs::canonicalize(&workspace).expect("canonical workspace");
    assert!(stdout.contains("Added watch path"));
    assert!(stdout.contains("LaunchAgent updated"));

    let config: Value = serde_json::from_str(
        &fs::read_to_string(home.join(".git-ai/config.json")).expect("read config"),
    )
    .expect("parse config");
    assert_eq!(
        config["xcode_paths"],
        Value::Array(vec![Value::String(
            canonical_workspace.to_string_lossy().to_string()
        )])
    );

    let plist = fs::read_to_string(home.join("Library/LaunchAgents/com.gitai.xcode-watcher.plist"))
        .expect("read plist");
    assert!(plist.contains(&*watcher.to_string_lossy()));
    assert!(plist.contains(&*canonical_workspace.to_string_lossy()));

    let launchctl_log = fs::read_to_string(launchctl_log).expect("read launchctl log");
    assert!(launchctl_log.contains("bootstrap"));
    assert!(launchctl_log.contains("kickstart"));
}

#[test]
fn test_xcode_remove_path_command_disables_launch_agent_when_last_path_removed() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().to_path_buf();
    let fake_bin = home.join("fake-bin");
    let workspace = home.join("work/ios/AppA");
    let launchctl_log = home.join("launchctl.log");

    fs::create_dir_all(&fake_bin).expect("create fake bin");
    fs::create_dir_all(&workspace).expect("create workspace");
    write_launchctl_stub(&fake_bin, 0, &launchctl_log);
    write_watcher_binary(&home);

    let add_output = run_git_ai(
        &home,
        &fake_bin,
        &["xcode", "add-path", workspace.to_str().unwrap()],
    );
    assert!(add_output.status.success());

    let remove_output = run_git_ai(
        &home,
        &fake_bin,
        &["xcode", "remove-path", workspace.to_str().unwrap()],
    );
    assert!(
        remove_output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&remove_output.stdout),
        String::from_utf8_lossy(&remove_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&remove_output.stdout);
    assert!(stdout.contains("Removed watch path"));
    assert!(stdout.contains("Watcher LaunchAgent removed"));

    let config: Value = serde_json::from_str(
        &fs::read_to_string(home.join(".git-ai/config.json")).expect("read config"),
    )
    .expect("parse config");
    assert!(config.get("xcode_paths").is_none());
    assert!(
        !home
            .join("Library/LaunchAgents/com.gitai.xcode-watcher.plist")
            .exists()
    );
}
