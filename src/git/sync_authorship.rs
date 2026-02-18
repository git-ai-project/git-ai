use crate::git::refs::{
    AI_AUTHORSHIP_PUSH_REFSPEC, copy_ref, merge_notes_from_ref, ref_exists, tracking_ref_for_remote,
};
use crate::{
    error::GitAiError,
    git::{cli_parser::ParsedGitInvocation, repository::exec_git},
    utils::debug_log,
};

use super::repository::Repository;

#[cfg(windows)]
fn disabled_hooks_config() -> &'static str {
    "core.hooksPath=NUL"
}

#[cfg(not(windows))]
fn disabled_hooks_config() -> &'static str {
    "core.hooksPath=/dev/null"
}

/// Result of checking for authorship notes on a remote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesExistence {
    /// Notes were found and fetched from the remote
    Found,
    /// Confirmed that no notes exist on the remote
    NotFound,
}

pub fn fetch_remote_from_args(
    repository: &Repository,
    parsed_args: &ParsedGitInvocation,
) -> Result<String, GitAiError> {
    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // 2) Fetch authorship refs from the appropriate remote
    // Try to detect remote (named remote, URL, or local path) from args first
    let positional_remote = extract_remote_from_fetch_args(&parsed_args.command_args);
    let specified_remote = positional_remote.or_else(|| {
        parsed_args
            .command_args
            .iter()
            .find(|a| remote_names.iter().any(|r| r == *a))
            .cloned()
    });

    let remote = specified_remote
        .or_else(|| repository.upstream_remote().ok().flatten())
        .or_else(|| repository.get_default_remote().ok().flatten());

    remote.map(|r| r.to_string()).ok_or_else(|| {
        GitAiError::Generic(
            "Could not determine a remote for fetch/push operation. \
                 No remote was specified in args, no upstream is configured, \
                 and no default remote was found."
                .to_string(),
        )
    })
}

// for use with post-fetch and post-pull and post-clone hooks
// Returns Ok(NotesExistence::Found) if notes were found and fetched,
// Ok(NotesExistence::NotFound) if confirmed no notes exist on remote,
// Err(...) for actual errors (network, permissions, etc.)
pub fn fetch_authorship_notes(
    repository: &Repository,
    remote_name: &str,
) -> Result<NotesExistence, GitAiError> {
    // Generate tracking ref for this remote
    let tracking_ref = tracking_ref_for_remote(remote_name);

    debug_log(&format!(
        "fetching authorship notes for remote '{}' to tracking ref '{}'",
        remote_name, tracking_ref
    ));

    // First, check if the remote has refs/notes/ai using ls-remote
    // This is important for bare repos where the refmap might not be configured
    let mut ls_remote_args = repository.global_args_for_exec();
    ls_remote_args.push("ls-remote".to_string());
    ls_remote_args.push(remote_name.to_string());
    ls_remote_args.push("refs/notes/ai".to_string());

    debug_log(&format!("ls-remote command: {:?}", ls_remote_args));

    match exec_git(&ls_remote_args) {
        Ok(output) => {
            let result = String::from_utf8_lossy(&output.stdout).to_string();
            debug_log(&format!("ls-remote stdout: '{}'", result));
            debug_log(&format!(
                "ls-remote stderr: '{}'",
                String::from_utf8_lossy(&output.stderr)
            ));

            if result.trim().is_empty() {
                debug_log(&format!(
                    "no authorship notes found on remote '{}', nothing to sync",
                    remote_name
                ));
                return Ok(NotesExistence::NotFound);
            }
            debug_log(&format!(
                "found authorship notes on remote '{}'",
                remote_name
            ));
        }
        Err(e) => {
            debug_log(&format!(
                "failed to check for authorship notes on remote '{}': {}",
                remote_name, e
            ));
            // Return error instead of assuming no notes - we don't know the state
            return Err(e);
        }
    }

    // Now fetch the notes to the tracking ref with explicit refspec
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);

    // Build the internal authorship fetch with explicit flags and disabled hooks.
    // IMPORTANT: use repository.global_args_for_exec() to ensure -C flag is present for bare repos.
    let fetch_authorship = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &fetch_refspec,
    );

    debug_log(&format!("fetch command: {:?}", fetch_authorship));

    match exec_git(&fetch_authorship) {
        Ok(output) => {
            debug_log(&format!(
                "fetch stdout: '{}'",
                String::from_utf8_lossy(&output.stdout)
            ));
            debug_log(&format!(
                "fetch stderr: '{}'",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Err(e) => {
            debug_log(&format!("authorship fetch failed: {}", e));
            return Err(e);
        }
    }

    // After successful fetch, merge the tracking ref into refs/notes/ai
    let local_notes_ref = "refs/notes/ai";

    if crate::git::refs::ref_exists(repository, &tracking_ref) {
        if crate::git::refs::ref_exists(repository, local_notes_ref) {
            // Both exist - merge them
            debug_log(&format!(
                "merging authorship notes from {} into {}",
                tracking_ref, local_notes_ref
            ));
            if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
                debug_log(&format!("notes merge failed: {}", e));
                // Don't fail on merge errors, just log and continue
            }
        } else {
            // Only tracking ref exists - copy it to local
            debug_log(&format!(
                "initializing {} from tracking ref {}",
                local_notes_ref, tracking_ref
            ));
            if let Err(e) = copy_ref(repository, &tracking_ref, local_notes_ref) {
                debug_log(&format!("notes copy failed: {}", e));
                // Don't fail on copy errors, just log and continue
            }
        }
    } else {
        debug_log(&format!(
            "tracking ref {} was not created after fetch",
            tracking_ref
        ));
    }

    Ok(NotesExistence::Found)
}
// for use with post-push hook
pub fn push_authorship_notes(repository: &Repository, remote_name: &str) -> Result<(), GitAiError> {
    // STEP 1: Fetch remote notes into tracking ref and merge before pushing
    // This ensures we don't lose notes from other branches/clones
    let tracking_ref = tracking_ref_for_remote(remote_name);
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);

    let fetch_before_push = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &fetch_refspec,
    );

    debug_log(&format!(
        "pre-push authorship fetch: {:?}",
        &fetch_before_push
    ));

    // Fetch is best-effort; if it fails (e.g., no remote notes yet), continue
    if exec_git(&fetch_before_push).is_ok() {
        // Merge fetched notes into local refs/notes/ai
        let local_notes_ref = "refs/notes/ai";

        if ref_exists(repository, &tracking_ref) {
            if ref_exists(repository, local_notes_ref) {
                // Both exist - merge them
                debug_log(&format!(
                    "pre-push: merging {} into {}",
                    tracking_ref, local_notes_ref
                ));
                if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
                    debug_log(&format!("pre-push notes merge failed: {}", e));
                }
            } else {
                // Only tracking ref exists - copy it to local
                debug_log(&format!(
                    "pre-push: initializing {} from {}",
                    local_notes_ref, tracking_ref
                ));
                if let Err(e) = copy_ref(repository, &tracking_ref, local_notes_ref) {
                    debug_log(&format!("pre-push notes copy failed: {}", e));
                }
            }
        }
    }

    // STEP 2: Push notes without force (requires fast-forward)
    let push_authorship =
        build_authorship_push_args(repository.global_args_for_exec(), remote_name);

    debug_log(&format!(
        "pushing authorship refs (no force): {:?}",
        &push_authorship
    ));
    if let Err(e) = exec_git(&push_authorship) {
        // Best-effort; don't fail user operation due to authorship sync issues
        debug_log(&format!("authorship push skipped due to error: {}", e));
        return Err(e);
    }

    Ok(())
}

