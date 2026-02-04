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

    print!("Fetching git-ai authorship notes");
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

    // Fetch authorship notes from origin
    if let Err(e) = fetch_authorship_notes(&repository, "origin") {
        debug_log(&format!("authorship fetch from origin failed: {}", e));
        println!(", failed.");
    } else {
        debug_log("successfully fetched authorship notes from origin");
        println!(", done.");
    }
}
