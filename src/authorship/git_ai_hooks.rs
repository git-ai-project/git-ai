use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::git::repository::Repository;
#[cfg(windows)]
use crate::utils::CREATE_NO_WINDOW;
use serde::Serialize;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

const POST_NOTES_UPDATED_HOOK: &str = "post_notes_updated";
const HOOK_WAIT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONCURRENT_HOOK_COMMANDS: usize = 4;
const MAX_HOOK_BATCHES_IN_FLIGHT: usize = 2;
const MAX_HOOK_PAYLOAD_BYTES: usize = 16 * 1_024 * 1_024;
const HOOK_THREAD_STACK_BYTES: usize = 512 * 1_024;
const HOOK_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const HOOK_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);

static HOOK_BATCHES_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static HOOK_DISPATCHER: OnceLock<Mutex<Option<mpsc::SyncSender<HookBatch>>>> = OnceLock::new();

struct RepoHookContext {
    repo_url: String,
    repo_name: String,
    branch: String,
    is_default_branch: bool,
}

#[derive(Serialize)]
struct PostNotesUpdatedEntry<'a> {
    commit_sha: &'a str,
    repo_url: &'a str,
    repo_name: &'a str,
    branch: &'a str,
    is_default_branch: bool,
    note_content: &'a str,
}

struct HookBatchPermit;

impl HookBatchPermit {
    fn try_acquire() -> Option<Self> {
        HOOK_BATCHES_IN_FLIGHT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
                (in_flight < MAX_HOOK_BATCHES_IN_FLIGHT).then_some(in_flight + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for HookBatchPermit {
    fn drop(&mut self) {
        HOOK_BATCHES_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

struct HookBatch {
    commands: Vec<String>,
    payload: Arc<Vec<u8>>,
    completion_tx: mpsc::Sender<()>,
    _permit: HookBatchPermit,
}

/// Dispatch configured `git_ai_hooks.post_notes_updated` shell commands.
///
/// The hook input is always passed through stdin as a JSON array of 1..N note entries.
/// Up to four commands are started in parallel by a single bounded dispatcher. The caller
/// waits up to 3 seconds for completion, while the dispatcher keeps ownership of unfinished
/// children so repeated slow hooks cannot create unbounded processes or threads.
pub fn post_notes_updated(repo: &Repository, notes: &[(String, String)]) {
    if notes.is_empty() {
        return;
    }

    let Some(configured_commands) = Config::get().git_ai_hook_commands(POST_NOTES_UPDATED_HOOK)
    else {
        return;
    };
    let hook_commands = configured_commands.clone();
    if hook_commands.is_empty() {
        return;
    }

    let Some(permit) = HookBatchPermit::try_acquire() else {
        tracing::debug!(
            "[git_ai_hooks] Skipping post_notes_updated hooks because {} batches are already active or queued",
            MAX_HOOK_BATCHES_IN_FLIGHT
        );
        return;
    };

    let context = build_repo_hook_context(repo);
    let payload = notes
        .iter()
        .map(|(commit_sha, note_content)| PostNotesUpdatedEntry {
            commit_sha,
            repo_url: &context.repo_url,
            repo_name: &context.repo_name,
            branch: &context.branch,
            is_default_branch: context.is_default_branch,
            note_content,
        })
        .collect::<Vec<_>>();
    let payload_json =
        match crate::http::serialize_json_with_limit(&payload, MAX_HOOK_PAYLOAD_BYTES) {
            Ok(json) => json,
            Err(e) => {
                tracing::debug!(
                    "[git_ai_hooks] Failed to serialize post_notes_updated payload: {}",
                    e
                );
                return;
            }
        };

    let (completion_tx, completion_rx) = mpsc::channel();
    let batch = HookBatch {
        commands: hook_commands,
        payload: Arc::new(payload_json.into_bytes()),
        completion_tx,
        _permit: permit,
    };

    match try_send_with_restart(hook_dispatcher(), batch, start_hook_dispatcher) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(_)) => {
            tracing::debug!("[git_ai_hooks] Post-notes hook dispatcher queue is full");
            return;
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            tracing::debug!("[git_ai_hooks] Post-notes hook dispatcher is unavailable");
            return;
        }
    }

    match completion_rx.recv_timeout(HOOK_WAIT_TIMEOUT) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => tracing::debug!(
            "[git_ai_hooks] Continuing with post-notes hook batch after {}ms",
            HOOK_WAIT_TIMEOUT.as_millis()
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            tracing::debug!("[git_ai_hooks] Post-notes hook dispatcher stopped unexpectedly")
        }
    }
}

fn hook_dispatcher() -> &'static Mutex<Option<mpsc::SyncSender<HookBatch>>> {
    HOOK_DISPATCHER.get_or_init(|| Mutex::new(None))
}

fn start_hook_dispatcher() -> Option<mpsc::SyncSender<HookBatch>> {
    let (tx, rx) = mpsc::sync_channel(MAX_HOOK_BATCHES_IN_FLIGHT);
    match std::thread::Builder::new()
        .name("git-ai-note-hooks".to_string())
        .stack_size(HOOK_THREAD_STACK_BYTES)
        .spawn(move || hook_dispatch_loop(rx))
    {
        Ok(_) => Some(tx),
        Err(error) => {
            tracing::debug!("[git_ai_hooks] Failed to start hook dispatcher: {}", error);
            None
        }
    }
}

fn try_send_with_restart<T>(
    slot: &Mutex<Option<mpsc::SyncSender<T>>>,
    mut value: T,
    mut start: impl FnMut() -> Option<mpsc::SyncSender<T>>,
) -> Result<(), mpsc::TrySendError<T>> {
    let mut sender = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for _ in 0..2 {
        if sender.is_none() {
            *sender = start();
        }
        let Some(active_sender) = sender.as_ref() else {
            return Err(mpsc::TrySendError::Disconnected(value));
        };
        match active_sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(value)) => {
                return Err(mpsc::TrySendError::Full(value));
            }
            Err(mpsc::TrySendError::Disconnected(returned)) => {
                value = returned;
                *sender = None;
            }
        }
    }
    Err(mpsc::TrySendError::Disconnected(value))
}

