use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};

/// Save the current working log as an authorship log in git notes (refs/notes/ai-stash)
pub(crate) fn save_stash_authorship_log(
    repo: &Repository,
    head_sha: &str,
    stash_sha: &str,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    tracing::debug!("Stash created with SHA: {}", stash_sha);

    // Build VirtualAttributions from the working log before it was cleared
    let working_log_va =
        VirtualAttributions::from_just_working_log(repo.clone(), head_sha.to_string(), None)?;

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
        tracing::debug!("No attributions to save for stash");
        delete_working_log_for_files(repo, head_sha, &filtered_files)?;
        return Ok(());
    }

    tracing::debug!(
        "Saving attributions for {} files (pathspecs: {:?})",
        filtered_files.len(),
        pathspecs
    );

    // Convert to authorship log, filtering to only include matched files
    let mut authorship_log = working_log_va.to_authorship_log()?;
    authorship_log
        .attestations
        .retain(|a| filtered_files.contains(&a.file_path));

    // Save as git note at refs/notes/ai-stash
    let json = authorship_log
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    save_stash_note(repo, stash_sha, &json)?;

    tracing::debug!(
        "Saved authorship log to refs/notes/ai-stash for stash {}",
        stash_sha
    );

    // Delete the working log entries for files that were stashed
    delete_working_log_for_files(repo, head_sha, &filtered_files)?;
    tracing::debug!(
        "Deleted working log entries for {} files",
        filtered_files.len()
    );

    Ok(())
}

/// Restore attributions from a stash by reading the git note and converting to INITIAL attributions
pub(crate) fn restore_stash_attributions(
    repo: &Repository,
    head_sha: &str,
    stash_sha: &str,
) -> Result<(), GitAiError> {
    tracing::debug!("Restoring stash attributions from SHA: {}", stash_sha);

    // Try to read authorship log from git note (refs/notes/ai-stash)
    let note_content = match read_stash_note(repo, stash_sha) {
        Ok(content) => content,
        Err(_) => {
            tracing::debug!("No authorship log found in refs/notes/ai-stash for this stash");
            return Ok(());
        }
    };

    // Parse the authorship log
    let authorship_log = match crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(&note_content) {
        Ok(log) => log,
        Err(e) => {
            tracing::debug!("Failed to parse stash authorship log: {}", e);
            return Ok(());
        }
    };

    tracing::debug!(
        "Loaded authorship log from stash: {} files, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    );

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

    let initial_humans = authorship_log.metadata.humans.clone();
    let initial_sessions = authorship_log.metadata.sessions.clone();

    // Write INITIAL attributions to working log
    if !initial_files.is_empty() || !initial_prompts.is_empty() || !initial_sessions.is_empty() {
        let working_log = repo.storage.working_log_for_base_commit(head_sha)?;
        let initial_file_contents =
            load_stashed_file_contents(repo, stash_sha, initial_files.keys())?;
        working_log.write_initial_attributions_with_contents(
            initial_files.clone(),
            initial_prompts.clone(),
            initial_humans,
            initial_file_contents,
            initial_sessions,
        )?;

        tracing::debug!(
            "✓ Wrote INITIAL attributions to working log for {}",
            head_sha
        );
    }

    Ok(())
}

fn load_stashed_file_contents<'a, I>(
    repo: &Repository,
    stash_sha: &str,
    file_paths: I,
) -> Result<std::collections::HashMap<String, String>, GitAiError>
where
    I: IntoIterator<Item = &'a String>,
{
    let stash_commit = repo.find_commit(stash_sha.to_string())?;
    let untracked_parent_sha = stash_commit.parent(2).ok().map(|commit| commit.id());
    let mut file_contents = std::collections::HashMap::new();

    for file_path in file_paths {
        let content = repo
            .get_file_content(file_path, stash_sha)
            .ok()
            .or_else(|| {
                untracked_parent_sha
                    .as_ref()
                    .and_then(|parent_sha| repo.get_file_content(file_path, parent_sha).ok())
            })
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default();
        file_contents.insert(file_path.clone(), content);
    }

    Ok(file_contents)
}

/// Save a note to refs/notes/ai-stash
fn save_stash_note(repo: &Repository, stash_sha: &str, content: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai-stash".to_string());
    args.push("add".to_string());
    args.push("-f".to_string()); // Force overwrite if exists
    args.push("-F".to_string());
    args.push("-".to_string()); // Read note content from stdin
    args.push(stash_sha.to_string());

    // Use stdin to provide the note content to avoid command line length limits
    exec_git_stdin(&args, content.as_bytes())?;
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
            && file.starts_with(prefix)
        {
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

    let working_log = repo.storage.working_log_for_base_commit(base_commit)?;

    // Read current initial attributions
    let mut initial_attrs = working_log.read_initial_attributions();

    // Remove entries for the specified files
    for file in files {
        initial_attrs.files.remove(file);
        initial_attrs.file_blobs.remove(file);
    }

    // Write back the modified attributions
    working_log.write_initial(initial_attrs)?;

    // Note: We're not modifying checkpoints here as they're historical records
    // The files were stashed, so we just remove them from the initial attributions

    Ok(())
}
