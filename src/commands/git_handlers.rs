use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::commands::hooks::checkout_hooks;
use crate::commands::hooks::cherry_pick_hooks;
use crate::commands::hooks::clone_hooks;
use crate::commands::hooks::commit_hooks;
use crate::commands::hooks::fetch_hooks;
use crate::commands::hooks::merge_hooks;
use crate::commands::hooks::push_hooks;
use crate::commands::hooks::rebase_hooks;
use crate::commands::hooks::reset_hooks;
use crate::commands::hooks::stash_hooks;
use crate::commands::hooks::switch_hooks;
use crate::config;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::find_repository;
use crate::git::repository::Repository;
use crate::observability;

use crate::observability::wrapper_performance_targets::log_performance_target_if_violated;
use crate::utils::{CREATE_NO_WINDOW, debug_log, is_interactive_terminal};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

/// Error type for hook panics
#[derive(Debug)]
struct HookPanicError(String);

impl std::fmt::Display for HookPanicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for HookPanicError {}

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
        let handler = forward_signal_handler as usize;
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

pub struct CommandHooksContext {
    pub pre_commit_hook_result: Option<bool>,
    pub rebase_original_head: Option<String>,
    pub _rebase_onto: Option<String>,
    pub fetch_authorship_handle: Option<std::thread::JoinHandle<()>>,
    pub stash_sha: Option<String>,
    pub push_authorship_handle: Option<std::thread::JoinHandle<()>>,
    /// VirtualAttributions captured before a pull --rebase --autostash operation.
    /// Used to preserve uncommitted AI attributions that git's internal stash would lose.
    pub stashed_va: Option<VirtualAttributions>,
}

pub fn handle_git(args: &[String]) {
    // If we're being invoked from a shell completion context, bypass git-ai logic
    // and delegate directly to the real git so existing completion scripts work.
    if in_shell_completion_context() {
        let orig_args: Vec<String> = std::env::args().skip(1).collect();
        proxy_to_git(&orig_args, true);
        return;
    }

    let mut parsed_args = parse_git_cli_args(args);

    let mut repository_option = find_repository(&parsed_args.global_args).ok();

    let has_repo = repository_option.is_some();

    if let Some(repo) = repository_option.as_ref() {
        observability::set_repo_context(repo);
    }

    let config = config::Config::get();

    let skip_hooks = !config.is_allowed_repository(&repository_option);

    if skip_hooks {
        debug_log(
            "Skipping git-ai hooks because repository is excluded or not in allow_repositories list",
        );
    }

    // Handle clone separately since repo doesn't exist before the command
    if parsed_args.command.as_deref() == Some("clone") && !parsed_args.is_help && !skip_hooks {
        let exit_status = proxy_to_git(&parsed_args.to_invocation_vec(), false);
        clone_hooks::post_clone_hook(&parsed_args, exit_status);
        exit_with_status(exit_status);
    }

    // run with hooks
    let exit_status = if !parsed_args.is_help && has_repo && !skip_hooks {
        let mut command_hooks_context = CommandHooksContext {
            pre_commit_hook_result: None,
            rebase_original_head: None,
            _rebase_onto: None,
            fetch_authorship_handle: None,
            stash_sha: None,
            push_authorship_handle: None,
            stashed_va: None,
        };

        let repository = repository_option.as_mut().unwrap();

        let pre_command_start = Instant::now();
        run_pre_command_hooks(&mut command_hooks_context, &mut parsed_args, repository);
        let pre_command_duration = pre_command_start.elapsed();

        let git_start = Instant::now();
        let exit_status = proxy_to_git(&parsed_args.to_invocation_vec(), false);
        let git_duration = git_start.elapsed();

        let post_command_start = Instant::now();
        run_post_command_hooks(
            &mut command_hooks_context,
            &parsed_args,
            exit_status,
            repository,
        );
        let post_command_duration = post_command_start.elapsed();

        log_performance_target_if_violated(
            &parsed_args.command.as_deref().unwrap_or("unknown"),
            pre_command_duration,
            git_duration,
            post_command_duration,
        );

        exit_status
    } else {
        // run without hooks
        proxy_to_git(&parsed_args.to_invocation_vec(), false)
    };
    exit_with_status(exit_status);
}

