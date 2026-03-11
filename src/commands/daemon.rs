use crate::daemon::{ControlRequest, DaemonConfig, DaemonMode, send_control_request};
use serde_json::Value;
use std::path::PathBuf;

pub fn handle_daemon(args: &[String]) {
    if args.is_empty() || is_help(args[0].as_str()) {
        print_help();
        std::process::exit(0);
    }

    match args[0].as_str() {
        "start" | "run" => {
            if let Err(e) = handle_start(&args[1..]) {
                eprintln!("Failed to start daemon: {}", e);
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
        "trace" => {
            if let Err(e) = handle_trace(&args[1..]) {
                eprintln!("Failed to ingest trace payload: {}", e);
                std::process::exit(1);
            }
        }
        "checkpoint" => {
            if let Err(e) = handle_checkpoint(&args[1..]) {
                eprintln!("Failed to ingest checkpoint payload: {}", e);
                std::process::exit(1);
            }
        }
        "barrier" => {
            if let Err(e) = handle_barrier(&args[1..]) {
                eprintln!("Failed waiting for barrier: {}", e);
                std::process::exit(1);
            }
        }
        "reconcile" => {
            if let Err(e) = handle_reconcile(&args[1..]) {
                eprintln!("Failed to reconcile family: {}", e);
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
    let mut config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    if let Some(mode) = parse_mode_arg(args)? {
        config = config.with_mode(mode);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime
        .block_on(async move { crate::daemon::run_daemon(config).await })
        .map_err(|e| e.to_string())
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

fn handle_trace(args: &[String]) -> Result<(), String> {
    let repo = parse_repo_arg(args).ok_or_else(|| "--repo is required".to_string())?;
    let payload = parse_json_arg(args)?;
    let wait = has_flag(args, "--wait");
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    let request = ControlRequest::TraceIngest {
        repo_working_dir: repo,
        payload,
        wait: Some(wait),
    };
    let response =
        send_control_request(&config.control_socket_path, &request).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn handle_checkpoint(args: &[String]) -> Result<(), String> {
    let repo = parse_repo_arg(args).ok_or_else(|| "--repo is required".to_string())?;
    let payload = parse_json_arg(args)?;
    let wait = has_flag(args, "--wait");
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    let request = ControlRequest::CheckpointRun {
        repo_working_dir: repo,
        payload,
        wait: Some(wait),
    };
    let response =
        send_control_request(&config.control_socket_path, &request).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn handle_barrier(args: &[String]) -> Result<(), String> {
    let repo = parse_repo_arg(args).ok_or_else(|| "--repo is required".to_string())?;
    let seq = parse_seq_arg(args)?;
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    let request = ControlRequest::BarrierAppliedThroughSeq {
        repo_working_dir: repo,
        seq,
    };
    let response =
        send_control_request(&config.control_socket_path, &request).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn handle_reconcile(args: &[String]) -> Result<(), String> {
    let repo = parse_repo_arg(args).unwrap_or_else(default_repo_path);
    let config = DaemonConfig::from_default_paths().map_err(|e| e.to_string())?;
    let request = ControlRequest::ReconcileFamily {
        repo_working_dir: repo,
    };
    let response =
        send_control_request(&config.control_socket_path, &request).map_err(|e| e.to_string())?;
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

fn parse_json_arg(args: &[String]) -> Result<Value, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--json" && i + 1 < args.len() {
            return serde_json::from_str(&args[i + 1]).map_err(|e| e.to_string());
        }
        i += 1;
    }
    Err("--json '<payload>' is required".to_string())
}

fn parse_seq_arg(args: &[String]) -> Result<u64, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--seq" && i + 1 < args.len() {
            return args[i + 1]
                .parse::<u64>()
                .map_err(|e| format!("invalid --seq value: {}", e));
        }
        i += 1;
    }
    Err("--seq <number> is required".to_string())
}

fn parse_mode_arg(args: &[String]) -> Result<Option<DaemonMode>, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--mode" && i + 1 < args.len() {
            let value = args[i + 1].trim();
            let mode = DaemonMode::from_str(value)
                .ok_or_else(|| format!("invalid --mode value: {}", value))?;
            return Ok(Some(mode));
        }
        i += 1;
    }
    Ok(None)
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
    eprintln!("  git-ai daemon start [--mode shadow|write]");
    eprintln!("  git-ai daemon status [--repo <path>]");
    eprintln!("  git-ai daemon shutdown");
    eprintln!("  git-ai daemon trace --repo <path> --json '<payload>' [--wait]");
    eprintln!("  git-ai daemon checkpoint --repo <path> --json '<payload>' [--wait]");
    eprintln!("  git-ai daemon barrier --repo <path> --seq <n>");
    eprintln!("  git-ai daemon reconcile [--repo <path>]");
}
