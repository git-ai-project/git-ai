mod commands;

use std::env;
use std::process;

const GIT_HOOK_NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
];

fn main() {
    let binary_name = env::args_os()
        .next()
        .and_then(|arg| arg.into_string().ok())
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "git-ai".to_string());

    // Legacy hook symlink detection: if invoked as a git hook name, show deprecation.
    if GIT_HOOK_NAMES.contains(&binary_name.as_str()) {
        eprintln!(
            "git-ai: the git core hooks feature has been sunset.\n\
             To remove the deprecated git-ai hook symlinks from this repository, run:\n\
             \n\
             \x20 git-ai git-hooks remove\n"
        );
        process::exit(0);
    }

    // Git proxy passthrough: if invoked as "git", exec the real git binary.
    if binary_name == "git" || binary_name == "git.exe" {
        proxy_to_real_git();
    }

    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("checkpoint") => commands::checkpoint::handle_checkpoint(&args[1..]),
        Some("post-commit") => commands::post_commit::handle_post_commit(),
        Some("post-rewrite") => commands::post_rewrite::handle_post_rewrite(&args[1..]),
        Some("post-rewrite-squash") => {
            commands::post_rewrite::handle_post_rewrite_squash(&args[1..])
        }
        Some("stash-save") => commands::stash::handle_stash_save(),
        Some("stash-restore") => commands::stash::handle_stash_restore(),
        Some("stash-restore-ref") => commands::stash::handle_stash_restore_ref(&args[1..]),
        Some("blame") => commands::blame::handle_blame(&args[1..]),
        Some("diff") => commands::diff::handle_diff(&args[1..]),
        Some("fetch-notes") | Some("fetch") => {
            commands::fetch_notes::handle_fetch_notes(&args[1..])
        }
        Some("push-notes") | Some("push") => commands::push_notes::handle_push_notes(&args[1..]),
        Some("search") => commands::search::handle_search(&args[1..]),
        Some("install") | Some("install-hooks") => commands::install::handle_install(),
        Some("git-hooks") => commands::git_hooks::handle_git_hooks(&args[1..]),
        Some("status") => commands::status::handle_status(&args[1..]),
        Some("stats") => commands::stats::handle_stats(&args[1..]),
        Some("log") => {
            let status = commands::log::handle_log(&args[1..]);
            if !status.success() {
                process::exit(status.code().unwrap_or(1));
            }
        }
        Some("show") => commands::show::handle_show(&args[1..]),
        Some("show-prompt") => commands::show_prompt::handle_show_prompt(&args[1..]),
        Some("login") => commands::login::handle_login(&args[1..]),
        Some("logout") => commands::logout::handle_logout(&args[1..]),
        Some("whoami") => commands::whoami::handle_whoami(&args[1..]),
        Some("dashboard") => commands::dashboard::handle_dashboard(&args[1..]),
        Some("exchange-nonce") => commands::exchange_nonce::handle_exchange_nonce(&args[1..]),
        Some("config") => commands::config::handle_config(&args[1..]),
        Some("flush-metrics-db") => commands::flush_metrics_db::handle_flush_metrics_db(&args[1..]),
        Some("bg") => commands::bg::handle_bg(&args[1..]),
        Some("ci") => commands::ci::handle_ci(&args[1..]),
        Some("gc") => {
            if let Err(e) = commands::gc::handle_gc(&args[1..]) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Some("migrate") => {
            if let Err(e) = commands::migrate::handle_migrate(&args[1..]) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Some("perf") => {
            if let Err(e) = commands::perf::handle_perf(&args[1..]) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Some("debug") => commands::debug::handle_debug(&args[1..]),
        Some("upgrade") => commands::upgrade::handle_upgrade(&args[1..]),
        Some("doctor") => commands::doctor::handle_doctor(&args[1..]),
        Some("effective-ignore-patterns") => {
            commands::internal::handle_internal_command("effective-ignore-patterns", &args[1..])
        }
        Some("blame-analysis") => {
            commands::internal::handle_internal_command("blame-analysis", &args[1..])
        }
        Some("fetch-authorship-notes") => {
            commands::internal::handle_internal_command("fetch-authorship-notes", &args[1..])
        }
        Some("fetch_authorship_notes") => {
            commands::internal::handle_internal_command("fetch_authorship_notes", &args[1..])
        }
        Some("push-authorship-notes") => {
            commands::internal::handle_internal_command("push-authorship-notes", &args[1..])
        }
        Some("--version") | Some("-v") | Some("version") => {
            println!("git-ai {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("usage: git-ai <command> [<args>]");
            println!();
            println!("Commands:");
            println!("  checkpoint    Record attribution checkpoint");
            println!("  post-commit   Generate authorship note for HEAD commit");
            println!("  post-rewrite  Copy authorship notes after rebase/amend");
            println!("  blame         Show blame with AI/human attribution");
            println!("  diff          Show diff with AI attribution");
            println!("  log           Show git log with authorship notes");
            println!("  show          Show authorship notes for a commit or range");
            println!("  show-prompt   Show a prompt by ID from authorship notes");
            println!("  search        Grep with attribution context");
            println!("  fetch-notes   Fetch authorship notes from remote");
            println!("  push-notes    Push authorship notes to remote");
            println!("  install       Install git hooks for automatic attribution");
            println!("  status        Show uncommitted attribution status");
            println!("  stats         Show attribution statistics");
            println!("  bg            Daemon lifecycle (run, start, stop, status)");
            println!("  gc            Remove orphaned authorship notes");
            println!("  migrate       Upgrade authorship note schemas in-place");
            println!("  perf          Performance baseline and regression detection");
            println!("  config        View and manage configuration");
            println!("  flush-metrics-db  Flush offline telemetry queue");
            println!("  debug         Print diagnostic information");
            println!("  login         Log in via device authorization flow");
            println!("  logout        Log out and clear stored credentials");
            println!("  whoami        Show current auth state and identity");
            println!("  dashboard     Open personal dashboard in browser");
            println!("  upgrade       Update git-ai to the latest version");
            println!("  doctor        Health check — verify installation and hooks");
        }
        Some(cmd) => {
            eprintln!("git-ai: unknown command '{}'", cmd);
            process::exit(1);
        }
    }
}

/// Proxy all arguments to the real git binary. Used when this binary is
/// symlinked as "git" to avoid breaking legacy installations.
fn proxy_to_real_git() -> ! {
    let git = git_ai::core::git_binary::git_path();
    let args: Vec<String> = env::args().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = process::Command::new(git).args(&args).exec();
        eprintln!("git-ai: failed to exec git: {}", err);
        process::exit(127);
    }

    #[cfg(not(unix))]
    {
        let status = process::Command::new(git)
            .args(&args)
            .status()
            .unwrap_or_else(|e| {
                eprintln!("git-ai: failed to run git: {}", e);
                process::exit(127);
            });
        process::exit(status.code().unwrap_or(1));
    }
}
