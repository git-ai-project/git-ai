//! Authorship helper functions used by the daemon that were previously in the
//! hooks modules. Extracted during the git hooks removal to keep the daemon
//! functional without the hooks infrastructure.

use crate::authorship::rebase_authorship::walk_commits_to_base;
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;
use crate::git::sync_authorship::push_authorship_notes;
use crate::utils::debug_log;

// ---------------------------------------------------------------------------
// Push authorship
// ---------------------------------------------------------------------------

pub fn run_pre_push_authorship(parsed_args: &ParsedGitInvocation, repository: &Repository) {
    crate::commands::upgrade::maybe_schedule_background_update_check();

    if should_skip_authorship_push(&parsed_args.command_args) {
        return;
    }

    let Some(remote) = resolve_push_remote(parsed_args, repository) else {
        debug_log("no remotes found for authorship push; skipping");
        return;
    };

    debug_log(&format!(
        "started pushing authorship notes to remote: {}",
        remote
    ));

    crate::observability::spawn_background_flush();

    // Spawn CAS flush if prompt_storage is "default" (CAS upload mode)
    if crate::config::Config::get().prompt_storage() == "default" {
        crate::commands::flush_cas::spawn_background_cas_flush();
    }

    if let Err(e) = push_authorship_notes(repository, &remote) {
        debug_log(&format!("authorship push failed: {}", e));
    }
}

fn should_skip_authorship_push(command_args: &[String]) -> bool {
    use crate::git::cli_parser::is_dry_run;
    is_dry_run(command_args)
        || command_args.iter().any(|a| a == "-d" || a == "--delete")
        || command_args.iter().any(|a| a == "--mirror")
}

fn resolve_push_remote(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<String> {
    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let upstream_remote = repository.upstream_remote().ok().flatten();
    let default_remote = repository.get_default_remote().ok().flatten();

    resolve_push_remote_from_parts(
        &parsed_args.command_args,
        &remote_names,
        upstream_remote,
        default_remote,
    )
}

fn resolve_push_remote_from_parts(
    command_args: &[String],
    known_remotes: &[String],
    upstream_remote: Option<String>,
    default_remote: Option<String>,
) -> Option<String> {
    let positional_remote = extract_remote_from_push_args(command_args, known_remotes);

    let specified_remote = positional_remote.or_else(|| {
        command_args
            .iter()
            .find(|arg| known_remotes.iter().any(|remote| remote == *arg))
            .cloned()
    });

    specified_remote.or(upstream_remote).or(default_remote)
}

fn extract_remote_from_push_args(args: &[String], known_remotes: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return args.get(i + 1).cloned();
        }
        if arg.starts_with('-') {
            if let Some((flag, value)) = is_push_option_with_inline_value(arg) {
                if flag == "--repo" {
                    return Some(value.to_string());
                }
                i += 1;
                continue;
            }

            if option_consumes_separate_value(arg.as_str()) {
                if arg == "--repo" {
                    return args.get(i + 1).cloned();
                }
                i += 2;
                continue;
            }

            i += 1;
            continue;
        }
        return Some(arg.clone());
    }

    known_remotes
        .iter()
        .find(|r| args.iter().any(|arg| arg == *r))
        .cloned()
}

fn is_push_option_with_inline_value(arg: &str) -> Option<(&str, &str)> {
    if let Some((flag, value)) = arg.split_once('=') {
        Some((flag, value))
    } else if (arg.starts_with("-C") || arg.starts_with("-c")) && arg.len() > 2 {
        let flag = &arg[..2];
        let value = &arg[2..];
        Some((flag, value))
    } else {
        None
    }
}

fn option_consumes_separate_value(arg: &str) -> bool {
    matches!(
        arg,
        "--repo" | "--receive-pack" | "--exec" | "-o" | "--push-option" | "-c" | "-C"
    )
}

// ---------------------------------------------------------------------------
// Stash authorship
// ---------------------------------------------------------------------------

