use crate::commands::git_hook_handlers::ENV_SKIP_MANAGED_HOOKS;
use crate::config;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::find_repository;
use crate::git::repository::Repository;
#[cfg(windows)]
use crate::utils::CREATE_NO_WINDOW;
#[cfg(windows)]
use crate::utils::is_interactive_terminal;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward_signal_handler(sig: libc::c_int) {
    let pgid = CHILD_PGID.load(Ordering::Relaxed);
    if pgid > 0 {
        unsafe {
            // Send to the whole child process group
            let _ = libc::kill(-pgid, sig);
        }
    }
}

#[cfg(unix)]
fn install_forwarding_handlers() {
    unsafe {
        let handler = forward_signal_handler as *const () as usize;
        let _ = libc::signal(libc::SIGTERM, handler);
        let _ = libc::signal(libc::SIGINT, handler);
        let _ = libc::signal(libc::SIGHUP, handler);
        let _ = libc::signal(libc::SIGQUIT, handler);
    }
}

#[cfg(unix)]
fn uninstall_forwarding_handlers() {
    unsafe {
        let _ = libc::signal(libc::SIGTERM, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGINT, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGHUP, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGQUIT, libc::SIG_DFL);
    }
}

pub fn handle_git(args: &[String]) {
    // If we're being invoked from a shell completion context, bypass git-ai logic
    // and delegate directly to the real git so existing completion scripts work.
    if in_shell_completion_context() {
        let orig_args: Vec<String> = std::env::args().skip(1).collect();
        proxy_to_git(&orig_args, true, None);
        return;
    }

    let parsed = parse_git_cli_args(args);

    // Read-only invocations don't need wrapper state (the daemon fast-paths
    // their trace events and never processes them through the normalizer).
    // Skip the invocation_id so we can also suppress trace2 for them,
    // avoiding unnecessary daemon work and wrapper_states memory leaks.
    //
    // Use is_definitely_read_only_invocation (not is_definitely_read_only_command)
    // so that subcommand-gated read-only calls like `git stash list` and
    // `git worktree list` are also suppressed — these account for thousands
    // of Zed IDE invocations per session.
    let is_read_only = {
        let subcommand = parsed.command_args.first().map(String::as_str);
        parsed.command.as_deref().is_some_and(|cmd| {
            crate::git::command_classification::is_definitely_read_only_invocation(cmd, subcommand)
        })
    };

    if is_read_only {
        let exit_status = proxy_to_git(args, false, None);
        exit_with_status(exit_status);
    }

    // Repo-creating commands (clone, init) have no meaningful pre/post
    // repo state — the target repo doesn't exist yet. The wrapper would
    // either capture nothing (clone from outside a repo) or the wrong
    // repo (clone from inside a different repo). Skip the invocation_id
    // so the daemon doesn't wait for wrapper state that never arrives or
    // is misleading; trace2 events still flow normally (trace2 suppression
    // requires *both* no invocation_id and a read-only command).
    let is_repo_creating = parsed
        .command
        .as_deref()
        .is_some_and(|cmd| matches!(cmd, "clone" | "init"));

    if is_repo_creating {
        let exit_status = proxy_to_git(args, false, None);
        exit_with_status(exit_status);
    }

    // Initialize the daemon telemetry handle so we can send wrapper state.
    // If the daemon isn't available, fall back to a plain passthrough proxy
    // (no invocation_id, no wrapper state, no extra GIT_* env vars).
    let daemon_connected = matches!(
        crate::daemon::telemetry_handle::init_daemon_telemetry_handle(),
        crate::daemon::telemetry_handle::DaemonTelemetryInitResult::Connected
    );

    if !daemon_connected {
        let exit_status = proxy_to_git(args, false, None);
        exit_with_status(exit_status);
    }

    let repository = find_repository(&parsed.global_args).ok();
    let worktree = repository.as_ref().and_then(|r| r.workdir().ok());

    let pre_state = worktree
        .as_deref()
        .and_then(crate::git::repo_state::read_head_state_for_worktree);
    let invocation_id = crate::uuid::generate_v4();

    // Send pre-state BEFORE running git so it's available when the daemon
    // processes the atexit trace event and starts the wrapper state timeout.
    send_wrapper_pre_state_to_daemon(&invocation_id, worktree.as_deref(), &pre_state);

    let exit_status = proxy_to_git(args, false, Some(&invocation_id));

    let post_state = worktree
        .as_deref()
        .and_then(crate::git::repo_state::read_head_state_for_worktree);

    send_wrapper_post_state_to_daemon(&invocation_id, worktree.as_deref(), &post_state);

    // After a successful commit, wait briefly for the daemon to produce an
    // authorship note so we can show stats inline (same UX as plain wrapper mode).
    if exit_status.success()
        && parsed.command.as_deref() == Some("commit")
        && let Some(repo) = repository.as_ref()
    {
        maybe_show_async_post_commit_stats(&parsed, repo);
    }

    exit_with_status(exit_status);
}

