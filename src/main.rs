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
    // Get the binary name that was called
    let binary_name = std::env::args_os()
        .next()
        .and_then(|arg| arg.into_string().ok())
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or("git-ai".to_string());

    if commands::git_hook_handlers::is_git_hook_binary_name(&binary_name) {
        let hook_args: Vec<String> = std::env::args().skip(1).collect();
        let exit_code =
            commands::git_hook_handlers::handle_git_hook_invocation(&binary_name, &hook_args);
        std::process::exit(exit_code);
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