fn hook_dispatch_loop(rx: mpsc::Receiver<HookBatch>) {
    while let Ok(batch) = rx.recv() {
        run_hook_batch(batch);
    }
}

fn run_hook_batch(batch: HookBatch) {
    let command_timeout = hook_command_timeout();
    for command_group in batch.commands.chunks(MAX_CONCURRENT_HOOK_COMMANDS) {
        let mut running_children = Vec::with_capacity(command_group.len());
        for hook_command in command_group {
            let mut child = match spawn_shell_command(hook_command) {
                Ok(child) => child,
                Err(e) => {
                    tracing::debug!(
                        "[git_ai_hooks] Failed to spawn post_notes_updated hook '{}': {}",
                        hook_command,
                        e
                    );
                    continue;
                }
            };

            let writer = if let Some(mut stdin) = child.stdin.take() {
                let payload_for_stdin = Arc::clone(&batch.payload);
                let command_for_log = hook_command.clone();
                match std::thread::Builder::new()
                    .name("git-ai-hook-stdin".to_string())
                    .stack_size(HOOK_THREAD_STACK_BYTES)
                    .spawn(move || {
                        use std::io::Write;
                        if let Err(e) = stdin.write_all(payload_for_stdin.as_slice()) {
                            tracing::debug!(
                                "[git_ai_hooks] Failed to write post_notes_updated stdin for '{}': {}",
                                command_for_log,
                                e
                            );
                        }
                    }) {
                    Ok(writer) => Some(writer),
                    Err(error) => {
                        tracing::debug!(
                            "[git_ai_hooks] Failed to start stdin writer for '{}': {}",
                            hook_command,
                            error
                        );
                        None
                    }
                }
            } else {
                tracing::debug!(
                    "[git_ai_hooks] Hook '{}' was spawned without a stdin pipe",
                    hook_command
                );
                None
            };

            running_children.push((hook_command.as_str(), child, writer));
        }

        wait_for_hook_children(running_children, command_timeout);
    }

    let _ = batch.completion_tx.send(());
}

fn hook_command_timeout() -> Duration {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(timeout) = std::env::var("GIT_AI_TEST_HOOK_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        return Duration::from_millis(timeout);
    }
    HOOK_COMMAND_TIMEOUT
}

type RunningHook<'a> = (&'a str, Child, Option<std::thread::JoinHandle<()>>);