#[cfg(feature = "test-support")]
pub fn resolve_alias_invocation(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<ParsedGitInvocation> {
    use std::collections::HashSet;

    let mut current = parsed_args.clone();
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let command = match current.command.as_deref() {
            Some(command) => command,
            None => return Some(current),
        };

        if !seen.insert(command.to_string()) {
            return None;
        }

        let key = format!("alias.{}", command);
        let alias_value = match repository.config_get_str(&key) {
            Ok(Some(value)) => value,
            _ => return Some(current),
        };

        let alias_tokens = parse_alias_tokens(&alias_value)?;

        let mut expanded_args = Vec::new();
        expanded_args.extend(current.global_args.iter().cloned());
        expanded_args.extend(alias_tokens);
        expanded_args.extend(current.command_args.iter().cloned());

        current = parse_git_cli_args(&expanded_args);
    }
}

#[cfg(feature = "test-support")]
fn parse_alias_tokens(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim_start();

    if trimmed.starts_with('!') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' => escaped = true,
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }

    if in_single || in_double {
        return None;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    Some(tokens)
}

/// In async (wrapper-to-daemon) mode, after a successful `git commit`, poll for
/// the daemon-produced authorship note and display stats inline when available.
/// Mirrors the same skip/display rules as plain wrapper mode in post_commit.rs.
fn maybe_show_async_post_commit_stats(parsed: &ParsedGitInvocation, repo: &Repository) {
    use crate::authorship::ignore::effective_ignore_patterns;
    use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
    use crate::git::cli_parser::is_dry_run;
    use crate::git::refs::show_authorship_note;
    use std::io::IsTerminal;

    // Respect the same suppression flags as the synchronous wrapper path.
    if is_dry_run(&parsed.command_args) {
        return;
    }
    let suppress_output = parsed.has_command_flag("--porcelain")
        || parsed.has_command_flag("--quiet")
        || parsed.has_command_flag("-q")
        || parsed.has_command_flag("--no-status");
    if suppress_output || config::Config::get().is_quiet() {
        return;
    }

    let is_interactive =
        std::io::stdout().is_terminal() || std::env::var_os("GIT_AI_TEST_FORCE_TTY").is_some();
    if !is_interactive {
        return;
    }

    // Determine the new commit SHA.
    let commit_sha = match repo.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return,
    };

    // Use a longer timeout under test to avoid flakiness on saturated CI machines.
    // GIT_AI_POST_COMMIT_TIMEOUT_MS allows tests to override the timeout.
    let timeout = if let Some(ms) = std::env::var("GIT_AI_POST_COMMIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::time::Duration::from_millis(ms)
    } else if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some() {
        std::time::Duration::from_secs(20)
    } else {
        std::time::Duration::from_millis(500)
    };

    // Poll for the authorship note the daemon should be producing.
    let poll_interval = std::time::Duration::from_millis(25);
    let start = std::time::Instant::now();
    let note_found = loop {
        if show_authorship_note(repo, &commit_sha).is_some() {
            break true;
        }
        if start.elapsed() >= timeout {
            break false;
        }
        std::thread::sleep(poll_interval);
    };

    if !note_found {
        eprintln!(
            "[git-ai] still processing commit {}... run `git ai stats` to see stats.",
            &commit_sha[..std::cmp::min(8, commit_sha.len())]
        );
        return;
    }

    // Check if this is a merge commit — skip expensive stats just like the sync path.
    let is_merge = repo
        .find_commit(commit_sha.clone())
        .map(|c| c.parent_count().unwrap_or(0) > 1)
        .unwrap_or(false);
    if is_merge {
        eprintln!(
            "[git-ai] Skipped git-ai stats for merge commit {}.",
            commit_sha
        );
        return;
    }

    // Run the same cost estimation the sync path uses.
    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    if let Ok(estimate) = crate::authorship::post_commit::estimate_stats_cost_for_head(
        repo,
        &commit_sha,
        &ignore_patterns,
    ) && estimate.should_skip()
    {
        eprintln!(
            "[git-ai] Skipped git-ai stats for large commit. Run `git ai stats {}` to compute stats on demand.",
            commit_sha
        );
        return;
    }

    // Compute and display the full stats.
    if let Ok(stats) = stats_for_commit_stats(repo, &commit_sha, &ignore_patterns) {
        write_stats_to_terminal(&stats, true);
    }
}

