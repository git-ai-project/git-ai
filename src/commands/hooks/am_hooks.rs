use crate::commands::git_handlers::CommandHooksContext;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::refs::notes_add;
use crate::git::repository::Repository;
use crate::utils::debug_log;
use base64::Engine;

/// Before `git am` runs, capture the current HEAD so we can determine
/// which commits were created by the am operation.
pub fn pre_am_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    context: &mut CommandHooksContext,
) {
    debug_log("=== AM PRE-COMMAND HOOK ===");

    if let Ok(head) = repository.head()
        && let Ok(target) = head.target()
    {
        debug_log(&format!("Captured pre-am HEAD: {}", target));
        context.am_original_head = Some(target);
    }

    // Capture the patch file paths from args so we can read headers later
    let patch_paths = find_patch_paths_from_args(&parsed_args.command_args);
    debug_log(&format!("Patch file paths: {:?}", patch_paths));
    context.am_patch_paths = Some(patch_paths);
}

/// After `git am` completes, walk the new commits and apply attribution
/// from X-Git-AI-Attribution headers found in the corresponding patch files.
pub fn post_am_hook(
    context: &CommandHooksContext,
    _parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    debug_log("=== AM POST-COMMAND HOOK ===");
    debug_log(&format!("Exit status: {}", exit_status));

    if !exit_status.success() {
        debug_log("git am failed, skipping attribution transfer");
        return;
    }

    let original_head = match &context.am_original_head {
        Some(head) => head.clone(),
        None => {
            debug_log("No pre-am HEAD captured, skipping attribution transfer");
            return;
        }
    };

    let patch_paths = match &context.am_patch_paths {
        Some(paths) if !paths.is_empty() => paths.clone(),
        _ => {
            debug_log("No patch paths captured, skipping attribution transfer");
            return;
        }
    };

    // Get the new HEAD
    let new_head = match repository.head() {
        Ok(head) => match head.target() {
            Ok(target) => target,
            Err(e) => {
                debug_log(&format!("Failed to get HEAD target: {}", e));
                return;
            }
        },
        Err(e) => {
            debug_log(&format!("Failed to get HEAD: {}", e));
            return;
        }
    };

    if original_head == new_head {
        debug_log("HEAD unchanged after git am, nothing to do");
        return;
    }

    // Walk the new commits from original_head to new_head
    let new_commits = match crate::authorship::rebase_authorship::walk_commits_to_base(
        repository,
        &new_head,
        &original_head,
    ) {
        Ok(commits) => {
            let mut commits = commits;
            commits.reverse(); // chronological order (oldest first)
            commits
        }
        Err(e) => {
            debug_log(&format!("Failed to walk new commits: {}", e));
            return;
        }
    };

    debug_log(&format!(
        "Found {} new commits and {} patch files",
        new_commits.len(),
        patch_paths.len()
    ));

    // Match commits to patch files by order (git am applies them sequentially)
    for (i, commit_sha) in new_commits.iter().enumerate() {
        if i >= patch_paths.len() {
            debug_log(&format!(
                "No more patch files for commit {} (index {})",
                commit_sha, i
            ));
            break;
        }

        let patch_path = &patch_paths[i];
        debug_log(&format!(
            "Processing commit {} with patch {}",
            commit_sha, patch_path
        ));

        match read_attribution_from_patch(patch_path) {
            Ok(Some(note_content)) => {
                debug_log(&format!(
                    "Found attribution header in patch, writing note for {}",
                    commit_sha
                ));

                // Parse the note to update the base_commit_sha to the new commit
                match crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(&note_content) {
                    Ok(mut log) => {
                        log.metadata.base_commit_sha = commit_sha.clone();
                        match log.serialize_to_string() {
                            Ok(updated_note) => {
                                if let Err(e) = notes_add(repository, commit_sha, &updated_note) {
                                    debug_log(&format!(
                                        "Failed to write attribution note for {}: {}",
                                        commit_sha, e
                                    ));
                                }
                            }
                            Err(e) => {
                                debug_log(&format!(
                                    "Failed to serialize updated note for {}: {}",
                                    commit_sha, e
                                ));
                            }
                        }
                    }
                    Err(_) => {
                        // If we can't parse it as AuthorshipLog, write it as-is
                        if let Err(e) = notes_add(repository, commit_sha, &note_content) {
                            debug_log(&format!(
                                "Failed to write raw attribution note for {}: {}",
                                commit_sha, e
                            ));
                        }
                    }
                }
            }
            Ok(None) => {
                debug_log(&format!("No attribution header in patch {}", patch_path));
            }
            Err(e) => {
                debug_log(&format!(
                    "Error reading attribution from patch {}: {}",
                    patch_path, e
                ));
            }
        }
    }

    debug_log("AM attribution transfer complete");
}

/// Extract patch file paths from git am arguments.
/// git am takes patch files as positional arguments, or reads from stdin/mailbox.
fn find_patch_paths_from_args(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }

        // Skip flags that take values
        if arg == "--directory" || arg == "-d" || arg == "--patch-format" || arg == "-S" {
            skip_next = true;
            continue;
        }

        // Skip boolean flags
        if arg.starts_with('-') {
            continue;
        }

        // This is a positional argument (patch file or directory)
        let path = std::path::Path::new(arg);
        if path.is_file() {
            paths.push(arg.clone());
        } else if path.is_dir() {
            // If it's a directory, find .patch files in it
            if let Ok(entries) = std::fs::read_dir(path) {
                let mut dir_patches: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "patch"))
                    .filter_map(|p| p.to_str().map(|s| s.to_string()))
                    .collect();
                dir_patches.sort();
                paths.extend(dir_patches);
            }
        }
    }

    paths
}

/// Read the X-Git-AI-Attribution header from a patch file and decode its content.
fn read_attribution_from_patch(patch_path: &str) -> Result<Option<String>, String> {
    let content = std::fs::read_to_string(patch_path)
        .map_err(|e| format!("Failed to read patch file: {}", e))?;

    for line in content.lines() {
        if let Some(encoded) = line.strip_prefix("X-Git-AI-Attribution: ") {
            let encoded = encoded.trim();
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("Failed to decode attribution header: {}", e))?;
            let note_content = String::from_utf8(decoded)
                .map_err(|e| format!("Attribution header is not valid UTF-8: {}", e))?;
            return Ok(Some(note_content));
        }

        // Stop looking after the header section (empty line before the body)
        // In email format, headers end at the first blank line
        if line.is_empty() {
            break;
        }
    }

    Ok(None)
}
