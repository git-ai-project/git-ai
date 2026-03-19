use crate::daemon::{ControlRequest, DaemonConfig, send_control_request};
use interprocess::local_socket::LocalSocketStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
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
    ensure_daemon_running(Duration::from_secs(2)).map(|_| ())
}

fn handle_run(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--mode") {
        return Err("--mode is no longer supported; daemon always runs in write mode".to_string());
    }
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    let runtime_dir = daemon_runtime_dir(&config)?;
    std::env::set_current_dir(&runtime_dir)
        .map_err(|e| format!("failed to set daemon runtime cwd to {}: {}", runtime_dir.display(), e))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime
        .block_on(async move { crate::daemon::run_daemon(config).await })
        .map_err(|e| e.to_string())
}

pub(crate) fn ensure_daemon_running(timeout: Duration) -> Result<DaemonConfig, String> {
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    if daemon_is_up(&config) {
        return Ok(config);
    }

    spawn_daemon_run_detached(&config)?;
    if wait_for_daemon_up(&config, timeout) {
        return Ok(config);
    }

    Err(format!(
        "timed out after {:?} waiting for daemon socket {}",
        timeout,
        config.control_socket_path.display()
    ))
}

fn daemon_is_up(config: &DaemonConfig) -> bool {
    if !config.control_socket_path.exists() {
        return false;
    }

    let socket_path = config.control_socket_path.to_string_lossy().to_string();
    let (tx, rx) = mpsc::sync_channel(1);
    let spawn_result = thread::Builder::new()
        .name("git-ai-daemon-liveness-probe".to_string())
        .spawn(move || {
            let ready = LocalSocketStream::connect(socket_path.as_str()).is_ok();
            let _ = tx.send(ready);
        });
    if spawn_result.is_err() {
        return false;
    }

    matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(true))
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

    child.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn handle_status(repo_working_dir: String) -> Result<(), String> {
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
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
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
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