fn wait_for_hook_children(mut children: Vec<RunningHook<'_>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !children.is_empty() && Instant::now() < deadline {
        let mut index = 0;
        while index < children.len() {
            match children[index].1.try_wait() {
                Ok(Some(status)) => {
                    let (command, _child, writer) = children.swap_remove(index);
                    if !status.success() {
                        tracing::debug!(
                            "[git_ai_hooks] Hook '{}' exited with status {}",
                            command,
                            status
                        );
                    }
                    join_hook_writer(writer);
                }
                Ok(None) => index += 1,
                Err(error) => {
                    let (command, mut child, writer) = children.swap_remove(index);
                    tracing::debug!(
                        "[git_ai_hooks] Failed waiting for hook '{}': {}",
                        command,
                        error
                    );
                    terminate_hook_child(&mut child);
                    join_hook_writer(writer);
                }
            }
        }
        if !children.is_empty() {
            std::thread::sleep(HOOK_CHILD_POLL_INTERVAL);
        }
    }

    for (command, mut child, writer) in children {
        tracing::debug!(
            "[git_ai_hooks] Hook '{}' exceeded the {}ms command timeout",
            command,
            timeout.as_millis()
        );
        terminate_hook_child(&mut child);
        join_hook_writer(writer);
    }
}

fn join_hook_writer(writer: Option<std::thread::JoinHandle<()>>) {
    if let Some(writer) = writer {
        let _ = writer.join();
    }
}

fn terminate_hook_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        // SAFETY: the hook child is started as the leader of this process group.
        if unsafe { libc::kill(process_group, libc::SIGKILL) } != 0 {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

pub fn post_notes_updated_single(repo: &Repository, commit_sha: &str, note_content: &str) {
    let note_batch = vec![(commit_sha.to_string(), note_content.to_string())];
    post_notes_updated(repo, &note_batch);
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("cmd");
    process.arg("/C").arg(command);
    process
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command);
    process
}

fn spawn_shell_command(command: &str) -> std::io::Result<Child> {
    let mut cmd = shell_command(command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn build_repo_hook_context(repo: &Repository) -> RepoHookContext {
    let repo_url = repo
        .get_default_remote()
        .ok()
        .flatten()
        .and_then(|remote_name| {
            repo.remotes_with_urls().ok().and_then(|remotes| {
                remotes
                    .into_iter()
                    .find(|(name, _)| name == &remote_name)
                    .map(|(_, url)| url)
            })
        })
        .unwrap_or_default();

    let repo_name = repo_url
        .rsplit('/')
        .next()
        .unwrap_or(&repo_url)
        .trim_end_matches(".git")
        .to_string();

    let branch = repo
        .head()
        .ok()
        .and_then(|head_ref| head_ref.shorthand().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let default_branch = repo
        .get_default_remote()
        .ok()
        .flatten()
        .and_then(|remote_name| {
            repo.remote_head(&remote_name).ok().map(|full| {
                full.strip_prefix(&format!("{}/", remote_name))
                    .unwrap_or(&full)
                    .to_string()
            })
        })
        .unwrap_or_else(|| "main".to_string());

    RepoHookContext {
        repo_url,
        repo_name,
        branch: branch.clone(),
        is_default_branch: branch == default_branch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_start_failure_leaves_slot_retryable() {
        let slot = Mutex::new(None);

        let failed = try_send_with_restart(&slot, 1_u8, || None);

        assert!(matches!(failed, Err(mpsc::TrySendError::Disconnected(1))));
        assert!(slot.lock().unwrap().is_none());

        let (sender, receiver) = mpsc::sync_channel(1);
        let mut sender = Some(sender);
        assert!(
            try_send_with_restart(&slot, 2_u8, || sender.take()).is_ok(),
            "a later call should retry dispatcher startup"
        );
        assert_eq!(receiver.recv().unwrap(), 2);
    }

    #[test]
    fn disconnected_dispatcher_restarts_without_replaying_value() {
        let (disconnected_sender, disconnected_receiver) = mpsc::sync_channel(1);
        drop(disconnected_receiver);
        let slot = Mutex::new(Some(disconnected_sender));
        let (replacement_sender, replacement_receiver) = mpsc::sync_channel(1);
        let mut replacement_sender = Some(replacement_sender);

        assert!(
            try_send_with_restart(&slot, 7_u8, || replacement_sender.take()).is_ok(),
            "a disconnected dispatcher should be replaced"
        );
        assert_eq!(replacement_receiver.recv().unwrap(), 7);
        assert!(replacement_receiver.try_recv().is_err());
    }
}
