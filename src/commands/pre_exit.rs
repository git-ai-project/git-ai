//! Handle the `pre-exit` command.
//!
//! Waits for the git-ai daemon to finish all in-flight work and telemetry
//! flushing before the process exits. This is useful in cloud sandboxes where
//! the container may be torn down immediately after a build step.

use crate::daemon::{ControlRequest, send_control_request};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
const LOG_INTERVAL_SECONDS: u64 = 5;

#[derive(Debug, Deserialize)]
struct PreExitResponse {
    done: bool,
    timed_out: bool,
    metrics_remaining: usize,
    notes_remaining: usize,
}

/// Handle `git-ai pre-exit [--timeout <seconds>]`.
///
/// Blocks until the daemon reports it has drained all work and flushed
/// telemetry, or until the configured timeout elapses. Prints a progress
/// message every few seconds while waiting.
pub(crate) fn handle_pre_exit(args: &[String]) {
    let mut timeout_secs = DEFAULT_TIMEOUT_SECONDS;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--timeout" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("pre-exit: --timeout requires a value");
                    std::process::exit(1);
                });
                timeout_secs = value.parse::<u64>().unwrap_or_else(|_| {
                    eprintln!("pre-exit: --timeout must be a positive integer");
                    std::process::exit(1);
                });
            }
            "--help" | "-h" | "help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                eprintln!("pre-exit: unknown argument: {}", arg);
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let config = match crate::commands::daemon::ensure_daemon_running(Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pre-exit: failed to reach git-ai background service: {}", e);
            std::process::exit(1);
        }
    };

    let request = ControlRequest::PreExit { timeout_secs };

    eprintln!("pre-exit: waiting for background service to finish work...");

    let running = Arc::new(AtomicBool::new(true));
    let running_for_thread = Arc::clone(&running);

    let progress_handle = thread::spawn(move || {
        while running_for_thread.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(LOG_INTERVAL_SECONDS));
            if running_for_thread.load(Ordering::Relaxed) {
                eprintln!("pre-exit: still waiting for background service...");
            }
        }
    });

    let response = send_control_request(&config.control_socket_path, &request);
    running.store(false, Ordering::Relaxed);
    let _ = progress_handle.join();

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "pre-exit: failed to send request to background service: {}",
                e
            );
            std::process::exit(1);
        }
    };

    if !response.ok {
        let message = response
            .data
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "background service returned an error".to_string());
        eprintln!("pre-exit: {}", message);
        std::process::exit(1);
    }

    let data = match response.data {
        Some(value) => match serde_json::from_value::<PreExitResponse>(value) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("pre-exit: failed to parse response: {}", e);
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("pre-exit: background service returned an empty response");
            std::process::exit(1);
        }
    };

    if data.timed_out {
        eprintln!("pre-exit: timed out after {}s", timeout_secs);
        std::process::exit(1);
    }

    if !data.done {
        eprintln!(
            "pre-exit: background service could not finish ({} metrics, {} notes remaining)",
            data.metrics_remaining, data.notes_remaining
        );
        std::process::exit(1);
    }

    eprintln!("pre-exit: background service finished");
}

fn print_usage() {
    eprintln!("Usage: git-ai pre-exit [--timeout <seconds>]");
    eprintln!("  Wait for the background service to finish all work and telemetry flushing.");
    eprintln!();
    eprintln!("Options:");
    eprintln!(
        "  --timeout <seconds>  Maximum time to wait (default: {})",
        DEFAULT_TIMEOUT_SECONDS
    );
    eprintln!("  -h, --help           Show this help");
}