pub fn save_stash_authorship_log(
    repo: &Repository,
    head_sha: &str,
    stash_sha: &str,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    debug_log(&format!("Stash created with SHA: {}", stash_sha));

    let working_log_va =
        VirtualAttributions::from_just_working_log(repo.clone(), head_sha.to_string(), None)?;

    let filtered_files: Vec<String> = if pathspecs.is_empty() {
        working_log_va
            .files()
            .into_iter()
            .map(|f| f.to_string())
            .collect()
    } else {
        working_log_va
            .files()
            .into_iter()
            .filter(|file| file_matches_pathspecs(file, pathspecs))
            .map(|f| f.to_string())
            .collect()
    };

    if filtered_files.is_empty() {
        debug_log("No attributions to save for stash");
        delete_working_log_for_files(repo, head_sha, &filtered_files)?;
        return Ok(());
    }

    debug_log(&format!(
        "Saving attributions for {} files (pathspecs: {:?})",
        filtered_files.len(),
        pathspecs
    ));

    let mut authorship_log = working_log_va.to_authorship_log()?;
    authorship_log
        .attestations
        .retain(|a| filtered_files.contains(&a.file_path));

    let json = authorship_log
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    save_stash_note(repo, stash_sha, &json)?;

    debug_log(&format!(
        "Saved authorship log to refs/notes/ai-stash for stash {}",
        stash_sha
    ));

    delete_working_log_for_files(repo, head_sha, &filtered_files)?;
    debug_log(&format!(
        "Deleted working log entries for {} files",
        filtered_files.len()
    ));

    Ok(())
}

pub fn restore_stash_attributions(
    repo: &Repository,
    head_sha: &str,
    stash_sha: &str,
) -> Result<(), GitAiError> {
    debug_log(&format!(
        "Restoring stash attributions from SHA: {}",
        stash_sha
    ));

    let note_content = match read_stash_note(repo, stash_sha) {
        Ok(content) => content,
        Err(_) => {
            debug_log("No authorship log found in refs/notes/ai-stash for this stash");
            return Ok(());
        }
    };

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

    if !initial_files.is_empty() || !initial_prompts.is_empty() {
        let working_log = repo.storage.working_log_for_base_commit(head_sha)?;
        let initial_file_contents =
            load_stashed_file_contents(repo, stash_sha, initial_files.keys())?;
        working_log.write_initial_attributions_with_contents(
            initial_files.clone(),
            initial_prompts.clone(),
            initial_file_contents,
        )?;

        debug_log(&format!(
            "Wrote INITIAL attributions to working log for {}",
            head_sha
        ));
    }

    Ok(())
}

pub fn extract_stash_pathspecs(parsed_args: &ParsedGitInvocation) -> Vec<String> {
    let mut pathspecs = Vec::new();
    let mut found_separator = false;
    let mut skip_next = false;

    for (i, arg) in parsed_args.command_args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        if arg == "--" {
            found_separator = true;
            continue;
        }

        if found_separator {
            pathspecs.push(arg.clone());
            continue;
        }

        if arg.starts_with('-') {
            if stash_option_consumes_value(arg) {
                skip_next = true;
            }
            continue;
        }

        if i == 0 && (arg == "push" || arg == "save" || arg == "pop" || arg == "apply") {
            continue;
        }

        if i == 1 && arg.starts_with("stash@") {
            continue;
        }

        pathspecs.push(arg.clone());
    }

    debug_log(&format!("Extracted pathspecs: {:?}", pathspecs));
    pathspecs
}

fn stash_option_consumes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-m" | "--message" | "--pathspec-from-file" | "--pathspec-file-nul"
    )
}

fn file_matches_pathspecs(file: &str, pathspecs: &[String]) -> bool {
    if pathspecs.is_empty() {
        return true;
    }

    for pathspec in pathspecs {
        if file == pathspec {
            return true;
        }
        if pathspec.ends_with('/') && file.starts_with(pathspec) {
            return true;
        }
        if file.starts_with(&format!("{}/", pathspec)) {
            return true;
        }
        if let Some(prefix) = pathspec.strip_suffix('*')
            && file.starts_with(prefix)
        {
            return true;
        }
    }

    false
}

