use crate::config;
use crate::git::cli_parser::{ParsedGitInvocation, extract_clone_target_directory};
use crate::git::repository::find_repository_in_path;
use crate::git::sync_authorship::fetch_authorship_notes;
use crate::utils::debug_log;

pub fn post_clone_hook(parsed_args: &ParsedGitInvocation, exit_status: std::process::ExitStatus) {
    // Only run if clone succeeded
    if !exit_status.success() {
        return;
    }

    // Extract the target directory from clone arguments
    let target_dir = match extract_clone_target_directory(&parsed_args.command_args) {
        Some(dir) => dir,
        None => {
            debug_log(
                "failed to extract target directory from clone command; skipping authorship fetch",
            );
            return;
        }
    };

    debug_log(&format!(
        "post-clone: attempting to fetch authorship notes for cloned repository at: {}",
        target_dir
    ));

    // Open the newly cloned repository
    let repository = match find_repository_in_path(&target_dir) {
        Ok(repo) => repo,
        Err(e) => {
            debug_log(&format!(
                "failed to open cloned repository at {}: {}; skipping authorship fetch",
                target_dir, e
            ));
            return;
        }
    };

    // Check if the newly cloned repository is allowed by allow_repositories config
    let config = config::Config::get();
    if !config.is_allowed_repository(&Some(repository.clone())) {
        debug_log(
            "Skipping authorship fetch for cloned repository: not in allow_repositories list",
        );
        return;
    }

    // Determine if output should be suppressed: respect quiet config and --quiet/-q git flags
    let suppress_output = config.is_quiet()
        || parsed_args.has_command_flag("--quiet")
        || parsed_args.has_command_flag("-q");

    if !suppress_output {
        eprint!("Fetching git-ai authorship notes");
    }

    // Fetch authorship notes from origin
    if let Err(e) = fetch_authorship_notes(&repository, "origin") {
        debug_log(&format!("authorship fetch from origin failed: {}", e));
        if !suppress_output {
            eprintln!(", failed.");
        }
    } else {
        debug_log("successfully fetched authorship notes from origin");
        if !suppress_output {
            eprintln!(", done.");
        }
    }
}
