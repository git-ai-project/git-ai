use crate::daemon::{
    ControlRequest, DaemonConfig, local_socket_connects_with_timeout, send_control_request,
};
use crate::utils::LockFile;
#[cfg(windows)]
use crate::utils::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, debug_log,
};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn handle_daemon(args: &[String]) {
    if args.is_empty() || is_help(args[0].as_str()) {
        print_help();
        std::process::exit(0);
    }

    match args[0].as_str() {
        "start" => {
            if let Err(e) = handle_start(&args[1..]) {
                eprintln!("Failed to start daemon: {}", e);
                std::process::exit(1);
            }
        }
        "run" => {
            if let Err(e) = handle_run(&args[1..]) {
                eprintln!("Failed to run daemon: {}", e);
                std::process::exit(1);
            }
        }
        "status" => {
            let repo = parse_repo_arg(&args[1..]).unwrap_or_else(default_repo_path);
            if let Err(e) = handle_status(repo) {
                eprintln!("Failed to get daemon status: {}", e);
                std::process::exit(1);
            }
        }
        "shutdown" => {
            if let Err(e) = handle_shutdown() {
                eprintln!("Failed to shut down daemon: {}", e);
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("Unknown daemon subcommand: {}", args[0]);
            print_help();
            std::process::exit(1);
        }
    }
}

fn handle_start(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--mode") {
        return Err("--mode is no longer supported; daemon always runs in write mode".to_string());
    }
    ensure_daemon_running_attached(Duration::from_secs(2)).map(|_| ())
}

/// Like `ensure_daemon_running`, but spawns with inherited stderr so the user
/// sees startup failures before the daemon detaches.
fn ensure_daemon_running_attached(timeout: Duration) -> Result<DaemonConfig, String> {
    let config = daemon_config_from_env_or_default_paths()?;
    if daemon_is_up(&config) {
        return Ok(config);
    }

    if daemon_startup_is_blocked(&config) {
        return Err(format!(
            "daemon startup blocked: lock held at {}",
            config.lock_path.display()
        ));
    }

    let mut child = spawn_daemon_run_with_piped_stderr(&config)?;
    let deadline = Instant::now() + timeout;
    loop {
        if daemon_is_up(&config) {
            // Daemon is healthy — let the detached child continue running.
            return Ok(config);
        }
        // Check if the child exited (startup failure).
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                let mut stderr_buf = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    use std::io::Read;
                    let _ = stderr.read_to_string(&mut stderr_buf);
                }
                let detail = if stderr_buf.trim().is_empty() {
                    format!("daemon process exited with {}", status)
                } else {
                    stderr_buf.trim().to_string()
                };
                return Err(format!("daemon failed to start: {}", detail));
            }
            Ok(Some(_)) => {
                // Exited successfully but sockets aren't up — unexpected.
                return Err("daemon process exited before sockets were ready".to_string());
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {:?} waiting for daemon sockets {} and {}",
                timeout,
                config.control_socket_path.display(),
                config.trace_socket_path.display()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn daemon_config_from_env_or_default_paths() -> Result<DaemonConfig, String> {
    DaemonConfig::from_env_or_default_paths().map_err(|e| e.to_string())
}

fn handle_run(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--mode") {
        return Err("--mode is no longer supported; daemon always runs in write mode".to_string());
    }
    let config = daemon_config_from_env_or_default_paths()?;
    let runtime_dir = daemon_runtime_dir(&config)?;
    std::env::set_current_dir(&runtime_dir).map_err(|e| {
        format!(
            "failed to set daemon runtime cwd to {}: {}",
            runtime_dir.display(),
            e
        )
    })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime
        .block_on(async move { crate::daemon::run_daemon(config).await })
        .map_err(|e| e.to_string())
}

pub(crate) fn ensure_daemon_running(timeout: Duration) -> Result<DaemonConfig, String> {
    let config = daemon_config_from_env_or_default_paths()?;
    if daemon_is_up(&config) {
        return Ok(config);
    }

    if daemon_startup_is_blocked(&config) {
        return Err(format!(
            "daemon startup blocked: lock held at {}",
            config.lock_path.display()
        ));
    }

    spawn_daemon_run_detached(&config)?;
    if wait_for_daemon_up(&config, timeout) {
        return Ok(config);
    }

    Err(format!(
        "timed out after {:?} waiting for daemon sockets {} and {}",
        timeout,
        config.control_socket_path.display(),
        config.trace_socket_path.display()
    ))
}

