use crate::git::cli_parser::{ParsedGitInvocation, extract_clone_target_directory};
use crate::git::repository::{find_repository_in_path, exec_git};
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

    // Configure automatic fetching of AI authorship notes for future fetches
    // Add a fetch refspec so that git fetch and git pull automatically fetch notes
    configure_notes_fetch(&repository);
}

fn configure_notes_fetch(repository: &crate::git::repository::Repository) {
    debug_log("configuring automatic fetch of authorship notes for origin");
    
    // Add fetch refspec: +refs/notes/ai:refs/notes/ai
    // This ensures git fetch and git pull automatically fetch authorship notes
    let mut args = repository.global_args_for_exec();
    args.push("config".to_string());
    args.push("--add".to_string());
    args.push("remote.origin.fetch".to_string());
    args.push("+refs/notes/ai:refs/notes/ai".to_string());

    match exec_git(&args) {
        Ok(_) => {
            debug_log("successfully configured automatic notes fetch");
        }
        Err(e) => {
            debug_log(&format!("failed to configure automatic notes fetch: {}", e));
        }
    }
}
