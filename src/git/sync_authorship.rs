use crate::git::refs::{
    AI_AUTHORSHIP_PUSH_REFSPEC, AI_SHARDED_NOTES_PREFIX, copy_ref, fallback_merge_notes_ours,
    merge_notes_from_ref, ref_exists, tracking_ref_for_remote,
};
use crate::{
    error::GitAiError,
    git::{
        cli_parser::ParsedGitInvocation,
        repository::{exec_git, exec_git_stdin},
    },
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

fn sharded_notes_enabled() -> bool {
    if crate::daemon::daemon_process_active() {
        let config = crate::config::Config::fresh();
        config.get_feature_flags().sharded_notes
    } else {
        crate::config::Config::get()
            .get_feature_flags()
            .sharded_notes
    }
}

/// Tracking ref prefix for sharded notes from a specific remote.
/// e.g. "refs/notes/ai-s-remote/origin/ab"
fn shard_tracking_ref_prefix(remote_name: &str) -> String {
    format!(
        "refs/notes/ai-s-remote/{}/",
        sanitize_remote_name_for_ref(remote_name)
    )
}

/// Sanitize a remote name for use in a ref path (same logic as refs.rs).
fn sanitize_remote_name_for_ref(remote: &str) -> String {
    remote
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolved shard ref pair: tracking ref name, local ref name, and their resolved OIDs.
struct ShardRefPair {
    tracking_ref: String,
    tracking_oid: String,
    local_shard_ref: String,
    local_oid: Option<String>, // None means local doesn't exist yet
}

/// Discover shard tracking refs and resolve both tracking and local OIDs in a single
/// `for-each-ref` + `cat-file --batch-check` round-trip pair (2 git processes total,
/// regardless of shard count).
fn resolve_shard_ref_pairs(
    repository: &Repository,
    remote_name: &str,
) -> Result<Vec<ShardRefPair>, GitAiError> {
    let prefix = shard_tracking_ref_prefix(remote_name);

    // 1. List tracking refs with their OIDs in one call
    let mut args = repository.global_args_for_exec();
    args.push("for-each-ref".to_string());
    args.push("--format=%(objectname) %(refname)".to_string());
    args.push(prefix.clone());

    let output = match exec_git(&args) {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| GitAiError::Generic("bad utf8".to_string()))?;

    // Each entry: (tracking_oid, tracking_refname, local_shard_ref)
    let tracking_refs: Vec<(String, String, String)> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let (oid, refname) = line.split_once(' ')?;
            let shard = refname.strip_prefix(&prefix)?;
            let local_ref = format!("{}{}", AI_SHARDED_NOTES_PREFIX, shard);
            Some((oid.to_string(), refname.to_string(), local_ref))
        })
        .collect();

    if tracking_refs.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Batch-resolve all local shard ref OIDs in one cat-file call
    let mut batch_args = repository.global_args_for_exec();
    batch_args.push("cat-file".to_string());
    batch_args.push("--batch-check".to_string());

    let stdin_data: String = tracking_refs
        .iter()
        .map(|(_, _, local_ref)| local_ref.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let batch_output = exec_git_stdin(&batch_args, stdin_data.as_bytes())?;
    let batch_stdout = String::from_utf8(batch_output.stdout)
        .map_err(|_| GitAiError::Generic("bad utf8".to_string()))?;

    let pairs: Vec<ShardRefPair> = tracking_refs
        .into_iter()
        .zip(batch_stdout.lines())
        .map(
            |((tracking_oid, tracking_ref, local_shard_ref), batch_line)| {
                // cat-file --batch-check output: "<oid> commit <size>" or "<ref> missing"
                let local_oid = if batch_line.contains("missing") {
                    None
                } else {
                    batch_line.split_whitespace().next().map(|s| s.to_string())
                };
                ShardRefPair {
                    tracking_ref,
                    tracking_oid,
                    local_shard_ref,
                    local_oid,
                }
            },
        )
        .collect();

    Ok(pairs)
}

/// Merge all shard tracking refs into their corresponding local shard refs.
///
/// Optimized to minimize git process invocations:
/// - 2 processes to discover and resolve all shard pairs (for-each-ref + cat-file)
/// - Skips shards where tracking == local (no change) — zero cost
/// - Batches all "copy" operations (local doesn't exist) into one update-ref --stdin call
/// - Only runs `git notes merge` for genuinely diverged shards (typically 1-2 per fetch)
fn merge_shard_tracking_refs(repository: &Repository, remote_name: &str) {
    let pairs = match resolve_shard_ref_pairs(repository, remote_name) {
        Ok(p) => p,
        Err(e) => {
            debug_log(&format!("failed to resolve shard ref pairs: {}", e));
            return;
        }
    };

    if pairs.is_empty() {
        return;
    }

    // Partition into: copies (local doesn't exist) and merges (both exist, different OIDs)
    let mut copies: Vec<(&str, &str)> = Vec::new(); // (local_ref, tracking_oid)
    let mut merges: Vec<(&str, &str)> = Vec::new(); // (local_ref, tracking_ref)

    for pair in &pairs {
        match &pair.local_oid {
            None => {
                // Local doesn't exist — copy tracking OID
                copies.push((&pair.local_shard_ref, &pair.tracking_oid));
            }
            Some(local_oid) if local_oid == &pair.tracking_oid => {
                // Already in sync — skip
            }
            Some(_) => {
                // Both exist, different — need merge
                merges.push((&pair.local_shard_ref, &pair.tracking_ref));
            }
        }
    }

    debug_log(&format!(
        "shard merge: {} unchanged, {} copies, {} merges",
        pairs.len() - copies.len() - merges.len(),
        copies.len(),
        merges.len(),
    ));

    // Batch all copies into one update-ref --stdin call
    if !copies.is_empty() {
        let mut args = repository.global_args_for_exec();
        args.push("update-ref".to_string());
        args.push("--stdin".to_string());

        let mut stdin_data = String::new();
        for (local_ref, tracking_oid) in &copies {
            // "create <ref> <oid>\n" — sets ref to oid, fails if ref already exists
            // (safe here because we confirmed local doesn't exist)
            stdin_data.push_str(&format!("create {} {}\n", local_ref, tracking_oid));
        }

        if let Err(e) = exec_git_stdin(&args, stdin_data.as_bytes()) {
            debug_log(&format!("batch shard copy failed: {}", e));
            // Fall back to individual copies
            for (local_ref, _tracking_oid) in &copies {
                if let Some(pair) = pairs.iter().find(|p| p.local_shard_ref == **local_ref)
                    && let Err(e) = copy_ref(repository, &pair.tracking_ref, local_ref)
                {
                    debug_log(&format!(
                        "shard copy failed for {} <- {}: {}",
                        local_ref, pair.tracking_ref, e
                    ));
                }
            }
        }
    }

    // Merges: these are genuinely diverged shards — run notes merge per shard.
    // Typically only 1-2 shards per fetch, so this is acceptable.
    for (local_ref, tracking_ref) in &merges {
        let shard_name = local_ref.strip_prefix("refs/notes/").unwrap_or(local_ref);
        let mut args = repository.global_args_for_exec();
        args.push("notes".to_string());
        args.push(format!("--ref={}", shard_name));
        args.push("merge".to_string());
        args.push("-s".to_string());
        args.push("ours".to_string());
        args.push("--quiet".to_string());
        args.push(tracking_ref.to_string());

        if let Err(e) = exec_git(&args) {
            debug_log(&format!(
                "shard merge failed for {} <- {}: {}",
                local_ref, tracking_ref, e
            ));
        }
    }
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
    let tracking_ref = tracking_ref_for_remote(remote_name);
    let sharded = sharded_notes_enabled();

    debug_log(&format!(
        "fetching authorship notes for remote '{}' to tracking ref '{}'",
        remote_name, tracking_ref
    ));

    // Fetch legacy ref
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);
    let fetch_authorship = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &[fetch_refspec.as_str()],
    );

    debug_log(&format!("fetch command: {:?}", fetch_authorship));

    let legacy_found = match exec_git(&fetch_authorship) {
        Ok(output) => {
            debug_log(&format!(
                "fetch stdout: '{}'",
                String::from_utf8_lossy(&output.stdout)
            ));
            debug_log(&format!(
                "fetch stderr: '{}'",
                String::from_utf8_lossy(&output.stderr)
            ));
            true
        }
        Err(e) => {
            if is_missing_remote_notes_ref_error(&e) {
                debug_log(&format!(
                    "no legacy authorship notes found on remote '{}'",
                    remote_name
                ));
                false
            } else {
                debug_log(&format!("authorship fetch failed: {}", e));
                return Err(e);
            }
        }
    };

    // Fetch shard refs separately so a missing legacy ref doesn't block shard fetching
    let mut shards_found = false;
    if sharded {
        let shard_prefix = shard_tracking_ref_prefix(remote_name);
        let shard_refspec = format!("+{}*:{}*", AI_SHARDED_NOTES_PREFIX, shard_prefix);
        let shard_fetch = build_authorship_fetch_args(
            repository.global_args_for_exec(),
            remote_name,
            &[shard_refspec.as_str()],
        );

        debug_log(&format!("shard fetch command: {:?}", shard_fetch));

        match exec_git(&shard_fetch) {
            Ok(_) => {
                shards_found = true;
            }
            Err(e) => {
                // Wildcard refspecs that match nothing don't error, so this is a real failure
                debug_log(&format!("shard fetch failed: {}", e));
            }
        }
    }

    if !legacy_found && !shards_found {
        return Ok(NotesExistence::NotFound);
    }

    // Merge the legacy tracking ref into refs/notes/ai
    let local_notes_ref = "refs/notes/ai";

    if crate::git::refs::ref_exists(repository, &tracking_ref) {
        if crate::git::refs::ref_exists(repository, local_notes_ref) {
            debug_log(&format!(
                "merging authorship notes from {} into {}",
                tracking_ref, local_notes_ref
            ));
            if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
                debug_log(&format!("notes merge failed: {}", e));
                // Fallback: manually merge notes when git notes merge crashes
                if let Err(e2) = fallback_merge_notes_ours(repository, &tracking_ref) {
                    debug_log(&format!("fallback merge also failed: {}", e2));
                }
            }
        } else {
            debug_log(&format!(
                "initializing {} from tracking ref {}",
                local_notes_ref, tracking_ref
            ));
            if let Err(e) = copy_ref(repository, &tracking_ref, local_notes_ref) {
                debug_log(&format!("notes copy failed: {}", e));
            }
        }
    } else {
        debug_log(&format!(
            "tracking ref {} was not created after fetch",
            tracking_ref
        ));
    }

    // Merge shard tracking refs
    if sharded {
        merge_shard_tracking_refs(repository, remote_name);
    }

    Ok(NotesExistence::Found)
}

