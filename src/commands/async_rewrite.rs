use crate::config;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::{LockFile, debug_log};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, prelude::*,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ASYNC_REWRITE_SOCKET_FILE: &str = "async-rewrite.socket";
const ASYNC_REWRITE_WORKER_LOCK_FILE: &str = "async-rewrite-worker.lock";
const ASYNC_REWRITE_WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_REWRITE_WORKER_BOOT_TIMEOUT: Duration = Duration::from_millis(2_000);
const ASYNC_REWRITE_WORKER_BOOT_POLL: Duration = Duration::from_millis(50);
const ASYNC_REWRITE_WORKER_ACCEPT_POLL: Duration = Duration::from_millis(25);
const ASYNC_REWRITE_JOB_SCHEMA_VERSION: &str = "async_rewrite/1";
const ASYNC_REWRITE_WORKER_ACK_OK: &str = "ok";
const ENV_ASYNC_REWRITE_WORKER: &str = "GIT_AI_ASYNC_REWRITE_WORKER";
#[cfg(unix)]
const UNIX_SOCKET_SAFE_MAX_PATH_BYTES: usize = 100;

pub const ASYNC_REWRITE_WORKER_SUBCOMMAND: &str = "async-rewrite-worker";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AsyncRewriteJob {
    schema_version: String,
    rewrite_log_event: RewriteLogEvent,
    commit_author: String,
    supress_output: bool,
    apply_side_effects: bool,
}

impl AsyncRewriteJob {
    fn new(
        rewrite_log_event: RewriteLogEvent,
        commit_author: String,
        supress_output: bool,
        apply_side_effects: bool,
    ) -> Self {
        Self {
            schema_version: ASYNC_REWRITE_JOB_SCHEMA_VERSION.to_string(),
            rewrite_log_event,
            commit_author,
            supress_output,
            apply_side_effects,
        }
    }
}

enum SendAttempt {
    Sent,
    NoActiveWorker,
}

#[derive(Clone)]
struct AsyncSocketTarget {
    name: Name<'static>,
    cleanup_path: Option<PathBuf>,
}

pub fn handle_rewrite_log_event(
    repository: &mut Repository,
    rewrite_log_event: RewriteLogEvent,
    commit_author: String,
    supress_output: bool,
    apply_side_effects: bool,
) {
    if !should_process_async(&rewrite_log_event) {
        repository.handle_rewrite_log_event(
            rewrite_log_event,
            commit_author,
            supress_output,
            apply_side_effects,
        );
        return;
    }

    let job = AsyncRewriteJob::new(
        rewrite_log_event.clone(),
        commit_author.clone(),
        supress_output,
        apply_side_effects,
    );

    match enqueue_async_rewrite_job(repository, &job) {
        Ok(()) => {
            debug_log("Rewrite event handed off; this is being processed async");
        }
        Err(err) => {
            eprintln!("git-ai async rewrite handoff failed: {}", err);
            std::process::exit(1);
        }
    }
}

pub fn handle_async_rewrite_worker(args: &[String]) {
    if let Err(err) = run_async_rewrite_worker(args) {
        debug_log(&format!("Async rewrite worker failed: {}", err));
        std::process::exit(1);
    }
}

fn should_process_async(rewrite_log_event: &RewriteLogEvent) -> bool {
    if !config::Config::get().feature_flags().async_rewrite_hooks {
        return false;
    }

    matches!(
        rewrite_log_event,
        RewriteLogEvent::Commit { .. }
            | RewriteLogEvent::CommitAmend { .. }
            | RewriteLogEvent::RebaseComplete { .. }
            | RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::MergeSquash { .. }
    )
}

fn enqueue_async_rewrite_job(repo: &Repository, job: &AsyncRewriteJob) -> Result<(), GitAiError> {
    let socket_target = async_socket_target(repo)?;

    match try_send_job(&socket_target.name, job)? {
        SendAttempt::Sent => return Ok(()),
        SendAttempt::NoActiveWorker => {}
    }

    let repo_path = repo.workdir()?.to_string_lossy().to_string();
    let mut worker_child = spawn_async_worker_process(&repo_path)?;
    debug_log("Spawned async rewrite worker process");

    let boot_deadline = Instant::now() + ASYNC_REWRITE_WORKER_BOOT_TIMEOUT;
    while Instant::now() < boot_deadline {
        if let Some(status) = worker_child.try_wait().map_err(GitAiError::IoError)? {
            debug_log(&format!(
                "Async rewrite worker exited before socket handoff completed: {}",
                status
            ));
            break;
        }
        match try_send_job(&socket_target.name, job)? {
            SendAttempt::Sent => return Ok(()),
            SendAttempt::NoActiveWorker => std::thread::sleep(ASYNC_REWRITE_WORKER_BOOT_POLL),
        }
    }

    match try_send_job(&socket_target.name, job)? {
        SendAttempt::Sent => Ok(()),
        SendAttempt::NoActiveWorker => Err(GitAiError::Generic(
            "Unable to deliver async rewrite job after spawn/retry".to_string(),
        )),
    }
}