fn head_state_to_repo_context(
    s: crate::git::repo_state::HeadState,
) -> crate::daemon::domain::RepoContext {
    crate::daemon::domain::RepoContext {
        head: s.head,
        branch: s.branch,
        detached: s.detached,
    }
}

fn send_wrapper_pre_state_to_daemon(
    invocation_id: &str,
    worktree: Option<&std::path::Path>,
    pre_state: &Option<crate::git::repo_state::HeadState>,
) {
    let Some(wt) = worktree else { return };
    let Some(pre) = pre_state.clone() else { return };
    let wt_str = wt.to_string_lossy().to_string();
    if let Err(e) = crate::daemon::telemetry_handle::send_wrapper_pre_state(
        invocation_id,
        &wt_str,
        head_state_to_repo_context(pre),
    ) {
        tracing::debug!(
            "wrapper: failed to send pre-state for {}: {}",
            invocation_id,
            e
        );
    }
}

fn send_wrapper_post_state_to_daemon(
    invocation_id: &str,
    worktree: Option<&std::path::Path>,
    post_state: &Option<crate::git::repo_state::HeadState>,
) {
    let Some(wt) = worktree else { return };
    let Some(post) = post_state.clone() else {
        return;
    };
    let wt_str = wt.to_string_lossy().to_string();
    if let Err(e) = crate::daemon::telemetry_handle::send_wrapper_post_state(
        invocation_id,
        &wt_str,
        head_state_to_repo_context(post),
    ) {
        tracing::debug!(
            "wrapper: failed to send post-state for {}: {}",
            invocation_id,
            e
        );
    }
}

