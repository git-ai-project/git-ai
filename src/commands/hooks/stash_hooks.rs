use crate::git::cli_parser::ParsedGitInvocation;

/// Extract pathspecs from stash push/save command
/// Format: git stash push [options] [--] [<pathspec>...]
pub(crate) fn extract_stash_pathspecs(parsed_args: &ParsedGitInvocation) -> Vec<String> {
    let mut pathspecs = Vec::new();
    let mut found_separator = false;
    let mut skip_next = false;

    for (i, arg) in parsed_args.command_args.iter().enumerate() {
        // Skip if this was consumed by a previous flag
        if skip_next {
            skip_next = false;
            continue;
        }

        // Found separator, everything after is pathspec
        if arg == "--" {
            found_separator = true;
            continue;
        }

        // After separator, everything is a pathspec
        if found_separator {
            pathspecs.push(arg.clone());
            continue;
        }

        // Skip flags and their values
        if arg.starts_with('-') {
            // Check if this flag consumes the next argument
            if stash_option_consumes_value(arg) {
                skip_next = true;
            }
            continue;
        }

        // Skip the subcommand (push/save/pop/apply)
        if i == 0 && (arg == "push" || arg == "save" || arg == "pop" || arg == "apply") {
            continue;
        }

        // Skip stash reference for pop/apply (e.g., stash@{0})
        if i == 1 && arg.starts_with("stash@") {
            continue;
        }

        // Everything else is a pathspec
        pathspecs.push(arg.clone());
    }

    tracing::debug!("Extracted pathspecs: {:?}", pathspecs);
    pathspecs
}

/// Check if a stash option consumes the next value
fn stash_option_consumes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-m" | "--message" | "--pathspec-from-file" | "--pathspec-file-nul"
    )
}