fn daemon_startup_is_blocked(config: &DaemonConfig) -> bool {
    if let Some(parent) = config.lock_path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }

    match LockFile::try_acquire(&config.lock_path) {
        Some(lock) => {
            drop(lock);
            false
        }
        None => true,
    }
}

fn daemon_is_up(config: &DaemonConfig) -> bool {
    local_socket_connects_with_timeout(&config.control_socket_path, Duration::from_millis(100))
        .is_ok()
        && local_socket_connects_with_timeout(&config.trace_socket_path, Duration::from_millis(100))
            .is_ok()
}

fn wait_for_daemon_up(config: &DaemonConfig, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if daemon_is_up(config) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn daemon_runtime_dir(config: &DaemonConfig) -> Result<PathBuf, String> {
    config.ensure_parent_dirs().map_err(|e| e.to_string())?;
    config
        .lock_path
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "daemon lock path has no parent".to_string())
}

fn spawn_daemon_run_detached(config: &DaemonConfig) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let runtime_dir = daemon_runtime_dir(config)?;
    let mut child = Command::new(exe);
    child
        .arg("daemon")
        .arg("run")
        .current_dir(&runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        let preferred_flags =
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB;
        child.creation_flags(preferred_flags);
        match child.spawn() {
            Ok(_) => Ok(()),
            Err(preferred_err) => {
                debug_log(&format!(
                    "detached daemon spawn with CREATE_BREAKAWAY_FROM_JOB failed, retrying without it: {}",
                    preferred_err
                ));
                child.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                child.spawn().map(|_| ()).map_err(|fallback_err| {
                    format!(
                        "failed to spawn detached daemon with flags {:#x}: {}; retry without CREATE_BREAKAWAY_FROM_JOB also failed: {}",
                        preferred_flags, preferred_err, fallback_err
                    )
                })
            }
        }
    }

    #[cfg(not(windows))]
    {
        child.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

fn spawn_daemon_run_with_piped_stderr(
    config: &DaemonConfig,
) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let runtime_dir = daemon_runtime_dir(config)?;
    let mut child = Command::new(exe);
    child
        .arg("daemon")
        .arg("run")
        .current_dir(&runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        let preferred_flags =
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB;
        child.creation_flags(preferred_flags);
        match child.spawn() {
            Ok(c) => Ok(c),
            Err(preferred_err) => {
                debug_log(&format!(
                    "detached daemon spawn with CREATE_BREAKAWAY_FROM_JOB failed, retrying without it: {}",
                    preferred_err
                ));
                child.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                child.spawn().map_err(|fallback_err| {
                    format!(
                        "failed to spawn detached daemon with flags {:#x}: {}; retry without CREATE_BREAKAWAY_FROM_JOB also failed: {}",
                        preferred_flags, preferred_err, fallback_err
                    )
                })
            }
        }
    }

    #[cfg(not(windows))]
    {
        child.spawn().map_err(|e| e.to_string())
    }
}

fn handle_status(repo_working_dir: String) -> Result<(), String> {
    let config = daemon_config_from_env_or_default_paths()?;
    let request = ControlRequest::StatusFamily { repo_working_dir };
    let response =
        send_control_request(&config.control_socket_path, &request).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn handle_shutdown() -> Result<(), String> {
    let config = daemon_config_from_env_or_default_paths()?;
    let response = send_control_request(&config.control_socket_path, &ControlRequest::Shutdown)
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn parse_repo_arg(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--repo" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn default_repo_path() -> String {
    PathBuf::from(".")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string()
}

fn is_help(value: &str) -> bool {
    value == "help" || value == "--help" || value == "-h"
}

fn print_help() {
    eprintln!("git-ai daemon - run and control git-ai daemon mode");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  git-ai daemon start");
    eprintln!("  git-ai daemon run");
    eprintln!("  git-ai daemon status [--repo <path>]");
    eprintln!("  git-ai daemon shutdown");
}
