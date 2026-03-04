use crate::async_worker::job::{AsyncJob, AsyncJobType};
use crate::async_worker::socket::{self, platform};
use crate::config;
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;
use std::path::Path;

/// Guard environment variable to prevent recursive async worker spawning.
const ASYNC_WORKER_GUARD_ENV: &str = "GIT_AI_ASYNC_WORKER_PROCESS";

/// Attempt to dispatch a rewrite log event asynchronously.
///
/// Returns `true` if the event was successfully dispatched to an async worker,
/// meaning the caller should NOT process the event synchronously.
///
/// Returns `false` if async dispatch failed or is not enabled, meaning the caller
/// should fall back to synchronous processing.
pub fn try_dispatch_async(
    repository: &Repository,
    rewrite_log_event: &RewriteLogEvent,
    commit_author: &str,
    suppress_output: bool,
    apply_side_effects: bool,
) -> bool {
    // Check if async worker feature flag is enabled
    let config = config::Config::get();
    if !config.feature_flags().async_worker {
        return false;
    }

    // Don't dispatch if we ARE the async worker (prevent recursion)
    if std::env::var(ASYNC_WORKER_GUARD_ENV).as_deref() == Ok("1") {
        debug_log("Skipping async dispatch: we are the async worker process");
        return false;
    }

    let ai_dir = &repository.storage.ai_dir;
    let socket_path = socket::socket_path_for_ai_dir(ai_dir);

    // Build the job payload
    let job = AsyncJob {
        job_type: AsyncJobType::RewriteLogEvent,
        repo_global_args: repository.global_args_for_exec(),
        git_dir: repository.path().to_string_lossy().to_string(),
        git_common_dir: repository.common_dir().to_string_lossy().to_string(),
        workdir: repository
            .workdir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        rewrite_log_event: rewrite_log_event.clone(),
        commit_author: commit_author.to_string(),
        suppress_output,
        apply_side_effects,
    };

    let wire_bytes = match job.to_wire_bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            debug_log(&format!("Failed to serialize async job: {}", e));
            return false;
        }
    };

    // The payload is the JSON part (skip the 4-byte length prefix since
    // platform::try_send_to_socket will use write_message which adds its own)
    let json_payload = &wire_bytes[4..];

    // Attempt 1: Try to send to an existing worker
    match platform::try_send_to_socket(&socket_path, json_payload) {
        Ok(true) => {
            debug_log("Async job dispatched to existing worker");
            return true;
        }
        Ok(false) => {
            debug_log("No active async worker found, spawning one...");
        }
        Err(e) => {
            debug_log(&format!("Error sending to socket: {}", e));
        }
    }

    // Attempt 2: Spawn a new worker and send to it
    if spawn_worker_and_send(ai_dir, &socket_path, json_payload) {
        debug_log("Async job dispatched to newly spawned worker");
        return true;
    }

    // Attempt 3: Maybe another worker was spawned in between - try one more time
    match platform::try_send_to_socket(&socket_path, json_payload) {
        Ok(true) => {
            debug_log("Async job dispatched on retry to existing worker");
            true
        }
        Ok(false) | Err(_) => {
            debug_log("Async dispatch failed after all attempts, falling back to sync");
            false
        }
    }
}

/// Spawn an async worker process and wait for it to bind to the socket,
/// then send the job payload.
fn spawn_worker_and_send(ai_dir: &Path, socket_path: &Path, json_payload: &[u8]) -> bool {
    let socket_path_str = socket_path.to_string_lossy().to_string();
    let ai_dir_str = ai_dir.to_string_lossy().to_string();

    let spawned = crate::utils::spawn_internal_git_ai_subcommand(
        "async-worker",
        &["--socket-path", &socket_path_str, "--ai-dir", &ai_dir_str],
        ASYNC_WORKER_GUARD_ENV,
        &[],
    );

    if !spawned {
        debug_log("Failed to spawn async worker process");
        return false;
    }

    // Wait for the worker to bind to the socket (poll with timeout)
    let max_wait = std::time::Duration::from_secs(3);
    let start = std::time::Instant::now();
    let poll_interval = std::time::Duration::from_millis(25);

    while start.elapsed() < max_wait {
        std::thread::sleep(poll_interval);

        // Check if the socket exists and is accepting connections
        match platform::try_send_to_socket(socket_path, json_payload) {
            Ok(true) => return true,
            Ok(false) => continue, // Socket not ready yet
            Err(_) => continue,
        }
    }

    debug_log("Timed out waiting for async worker to start");
    false
}
