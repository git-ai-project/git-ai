mod api;
mod auth;
mod authorship;
mod ci;
mod commands;
mod config;
mod error;
mod feature_flags;
mod git;
mod mdm;
mod metrics;
mod observability;
mod repo_url;
mod utils;

use clap::Parser;

#[derive(Parser)]
#[command(name = "git-ai")]
#[command(about = "git proxy with AI authorship tracking", long_about = None)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Cli {
    /// Git command and arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn main() {
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();

    // Get the binary name that was called
    let binary_name = raw_args
        .first()
        .and_then(|arg| arg.to_str().map(|s| s.to_string()))
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or("git-ai".to_string());

    // Fast path for managed hook launchers. This avoids clap parsing and command
    // router setup on the hook hot path.
    if raw_args.get(1).and_then(|arg| arg.to_str()) == Some("hook-trampoline") {
        let hook_args = raw_args
            .iter()
            .skip(2)
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        commands::core_hook_trampoline::handle_hook_trampoline_command(&hook_args);
        return;
    }

    let cli = Cli::parse();

    #[cfg(debug_assertions)]
    {
        if std::env::var("GIT_AI").as_deref() == Ok("git") {
            commands::git_handlers::handle_git(&cli.args);
            return;
        }
    }

    if binary_name == "git-ai" || binary_name == "git-ai.exe" {
        commands::git_ai_handlers::handle_git_ai(&cli.args);
        std::process::exit(0);
    }

    commands::git_handlers::handle_git(&cli.args);
}