fn proxy_to_git(
    args: &[String],
    exit_on_completion: bool,
    wrapper_invocation_id: Option<&str>,
) -> std::process::ExitStatus {
    // Suppress trace2 for read-only invocations to avoid hitting the daemon
    // with events that can never produce meaningful state changes.  In async
    // mode, read-only invocations are handled before this point (no
    // invocation_id set), so wrapper_invocation_id is only Some for mutating
    // commands that need trace2 events for the daemon to match wrapper state.
    //
    // Use is_definitely_read_only_invocation so that subcommand-gated
    // read-only calls like `git stash list` and `git worktree list` are also
    // suppressed (matches the updated wrapper check in handle_git above).
    let suppress_trace2 = wrapper_invocation_id.is_none() && {
        let parsed = parse_git_cli_args(args);
        let subcommand = parsed.command_args.first().map(String::as_str);
        parsed.command.as_deref().is_some_and(|cmd| {
            crate::git::command_classification::is_definitely_read_only_invocation(cmd, subcommand)
        })
    };

    // Use spawn for interactive commands
    let child = {
        #[cfg(unix)]
        {
            // Only create a new process group for non-interactive runs.
            // If stdin is a TTY, the child must remain in the foreground
            // terminal process group to avoid SIGTTIN/SIGTTOU hangs.
            let is_interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
            let should_setpgid = !is_interactive;

            let mut cmd = Command::new(config::Config::get().git_cmd());
            cmd.args(args);
            cmd.env(ENV_SKIP_MANAGED_HOOKS, "1");
            if suppress_trace2 {
                cmd.env("GIT_TRACE2_EVENT", "0");
            }
            if let Some(id) = wrapper_invocation_id {
                cmd.env("GIT_AI_WRAPPER_INVOCATION_ID", id);
                cmd.env("GIT_TRACE2_ENV_VARS", "GIT_AI_WRAPPER_INVOCATION_ID");
            }
            unsafe {
                let setpgid_flag = should_setpgid;
                cmd.pre_exec(move || {
                    if setpgid_flag {
                        // Make the child its own process group leader so we can signal the group
                        let _ = libc::setpgid(0, 0);
                    }
                    Ok(())
                });
            }
            // We return both the spawned child and whether we changed PGID
            match cmd.spawn() {
                Ok(child) => Ok((child, should_setpgid)),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(unix))]
        {
            let mut cmd = Command::new(config::Config::get().git_cmd());
            cmd.args(args);
            cmd.env(ENV_SKIP_MANAGED_HOOKS, "1");
            if suppress_trace2 {
                cmd.env("GIT_TRACE2_EVENT", "0");
            }
            if let Some(id) = wrapper_invocation_id {
                cmd.env("GIT_AI_WRAPPER_INVOCATION_ID", id);
                cmd.env("GIT_TRACE2_ENV_VARS", "GIT_AI_WRAPPER_INVOCATION_ID");
            }

            #[cfg(windows)]
            {
                if !is_interactive_terminal() {
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
            }

            cmd.spawn()
        }
    };

    #[cfg(unix)]
    match child {
        Ok((mut child, setpgid)) => {
            #[cfg(unix)]
            {
                if setpgid {
                    // Record the child's process group id (same as its pid after setpgid)
                    let pgid: i32 = child.id() as i32;
                    CHILD_PGID.store(pgid, Ordering::Relaxed);
                    install_forwarding_handlers();
                }
            }
            let status = child.wait();
            match status {
                Ok(status) => {
                    #[cfg(unix)]
                    {
                        if setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                    }
                    if exit_on_completion {
                        exit_with_status(status);
                    }
                    status
                }
                Err(e) => {
                    #[cfg(unix)]
                    {
                        if setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                    }
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }

    #[cfg(not(unix))]
    match child {
        Ok(mut child) => {
            let status = child.wait();
            match status {
                Ok(status) => {
                    if exit_on_completion {
                        exit_with_status(status);
                    }
                    status
                }
                Err(e) => {
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to execute git command: {}", e);
            std::process::exit(1);
        }
    }
}

// Exit mirroring the child's termination: same signal if signaled, else exit code
fn exit_with_status(status: std::process::ExitStatus) -> ! {
    #[cfg(unix)]
    {
        if let Some(sig) = status.signal() {
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            // Should not return
            unreachable!();
        }
    }
    std::process::exit(status.code().unwrap_or(1));
}

// Detect if current process invocation is coming from shell completion machinery
// (bash, zsh via bashcompinit). If so, we should proxy directly to the real git
// without any extra behavior that could interfere with completion scripts.
fn in_shell_completion_context() -> bool {
    std::env::var("COMP_LINE").is_ok()
        || std::env::var("COMP_POINT").is_ok()
        || std::env::var("COMP_TYPE").is_ok()
}
