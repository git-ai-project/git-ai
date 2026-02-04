use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, exec_git};
use crate::utils::debug_log;

pub fn pre_stash_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    // Check if this is a pop or apply command - we need to capture the stash SHA before Git deletes it
    let subcommand = match parsed_args.pos_command(0) {
        Some(cmd) => cmd,
        None => return, // Implicit push, nothing to capture
    };

    if subcommand == "pop" || subcommand == "apply" {
        // Capture the stash SHA BEFORE git runs (pop will delete it)
        let stash_ref = parsed_args
            .pos_command(1)
            .unwrap_or_else(|| "stash@{0}".to_string());

        if let Ok(stash_sha) = resolve_stash_to_sha(repository, &stash_ref) {
            command_hooks_context.stash_sha = Some(stash_sha);
            debug_log(&format!("Pre-stash: captured stash SHA for {}", subcommand));
        }
    } else {
        let _ = match crate::commands::checkpoint::run(
            repository,
            &get_commit_default_author(repository, &parsed_args.command_args),
            CheckpointKind::Human,
            false,
            false,
            true,
            None,
            true, // same optimizations as pre_commit.rs
        ) {
            Ok(result) => result,
            Err(e) => {
                debug_log(&format!("Failed to run checkpoint: {}", e));
                return;
            }
        };
    }
}

pub fn post_stash_hook(
    command_hooks_context: &CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    exit_status: std::process::ExitStatus,
) {
    if !exit_status.success() {
        debug_log("Stash failed, skipping post-stash hook");
        return;
    }

    // Check what subcommand was used
    let subcommand = match parsed_args.pos_command(0) {
        Some(cmd) => cmd,
        None => {
            // No subcommand means implicit "push"
            "push".to_string()
        }
    };

    debug_log(&format!("Post-stash: processing stash {}", subcommand));

    // Handle different subcommands
    if subcommand == "push" || subcommand == "save" {
        // Extract pathspecs from command
        let pathspecs = extract_stash_pathspecs(parsed_args);

        // Stash was created - save authorship log as git note
        if let Err(e) = save_stash_authorship_log(repository, &pathspecs) {
            debug_log(&format!("Failed to save stash authorship log: {}", e));
        }
    } else if subcommand == "pop" || subcommand == "apply" {
        // Stash was applied - restore attributions from git note
        // Use the stash SHA we captured in pre-hook (before Git deleted it)
        let stash_sha = match &command_hooks_context.stash_sha {
            Some(sha) => sha.clone(),
            None => {
                debug_log("No stash SHA captured in pre-hook, cannot restore attributions");
                return;
            }
        };

        debug_log(&format!(
            "Restoring attributions from stash SHA: {}",
            stash_sha
        ));

        let human_author = get_commit_default_author(repository, &parsed_args.command_args);

        if let Err(e) = restore_stash_attributions(repository, &stash_sha, &human_author) {
            debug_log(&format!("Failed to restore stash attributions: {}", e));
        }
    }
}

/// Save the current working log as an authorship log in git notes (refs/notes/ai-stash)
fn save_stash_authorship_log(repo: &Repository, pathspecs: &[String]) -> Result<(), GitAiError> {
    let head_sha = repo.head()?.target()?.to_string();

    // Get the stash SHA that was just created (stash@{0})
    let stash_sha = resolve_stash_to_sha(repo, "stash@{0}")?;
    debug_log(&format!("Stash created with SHA: {}", stash_sha));

    // Build VirtualAttributions from the working log before it was cleared
    let working_log_va =
        VirtualAttributions::from_just_working_log(repo.clone(), head_sha.clone(), None)?;

    // Filter attributions to only include files that match the pathspecs
    let filtered_files: Vec<String> = if pathspecs.is_empty() {
        // No pathspecs means all files
        working_log_va
            .files()
            .into_iter()
            .map(|f| f.to_string())
            .collect()
    } else {
        working_log_va
            .files()
            .into_iter()
            .filter(|file| file_matches_pathspecs(file, pathspecs, repo))
            .map(|f| f.to_string())
            .collect()
    };

    // If there are no attributions, just clean up working log for filtered files
    if filtered_files.is_empty() {
        debug_log("No attributions to save for stash");
        delete_working_log_for_files(repo, &head_sha, &filtered_files)?;
        return Ok(());
    }

    debug_log(&format!(
        "Saving attributions for {} files (pathspecs: {:?})",
        filtered_files.len(),
        pathspecs
    ));

    // Convert to authorship log, filtering to only include matched files
    let mut authorship_log = working_log_va.to_authorship_log()?;
    authorship_log
        .attestations
        .retain(|a| filtered_files.contains(&a.file_path));

    // Save as git note at refs/notes/ai-stash
    let json = authorship_log
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    save_stash_note(repo, &stash_sha, &json)?;

    debug_log(&format!(
        "Saved authorship log to refs/notes/ai-stash for stash {}",
        stash_sha
    ));

    // Delete the working log entries for files that were stashed
    delete_working_log_for_files(repo, &head_sha, &filtered_files)?;
    debug_log(&format!(
        "Deleted working log entries for {} files",
        filtered_files.len()
    ));

    Ok(())
}