fn run_pre_command_hooks(
    command_hooks_context: &mut CommandHooksContext,
    parsed_args: &mut ParsedGitInvocation,
    repository: &mut Repository,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Pre-command hooks
        match parsed_args.command.as_deref() {
            Some("commit") => {
                command_hooks_context.pre_commit_hook_result = Some(
                    commit_hooks::commit_pre_command_hook(parsed_args, repository),
                );
            }
            Some("rebase") => {
                rebase_hooks::pre_rebase_hook(parsed_args, repository, command_hooks_context);
            }
            Some("reset") => {
                reset_hooks::pre_reset_hook(parsed_args, repository);
            }
            Some("cherry-pick") => {
                cherry_pick_hooks::pre_cherry_pick_hook(
                    parsed_args,
                    repository,
                    command_hooks_context,
                );
            }
            Some("push") => {
                command_hooks_context.push_authorship_handle =
                    push_hooks::push_pre_command_hook(parsed_args, repository);
            }
            Some("fetch") => {
                command_hooks_context.fetch_authorship_handle =
                    fetch_hooks::fetch_pull_pre_command_hook(parsed_args, repository);
            }
            Some("pull") => {
                fetch_hooks::pull_pre_command_hook(parsed_args, repository, command_hooks_context);
            }
            Some("stash") => {
                let config = config::Config::get();

                if config.feature_flags().rewrite_stash {
                    stash_hooks::pre_stash_hook(parsed_args, repository, command_hooks_context);
                }
            }
            Some("checkout") => {
                checkout_hooks::pre_checkout_hook(parsed_args, repository, command_hooks_context);
            }
            Some("switch") => {
                switch_hooks::pre_switch_hook(parsed_args, repository, command_hooks_context);
            }
            _ => {}
        }
    }));

    if let Err(panic_payload) = result {
        let error_message = if let Some(message) = panic_payload.downcast_ref::<&str>() {
            format!("Panic in run_pre_command_hooks: {}", message)
        } else if let Some(message) = panic_payload.downcast_ref::<String>() {
            format!("Panic in run_pre_command_hooks: {}", message)
        } else {
            "Panic in run_pre_command_hooks: unknown panic".to_string()
        };

        let command_name = parsed_args.command.as_deref().unwrap_or("unknown");
        let context = serde_json::json!({
            "function": "run_pre_command_hooks",
            "command": command_name,
            "args": parsed_args.to_invocation_vec(),
        });

        debug_log(&error_message);
        observability::log_error(&HookPanicError(error_message.clone()), Some(context));
    }
}

fn run_post_command_hooks(
    command_hooks_context: &mut CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Post-command hooks
        match parsed_args.command.as_deref() {
            Some("commit") => commit_hooks::commit_post_command_hook(
                parsed_args,
                exit_status,
                repository,
                command_hooks_context,
            ),
            Some("fetch") => fetch_hooks::fetch_pull_post_command_hook(
                repository,
                parsed_args,
                exit_status,
                command_hooks_context,
            ),
            Some("pull") => fetch_hooks::pull_post_command_hook(
                repository,
                parsed_args,
                exit_status,
                command_hooks_context,
            ),
            Some("push") => push_hooks::push_post_command_hook(
                repository,
                parsed_args,
                exit_status,
                command_hooks_context,
            ),
            Some("reset") => reset_hooks::post_reset_hook(parsed_args, repository, exit_status),
            Some("merge") => merge_hooks::post_merge_hook(parsed_args, exit_status, repository),
            Some("rebase") => rebase_hooks::handle_rebase_post_command(
                command_hooks_context,
                parsed_args,
                exit_status,
                repository,
            ),
            Some("cherry-pick") => cherry_pick_hooks::post_cherry_pick_hook(
                command_hooks_context,
                parsed_args,
                exit_status,
                repository,
            ),
            Some("stash") => {
                let config = config::Config::get();

                if config.feature_flags().rewrite_stash {
                    stash_hooks::post_stash_hook(
                        &command_hooks_context,
                        parsed_args,
                        repository,
                        exit_status,
                    );
                }
            }
            Some("checkout") => {
                checkout_hooks::post_checkout_hook(
                    parsed_args,
                    repository,
                    exit_status,
                    command_hooks_context,
                );
            }
            Some("switch") => {
                switch_hooks::post_switch_hook(
                    parsed_args,
                    repository,
                    exit_status,
                    command_hooks_context,
                );
            }
            _ => {}
        }
    }));

    if let Err(panic_payload) = result {
        let error_message = if let Some(message) = panic_payload.downcast_ref::<&str>() {
            format!("Panic in run_post_command_hooks: {}", message)
        } else if let Some(message) = panic_payload.downcast_ref::<String>() {
            format!("Panic in run_post_command_hooks: {}", message)
        } else {
            "Panic in run_post_command_hooks: unknown panic".to_string()
        };

        let command_name = parsed_args.command.as_deref().unwrap_or("unknown");
        let exit_code = exit_status.code().unwrap_or(-1);
        let context = serde_json::json!({
            "function": "run_post_command_hooks",
            "command": command_name,
            "exit_code": exit_code,
            "args": parsed_args.to_invocation_vec(),
        });

        debug_log(&error_message);
        observability::log_error(&HookPanicError(error_message.clone()), Some(context));
    }
}

fn proxy_to_git(args: &[String], exit_on_completion: bool) -> std::process::ExitStatus {
    // debug_log(&format!("proxying to git with args: {:?}", args));
    // debug_log(&format!("prepended global args: {:?}", prepend_global(args)));
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
                    return status;
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
                    return status;
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