fn spawn_async_worker_process(repo_path: &str) -> Result<Child, GitAiError> {
    let exe = crate::utils::current_git_ai_exe_path()?;
    let mut cmd = Command::new(exe);
    cmd.arg(ASYNC_REWRITE_WORKER_SUBCOMMAND)
        .arg("--repo")
        .arg(repo_path)
        .env(crate::commands::git_hook_handlers::ENV_SKIP_ALL_HOOKS, "1")
        .env(ENV_ASYNC_REWRITE_WORKER, "1")
        // In debug builds, GIT_AI=git forces argv[0]-independent git-proxy mode.
        // Worker must run in git-ai subcommand mode, so strip this override.
        .env_remove("GIT_AI")
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        if !crate::utils::is_interactive_terminal() {
            cmd.creation_flags(crate::utils::CREATE_NO_WINDOW);
        }
    }

    cmd.spawn().map_err(GitAiError::IoError)
}

fn try_send_job(socket_name: &Name<'_>, job: &AsyncRewriteJob) -> Result<SendAttempt, GitAiError> {
    let mut stream = match LocalSocketStream::connect(socket_name.borrow()) {
        Ok(stream) => stream,
        Err(err) if is_no_active_worker_error(&err) => return Ok(SendAttempt::NoActiveWorker),
        Err(err) => {
            return Err(GitAiError::Generic(format!(
                "Failed to connect to async rewrite socket: {}",
                err
            )));
        }
    };

    let mut payload = serde_json::to_vec(job)?;
    payload.push(b'\n');
    stream.write_all(&payload).map_err(|err| {
        GitAiError::Generic(format!(
            "Failed to write async rewrite job to socket: {}",
            err
        ))
    })?;
    stream.flush().map_err(|err| {
        GitAiError::Generic(format!(
            "Failed to flush async rewrite job to socket: {}",
            err
        ))
    })?;

    let mut ack = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        let bytes_read = reader.read_line(&mut ack).map_err(|err| {
            GitAiError::Generic(format!(
                "Failed to read async rewrite worker ACK from socket: {}",
                err
            ))
        })?;
        if bytes_read == 0 {
            return Err(GitAiError::Generic(
                "Async rewrite worker closed socket without ACK".to_string(),
            ));
        }
    }
    if ack.trim() != ASYNC_REWRITE_WORKER_ACK_OK {
        return Err(GitAiError::Generic(format!(
            "Async rewrite worker returned unexpected ACK: {}",
            ack.trim()
        )));
    }

    Ok(SendAttempt::Sent)
}

fn run_async_rewrite_worker(args: &[String]) -> Result<(), GitAiError> {
    let repo_path = parse_repo_path(args)?;
    let mut repo = find_repository_in_path(&repo_path)?;

    let lock_path = repo.storage.ai_dir.join(ASYNC_REWRITE_WORKER_LOCK_FILE);
    let Some(_lock) = LockFile::try_acquire(&lock_path) else {
        debug_log("Async rewrite worker already active for this repository");
        return Ok(());
    };

    let socket_target = async_socket_target(&repo)?;
    if let Some(socket_path) = socket_target.cleanup_path.as_ref() {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        cleanup_socket_path_if_present(socket_path)?;
    }

    let listener = ListenerOptions::new()
        .name(socket_target.name.borrow())
        .nonblocking(ListenerNonblockingMode::Accept)
        .create_sync()
        .map_err(|err| GitAiError::Generic(format!("Failed to bind async socket: {}", err)))?;

    debug_log("Async rewrite worker is listening for jobs");

    let mut idle_deadline = Instant::now() + ASYNC_REWRITE_WORKER_IDLE_TIMEOUT;
    loop {
        match listener.accept() {
            Ok(mut stream) => {
                if let Err(err) = process_job_stream(&mut repo, &mut stream) {
                    debug_log(&format!("Failed processing async rewrite job: {}", err));
                    let _ = stream.write_all(b"err\n");
                } else {
                    let _ =
                        stream.write_all(format!("{}\n", ASYNC_REWRITE_WORKER_ACK_OK).as_bytes());
                }
                idle_deadline = Instant::now() + ASYNC_REWRITE_WORKER_IDLE_TIMEOUT;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= idle_deadline {
                    break;
                }
                std::thread::sleep(ASYNC_REWRITE_WORKER_ACCEPT_POLL);
            }
            Err(err) => {
                if let Some(socket_path) = socket_target.cleanup_path.as_ref() {
                    cleanup_socket_path_if_present(socket_path)?;
                }
                return Err(GitAiError::Generic(format!(
                    "Async worker accept failed: {}",
                    err
                )));
            }
        }
    }

    drop(listener);
    if let Some(socket_path) = socket_target.cleanup_path.as_ref() {
        cleanup_socket_path_if_present(socket_path)?;
    }
    debug_log("Async rewrite worker idle timeout reached; exiting");
    Ok(())
}

