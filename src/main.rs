mod commands;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("checkpoint") => commands::checkpoint::handle_checkpoint(&args[1..]),
        Some("post-commit") => commands::post_commit::handle_post_commit(),
        Some("post-rewrite") => commands::post_rewrite::handle_post_rewrite(&args[1..]),
        Some("post-rewrite-squash") => commands::post_rewrite::handle_post_rewrite_squash(&args[1..]),
        Some("stash-save") => commands::stash::handle_stash_save(),
        Some("stash-restore") => commands::stash::handle_stash_restore(),
        Some("stash-restore-ref") => commands::stash::handle_stash_restore_ref(&args[1..]),
        Some("blame") => commands::blame::handle_blame(&args[1..]),
        Some("diff") => commands::diff::handle_diff(&args[1..]),
        Some("fetch-notes") | Some("fetch") => commands::fetch_notes::handle_fetch_notes(&args[1..]),
        Some("push-notes") | Some("push") => commands::push_notes::handle_push_notes(&args[1..]),
        Some("search") => commands::search::handle_search(&args[1..]),
        Some("install") => commands::install::handle_install(),
        Some("status") => commands::status::handle_status(&args[1..]),
        Some("stats") => commands::stats::handle_stats(&args[1..]),
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
        Some("doctor") => commands::doctor::handle_doctor(&args[1..]),
        Some("effective-ignore-patterns") => commands::internal::handle_internal_command("effective-ignore-patterns", &args[1..]),
        Some("blame-analysis") => commands::internal::handle_internal_command("blame-analysis", &args[1..]),
        Some("fetch-authorship-notes") => commands::internal::handle_internal_command("fetch-authorship-notes", &args[1..]),
        Some("fetch_authorship_notes") => commands::internal::handle_internal_command("fetch_authorship_notes", &args[1..]),
        Some("push-authorship-notes") => commands::internal::handle_internal_command("push-authorship-notes", &args[1..]),
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
            println!("  doctor        Health check — verify installation and hooks");
        }
        Some(cmd) => {
            eprintln!("git-ai: unknown command '{}'", cmd);
            process::exit(1);
        }
    }
}