/// Restore attributions from a stash by reading the git note and converting to INITIAL attributions
fn restore_stash_attributions(
    repo: &Repository,
    stash_sha: &str,
    _human_author: &str,
) -> Result<(), GitAiError> {
    debug_log(&format!(
        "Restoring stash attributions from SHA: {}",
        stash_sha
    ));

    let head_sha = repo.head()?.target()?.to_string();

    // Try to read authorship log from git note (refs/notes/ai-stash)
    let note_content = match read_stash_note(repo, stash_sha) {
        Ok(content) => content,
        Err(_) => {
            debug_log("No authorship log found in refs/notes/ai-stash for this stash");
            return Ok(());
        }
    };

    // Parse the authorship log
    let authorship_log = match crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(&note_content) {
        Ok(log) => log,
        Err(e) => {
            debug_log(&format!("Failed to parse stash authorship log: {}", e));
            return Ok(());
        }
    };

    debug_log(&format!(
        "Loaded authorship log from stash: {} files, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    ));

    // Convert authorship log to INITIAL attributions
    let mut initial_files = std::collections::HashMap::new();
    for attestation in &authorship_log.attestations {
        let mut line_attrs = Vec::new();
        for entry in &attestation.entries {
            for range in &entry.line_ranges {
                let (start, end) = match range {
                    crate::authorship::authorship_log::LineRange::Single(line) => (*line, *line),
                    crate::authorship::authorship_log::LineRange::Range(start, end) => {
                        (*start, *end)
                    }
                };
                line_attrs.push(crate::authorship::attribution_tracker::LineAttribution {
                    start_line: start,
                    end_line: end,
                    author_id: entry.hash.clone(),
                    overrode: None,
                });
            }
        }
        if !line_attrs.is_empty() {
            initial_files.insert(attestation.file_path.clone(), line_attrs);
        }
    }

    let initial_prompts: std::collections::HashMap<_, _> = authorship_log
        .metadata
        .prompts
        .clone()
        .into_iter()
        .collect();

    // Write INITIAL attributions to working log
    if !initial_files.is_empty() || !initial_prompts.is_empty() {
        let working_log = repo.storage.working_log_for_base_commit(&head_sha);
        working_log.write_initial_attributions(initial_files.clone(), initial_prompts.clone())?;

        debug_log(&format!(
            "✓ Wrote INITIAL attributions to working log for {}",
            head_sha
        ));
    }

    Ok(())
}

/// Save a note to refs/notes/ai-stash
fn save_stash_note(repo: &Repository, stash_sha: &str, content: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai-stash".to_string());
    args.push("add".to_string());
    args.push("-f".to_string()); // Force overwrite if exists
    args.push("-m".to_string());
    args.push(content.to_string());
    args.push(stash_sha.to_string());

    let output = exec_git(&args)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "Failed to save stash note: git notes exited with status {}",
            output.status
        )));
    }

    Ok(())
}

/// Read a note from refs/notes/ai-stash
fn read_stash_note(repo: &Repository, stash_sha: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai-stash".to_string());
    args.push("show".to_string());
    args.push(stash_sha.to_string());

    let output = exec_git(&args)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "Failed to read stash note: git notes exited with status {}",
            output.status
        )));
    }

    let content = std::str::from_utf8(&output.stdout)?;
    Ok(content.to_string())
}

/// Resolve a stash reference to its commit SHA
fn resolve_stash_to_sha(repo: &Repository, stash_ref: &str) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push(stash_ref.to_string());

    let output = exec_git(&args)?;

    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "Failed to resolve stash reference '{}': git rev-parse exited with status {}",
            stash_ref, output.status
        )));
    }

    let stdout = std::str::from_utf8(&output.stdout)?;
    let sha = stdout.trim().to_string();

    Ok(sha)
}

/// Extract pathspecs from stash push/save command
/// Format: git stash push [options] [--] [<pathspec>...]
fn extract_stash_pathspecs(parsed_args: &ParsedGitInvocation) -> Vec<String> {
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

    debug_log(&format!("Extracted pathspecs: {:?}", pathspecs));
    pathspecs
}

/// Check if a stash option consumes the next value
fn stash_option_consumes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-m" | "--message" | "--pathspec-from-file" | "--pathspec-file-nul"
    )
}

/// Check if a file path matches any of the given pathspecs
fn file_matches_pathspecs(file: &str, pathspecs: &[String], _repo: &Repository) -> bool {
    if pathspecs.is_empty() {
        return true; // No pathspecs means match all
    }

    for pathspec in pathspecs {
        // Handle exact matches
        if file == pathspec {
            return true;
        }

        // Handle directory matches (pathspec/ matches pathspec/file.txt)
        if pathspec.ends_with('/') && file.starts_with(pathspec) {
            return true;
        }

        // Handle directory without trailing slash
        if file.starts_with(&format!("{}/", pathspec)) {
            return true;
        }

        // Simple glob matching - check if path starts with prefix before *
        if let Some(prefix) = pathspec.strip_suffix('*')
            && file.starts_with(prefix) {
                return true;
            }
    }

    false
}

/// Delete working log entries for specific files
fn delete_working_log_for_files(
    repo: &Repository,
    base_commit: &str,
    files: &[String],
) -> Result<(), GitAiError> {
    if files.is_empty() {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(base_commit);

    // Read current initial attributions
    let mut initial_attrs = working_log.read_initial_attributions();

    // Remove entries for the specified files
    for file in files {
        initial_attrs.files.remove(file);
    }

    // Write back the modified attributions
    working_log.write_initial_attributions(initial_attrs.files, initial_attrs.prompts)?;

    // Note: We're not modifying checkpoints here as they're historical records
    // The files were stashed, so we just remove them from the initial attributions

    Ok(())
}