fn is_missing_remote_notes_ref_error(error: &GitAiError) -> bool {
    let GitAiError::GitCliError { stderr, .. } = error else {
        return false;
    };

    let stderr_lower = stderr.to_ascii_lowercase();
    stderr_lower.contains("refs/notes/ai")
        && (stderr_lower.contains("couldn't find remote ref")
            || stderr_lower.contains("could not find remote ref")
            || stderr_lower.contains("remote ref does not exist")
            || stderr_lower.contains("not our ref"))
}
/// Maximum number of fetch-merge-push attempts before giving up.
/// On busy monorepos, concurrent pushers can cause non-fast-forward rejections
/// even after a successful merge, so we retry the full cycle.
const PUSH_NOTES_MAX_ATTEMPTS: usize = 3;

// for use with post-push hook
pub fn push_authorship_notes(repository: &Repository, remote_name: &str) -> Result<(), GitAiError> {
    let mut last_error = None;

    for attempt in 0..PUSH_NOTES_MAX_ATTEMPTS {
        if attempt > 0 {
            debug_log(&format!(
                "retrying notes push (attempt {}/{})",
                attempt + 1,
                PUSH_NOTES_MAX_ATTEMPTS
            ));
        }

        fetch_and_merge_tracking_notes(repository, remote_name);

        // Push notes without force (requires fast-forward)
        let sharded = sharded_notes_enabled();
        let shard_push_refspec =
            format!("{}*:{}*", AI_SHARDED_NOTES_PREFIX, AI_SHARDED_NOTES_PREFIX);
        let mut push_refspecs: Vec<&str> = vec![AI_AUTHORSHIP_PUSH_REFSPEC];
        if sharded {
            push_refspecs.push(&shard_push_refspec);
        }

        let push_args = build_authorship_push_args(
            repository.global_args_for_exec(),
            remote_name,
            &push_refspecs,
        );

        debug_log(&format!(
            "pushing authorship refs (no force): {:?}",
            &push_args
        ));

        match exec_git(&push_args) {
            Ok(_) => return Ok(()),
            Err(e) => {
                debug_log(&format!("authorship push failed: {}", e));
                if is_non_fast_forward_error(&e) && attempt + 1 < PUSH_NOTES_MAX_ATTEMPTS {
                    // Another pusher updated remote notes between our merge and push.
                    // Retry the full fetch-merge-push cycle.
                    last_error = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| GitAiError::Generic("notes push exhausted retries".to_string())))
}

/// Fetch remote notes into a tracking ref and merge into local refs/notes/ai.
/// Fetches legacy and shard refs separately so a missing ref doesn't block the other.
fn fetch_and_merge_tracking_notes(repository: &Repository, remote_name: &str) {
    let sharded = sharded_notes_enabled();
    let tracking_ref = tracking_ref_for_remote(remote_name);

    // Fetch legacy ref (best-effort)
    let fetch_refspec = format!("+refs/notes/ai:{}", tracking_ref);
    let legacy_fetch = build_authorship_fetch_args(
        repository.global_args_for_exec(),
        remote_name,
        &[fetch_refspec.as_str()],
    );

    debug_log(&format!("pre-push authorship fetch: {:?}", &legacy_fetch));

    if exec_git(&legacy_fetch).is_ok() {
        let local_notes_ref = "refs/notes/ai";

        if ref_exists(repository, &tracking_ref) {
            if ref_exists(repository, local_notes_ref) {
                debug_log(&format!(
                    "pre-push: merging {} into {}",
                    tracking_ref, local_notes_ref
                ));
                if let Err(e) = merge_notes_from_ref(repository, &tracking_ref) {
                    debug_log(&format!("pre-push notes merge failed: {}", e));
                    // Fallback: manually merge notes when git notes merge crashes
                    if let Err(e2) = fallback_merge_notes_ours(repository, &tracking_ref) {
                        debug_log(&format!("pre-push fallback merge also failed: {}", e2));
                    }
                }
            } else {
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

    // Fetch shard refs separately (best-effort)
    if sharded {
        let shard_prefix = shard_tracking_ref_prefix(remote_name);
        let shard_fetch_refspec = format!("+{}*:{}*", AI_SHARDED_NOTES_PREFIX, shard_prefix);
        let shard_fetch = build_authorship_fetch_args(
            repository.global_args_for_exec(),
            remote_name,
            &[shard_fetch_refspec.as_str()],
        );

        debug_log(&format!("pre-push shard fetch: {:?}", &shard_fetch));

        if exec_git(&shard_fetch).is_ok() {
            merge_shard_tracking_refs(repository, remote_name);
        }
    }
}

fn is_non_fast_forward_error(error: &GitAiError) -> bool {
    let GitAiError::GitCliError { stderr, .. } = error else {
        return false;
    };
    stderr.contains("non-fast-forward")
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
    fetch_refspecs: &[&str],
) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("fetch".to_string());
    args.push("--no-tags".to_string());
    args.push("--recurse-submodules=no".to_string());
    args.push("--no-write-fetch-head".to_string());
    args.push("--no-write-commit-graph".to_string());
    args.push("--no-auto-maintenance".to_string());
    args.push(remote_name.to_string());
    for refspec in fetch_refspecs {
        args.push(refspec.to_string());
    }
    args
}

fn build_authorship_push_args(
    global_args: Vec<String>,
    remote_name: &str,
    push_refspecs: &[&str],
) -> Vec<String> {
    let mut args = with_disabled_hooks(global_args);
    args.push("push".to_string());
    args.push("--quiet".to_string());
    args.push("--no-recurse-submodules".to_string());
    args.push("--no-verify".to_string());
    args.push("--no-signed".to_string());
    args.push(remote_name.to_string());
    for refspec in push_refspecs {
        args.push(refspec.to_string());
    }
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
            &["+refs/notes/ai:refs/notes/ai-remote/origin"],
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
        let args = build_authorship_push_args(
            vec!["-C".to_string(), "/tmp/repo".to_string()],
            "origin",
            &[AI_AUTHORSHIP_PUSH_REFSPEC],
        );

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == disabled_hooks)
        );
        assert!(args.contains(&"push".to_string()));
    }

    #[test]
    fn missing_remote_notes_ref_error_is_detected() {
        let err = GitAiError::GitCliError {
            code: Some(128),
            stderr: "fatal: couldn't find remote ref refs/notes/ai".to_string(),
            args: vec!["fetch".to_string(), "origin".to_string()],
        };
        assert!(is_missing_remote_notes_ref_error(&err));
    }

    #[test]
    fn missing_remote_notes_ref_error_ignores_unrelated_git_errors() {
        let err = GitAiError::GitCliError {
            code: Some(128),
            stderr: "fatal: Authentication failed for 'https://github.com/org/repo.git/'"
                .to_string(),
            args: vec!["fetch".to_string(), "origin".to_string()],
        };
        assert!(!is_missing_remote_notes_ref_error(&err));
    }
}