fn delete_working_log_for_files(
    repo: &Repository,
    base_commit: &str,
    files: &[String],
) -> Result<(), GitAiError> {
    if files.is_empty() {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(base_commit)?;
    let mut initial_attrs = working_log.read_initial_attributions();

    for file in files {
        initial_attrs.files.remove(file);
        initial_attrs.file_blobs.remove(file);
    }

    working_log.write_initial(initial_attrs)?;
    Ok(())
}

fn save_stash_note(repo: &Repository, stash_sha: &str, content: &str) -> Result<(), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push("--ref=ai-stash".to_string());
    args.push("add".to_string());
    args.push("-f".to_string());
    args.push("-F".to_string());
    args.push("-".to_string());
    args.push(stash_sha.to_string());

    exec_git_stdin(&args, content.as_bytes())?;
    Ok(())
}

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

// ---------------------------------------------------------------------------
// Rebase commit mappings
// ---------------------------------------------------------------------------

pub fn build_rebase_commit_mappings(
    repository: &Repository,
    original_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), GitAiError> {
    if let Some(onto_head) = onto_head
        && !crate::git::repo_state::is_valid_git_oid(onto_head)
    {
        return Err(GitAiError::Generic(format!(
            "rebase mapping expected resolved onto oid, got '{}'",
            onto_head
        )));
    }

    let new_head_commit = repository.find_commit(new_head.to_string())?;
    let original_head_commit = repository.find_commit(original_head.to_string())?;

    let merge_base = repository.merge_base(original_head_commit.id(), new_head_commit.id())?;

    let original_base = onto_head
        .and_then(|onto| original_equivalent_for_rewritten_commit(repository, onto))
        .filter(|mapped| mapped != original_head && is_ancestor(repository, mapped, original_head))
        .unwrap_or_else(|| merge_base.clone());

    let mut original_commits = walk_commits_to_base(repository, original_head, &original_base)?;
    original_commits.reverse();

    if original_commits.is_empty() {
        debug_log(&format!(
            "Commit mapping: 0 original -> 0 new (merge_base: {}, original_base: {})",
            merge_base, original_base
        ));
        return Ok((original_commits, Vec::new()));
    }

    let new_commits_base = onto_head
        .filter(|onto| is_ancestor(repository, onto, new_head))
        .unwrap_or(merge_base.as_str());

    let mut new_commits = walk_commits_to_base(repository, new_head, new_commits_base)?;
    new_commits.reverse();

    debug_log(&format!(
        "Commit mapping: {} original -> {} new (merge_base: {}, original_base: {}, new_base: {})",
        original_commits.len(),
        new_commits.len(),
        merge_base,
        original_base,
        new_commits_base
    ));

    Ok((original_commits, new_commits))
}

fn original_equivalent_for_rewritten_commit(
    repository: &Repository,
    rewritten_commit: &str,
) -> Option<String> {
    let events = repository.storage.read_rewrite_events().ok()?;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { rebase_complete } => {
                if let Some(index) = rebase_complete
                    .new_commits
                    .iter()
                    .position(|commit| commit == rewritten_commit)
                {
                    return rebase_complete.original_commits.get(index).cloned();
                }
            }
            RewriteLogEvent::CherryPickComplete {
                cherry_pick_complete,
            } => {
                if let Some(index) = cherry_pick_complete
                    .new_commits
                    .iter()
                    .position(|commit| commit == rewritten_commit)
                {
                    return cherry_pick_complete.source_commits.get(index).cloned();
                }
            }
            RewriteLogEvent::CommitAmend { commit_amend }
                if commit_amend.amended_commit_sha == rewritten_commit =>
            {
                return Some(commit_amend.original_commit);
            }
            _ => {}
        }
    }
    None
}

fn is_ancestor(repository: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    crate::git::repository::exec_git(&args).is_ok()
}