fn process_job_stream(
    repo: &mut Repository,
    stream: &mut LocalSocketStream,
) -> Result<(), GitAiError> {
    let mut payload = String::new();
    let bytes_read = {
        let mut reader = BufReader::new(&mut *stream);
        reader.read_line(&mut payload)?
    };
    if bytes_read == 0 {
        return Err(GitAiError::Generic(
            "Received empty async rewrite payload".to_string(),
        ));
    }

    let payload = payload.trim();
    if payload.is_empty() {
        return Err(GitAiError::Generic(
            "Received empty async rewrite payload".to_string(),
        ));
    }

    let job = serde_json::from_str::<AsyncRewriteJob>(payload)?;

    if job.schema_version != ASYNC_REWRITE_JOB_SCHEMA_VERSION {
        return Err(GitAiError::Generic(format!(
            "Ignoring async rewrite payload with unsupported schema version {}",
            job.schema_version
        )));
    }

    repo.handle_rewrite_log_event(
        job.rewrite_log_event,
        job.commit_author,
        job.supress_output,
        job.apply_side_effects,
    );

    Ok(())
}

fn parse_repo_path(args: &[String]) -> Result<String, GitAiError> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--repo" {
            if i + 1 >= args.len() {
                return Err(GitAiError::Generic(
                    "Missing value for async worker --repo".to_string(),
                ));
            }
            return Ok(args[i + 1].clone());
        }
        i += 1;
    }

    let cwd = std::env::current_dir()?;
    Ok(cwd.to_string_lossy().to_string())
}

fn async_rewrite_socket_path(repo: &Repository) -> PathBuf {
    repo.storage.ai_dir.join(ASYNC_REWRITE_SOCKET_FILE)
}

fn async_socket_target(repo: &Repository) -> Result<AsyncSocketTarget, GitAiError> {
    let socket_path = resolve_socket_path(&async_rewrite_socket_path(repo));

    #[cfg(unix)]
    if unix_socket_path_is_safe(&socket_path)
        && let Ok(name) = socket_path.to_path_buf().to_fs_name::<GenericFilePath>()
    {
        return Ok(AsyncSocketTarget {
            name: name.into_owned(),
            cleanup_path: Some(socket_path),
        });
    }

    #[cfg(not(unix))]
    if let Ok(name) = socket_path.to_path_buf().to_fs_name::<GenericFilePath>() {
        return Ok(AsyncSocketTarget {
            name: name.into_owned(),
            cleanup_path: Some(socket_path),
        });
    }

    let mut hasher = Sha256::new();
    let canonical_ai_dir = repo
        .storage
        .ai_dir
        .canonicalize()
        .unwrap_or_else(|_| repo.storage.ai_dir.clone());
    hasher.update(canonical_ai_dir.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let namespace = format!("git-ai-async-{}", &hash[..24]);
    let name = namespace.to_ns_name::<GenericNamespaced>().map_err(|err| {
        GitAiError::Generic(format!(
            "Failed to derive async rewrite namespaced socket: {}",
            err
        ))
    })?;

    Ok(AsyncSocketTarget {
        name: name.into_owned(),
        cleanup_path: None,
    })
}

#[cfg(unix)]
fn unix_socket_path_is_safe(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() < UNIX_SOCKET_SAFE_MAX_PATH_BYTES
}

fn resolve_socket_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };

    match parent.canonicalize() {
        Ok(canonical_parent) => canonical_parent.join(file_name),
        Err(_) => path.to_path_buf(),
    }
}

fn cleanup_socket_path_if_present(path: &Path) -> Result<(), GitAiError> {
    #[cfg(unix)]
    {
        if path.exists()
            && let Err(err) = std::fs::remove_file(path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(GitAiError::IoError(err));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn is_no_active_worker_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    )
}