fn extract_remote_from_fetch_args(args: &[String]) -> Option<String> {
    let mut after_double_dash = false;

    for arg in args {
        if !after_double_dash {
            if arg == "--" {
                after_double_dash = true;
                continue;
            }
            if arg.starts_with('-') {
                // Option; skip
                continue;
            }
        }

        // Candidate positional arg; determine if it's a repository URL/path
        let s = arg.as_str();

        // 1) URL forms (https://, ssh://, file://, git://, etc.)
        if s.contains("://") || s.starts_with("file://") {
            return Some(arg.clone());
        }

        // 2) SCP-like syntax: user@host:path
        if s.contains('@') && s.contains(':') && !s.contains("://") {
            return Some(arg.clone());
        }

        // 3) Local path forms
        if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with("~/")
        {
            return Some(arg.clone());
        }

        // Heuristic: bare repo directories often end with .git
        if s.ends_with(".git") {
            return Some(arg.clone());
        }

        // 4) As a last resort, if the path exists on disk, treat as local path
        if std::path::Path::new(s).exists() {
            return Some(arg.clone());
        }

        // Otherwise, do not treat this positional token as a repository; likely a refspec
        break;
    }

    None
}

fn with_disabled_hooks(mut args: Vec<String>) -> Vec<String> {
    args.push("-c".to_string());
    args.push(disabled_hooks_config().to_string());
    args
}

fn build_authorship_fetch_args(
    global_args: Vec<String>,
    remote_name: &str,
    fetch_refspec: &str,
) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("fetch".to_string());
    args.push("--no-tags".to_string());
    args.push("--recurse-submodules=no".to_string());
    args.push("--no-write-fetch-head".to_string());
    args.push("--no-write-commit-graph".to_string());
    args.push("--no-auto-maintenance".to_string());
    args.push(remote_name.to_string());
    args.push(fetch_refspec.to_string());
    args
}

fn build_authorship_push_args(global_args: Vec<String>, remote_name: &str) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("push".to_string());
    args.push("--quiet".to_string());
    args.push("--no-recurse-submodules".to_string());
    args.push("--no-verify".to_string());
    args.push("--no-signed".to_string());
    args.push(remote_name.to_string());
    args.push(AI_AUTHORSHIP_PUSH_REFSPEC.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorship_fetch_args_always_disable_hooks() {
        let disabled_hooks = disabled_hooks_config();
        let args = build_authorship_fetch_args(
            vec!["-C".to_string(), "/tmp/repo".to_string()],
            "origin",
            "+refs/notes/ai:refs/notes/ai-remote/origin",
        );

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == disabled_hooks)
        );
        assert!(args.contains(&"fetch".to_string()));
    }

    #[test]
    fn authorship_push_args_always_disable_hooks() {
        let disabled_hooks = disabled_hooks_config();
        let args =
            build_authorship_push_args(vec!["-C".to_string(), "/tmp/repo".to_string()], "origin");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == disabled_hooks)
        );
        assert!(args.contains(&"push".to_string()));
    }
}
