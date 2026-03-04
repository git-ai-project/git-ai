use crate::async_worker::job::AsyncJob;
use crate::async_worker::socket::{platform, read_message};
use crate::git::repository::find_repository;
use crate::utils::debug_log;
use std::path::PathBuf;
use std::time::Duration;

/// How long the worker waits for new jobs after completing one before shutting down.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the async worker process.
///
/// This function:
/// 1. Binds to the socket atomically (exits if another worker owns it)
/// 2. Accepts jobs on the socket and processes them sequentially
/// 3. After each job, waits IDLE_TIMEOUT for more work
/// 4. Cleans up the socket and exits when idle
pub fn run_async_worker(socket_path_str: &str, ai_dir_str: &str) {
    let socket_path = PathBuf::from(socket_path_str);
    let _ai_dir = PathBuf::from(ai_dir_str);

    // Set up logging for the worker process
    debug_log(&format!(
        "Async worker starting, socket: {}",
        socket_path.display()
    ));

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        debug_log(&format!("Failed to create socket parent directory: {}", e));
        return;
    }

    // Bind to the socket atomically
    #[cfg(unix)]
    let listener = match platform::bind_socket(&socket_path) {
        Ok(listener) => listener,
        Err(e) => {
            debug_log(&format!(
                "Failed to bind socket (another worker likely owns it): {}",
                e
            ));
            return;
        }
    };

    #[cfg(windows)]
    {
        if let Err(e) = platform::bind_socket(&socket_path) {
            debug_log(&format!(
                "Failed to acquire lock (another worker likely owns it): {}",
                e
            ));
            return;
        }
    }

    debug_log("Async worker bound to socket, ready to accept jobs");

    // Main job processing loop
    loop {
        #[cfg(unix)]
        let connection = platform::accept_with_timeout(&listener, IDLE_TIMEOUT);

        #[cfg(windows)]
        let connection = {
            // On Windows, we'd accept via named pipe server
            // For now, use a simplified approach
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Windows named pipe server not yet implemented",
            ))
        };

        match connection {
            Ok(Some(mut stream)) => {
                debug_log("Accepted connection from client");

                // Read the job message
                match read_message(&mut stream) {
                    Ok(Some(payload)) => {
                        debug_log(&format!("Received job payload ({} bytes)", payload.len()));
                        process_job(&payload);
                    }
                    Ok(None) => {
                        debug_log("Client disconnected without sending data");
                    }
                    Err(e) => {
                        debug_log(&format!("Error reading job from socket: {}", e));
                    }
                }
            }
            Ok(None) => {
                // Timeout - no new connections
                debug_log("Async worker idle timeout reached, shutting down");
                break;
            }
            Err(e) => {
                debug_log(&format!("Error accepting connection: {}", e));
                break;
            }
        }
    }

    // Clean up
    platform::cleanup_socket(&socket_path);
    debug_log("Async worker shut down cleanly");
}

/// Process a single async job.
fn process_job(payload: &[u8]) {
    let job = match AsyncJob::from_json_bytes(payload) {
        Ok(job) => job,
        Err(e) => {
            debug_log(&format!("Failed to deserialize async job: {}", e));
            return;
        }
    };

    debug_log(&format!(
        "Processing async job: git_dir={}, event_type={:?}",
        job.git_dir, job.job_type
    ));

    // Reconstruct the Repository from the snapshotted state
    let mut repository = match find_repository(&job.repo_global_args) {
        Ok(repo) => repo,
        Err(e) => {
            debug_log(&format!(
                "Failed to reconstruct repository from global_args {:?}: {}",
                job.repo_global_args, e
            ));
            return;
        }
    };

    // Execute the rewrite log event handling
    repository.handle_rewrite_log_event(
        job.rewrite_log_event,
        job.commit_author,
        job.suppress_output,
        job.apply_side_effects,
    );

    debug_log("Async job processed successfully");
}
