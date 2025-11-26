use crate::commands::upgrade;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::Repository;
use crate::git::sync_authorship::{NotesExistence, fetch_authorship_notes};
use crate::utils::debug_log;

// testing
const AUTHORSHIP_REFSPEC: &str = "refs/notes/ai:refs/notes/ai";

pub fn push_pre_command_hook(parsed_args: &mut ParsedGitInvocation, repository: &Repository) {
    if is_dry_run(&parsed_args.command_args)
        || parsed_args
            .command_args
            .iter()
            .any(|a| a == "-d" || a == "--delete")
        || parsed_args.command_args.iter().any(|a| a == "--mirror")
    {
        return;
    }

    upgrade::maybe_schedule_background_update_check();

    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Detect the remote being pushed to
    let positional_remote = extract_remote_from_push_args(&parsed_args.command_args, &remote_names);

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

    if let Some(remote) = remote {
        // Check if notes exist on remote to determine if we need to force push
        let force_notes = match fetch_authorship_notes(repository, &remote) {
            Ok(NotesExistence::Found) => {
                debug_log(&format!("fetched authorship notes for remote: {}", remote));
                false // Notes exist, no need to force
            }
            Ok(NotesExistence::NotFound) => {
                debug_log(&format!("no authorship notes found on remote: {}", remote));
                true // No notes on remote, need to force push to create ref
            }
            Err(e) => {
                debug_log(&format!(
                    "failed to check authorship notes for remote: {}",
                    e
                ));
                false // On error, don't force (safer default)
            }
        };
        // Try to inject the authorship refspec into the push command
        if let Some(new_args) = inject_authorship_refspec(
            &parsed_args.command_args,
            &remote,
            &remote_names,
            force_notes,
        ) {
            debug_log(&format!(
                "old args: git push {}",
                parsed_args.command_args.join(" ")
            ));
            debug_log(&format!("new args: git push {}", new_args.join(" ")));
            parsed_args.command_args = new_args;
        }
    } else {
        debug_log("no remotes found for authorship push; skipping");
    }
}

/// Injects the authorship refspec into push command arguments.
/// If force_notes is true, the refspec will be prefixed with + to force push.
fn inject_authorship_refspec(
    args: &[String],
    remote: &str,
    known_remotes: &[String],
    force_notes: bool,
) -> Option<Vec<String>> {
    // Build the refspec to inject (with or without force prefix)
    let refspec = if force_notes {
        format!("+{}", AUTHORSHIP_REFSPEC)
    } else {
        AUTHORSHIP_REFSPEC.to_string()
    };

    // Skip conditions: don't inject refspec for these cases
    if is_dry_run(args)
        || args.iter().any(|a| {
            a == "-d" || a == "--delete" || a == "--mirror" || a == "--all" || a == "--tags"
        })
    {
        return None;
    }

    // Check for deletion refspecs like :branch
    if args.iter().any(|a| a.starts_with(':') && a.len() > 1) {
        return None;
    }

    // Check if authorship refspec is already present (with or without force)
    if args
        .iter()
        .any(|a| a == AUTHORSHIP_REFSPEC || a == &format!("+{}", AUTHORSHIP_REFSPEC))
    {
        return None;
    }

    let mut new_args = Vec::new();
    let mut double_dash_index: Option<usize> = None;
    let mut found_remote_explicitly = false;
    let mut found_refspec = false;
    let mut i = 0;

    // First pass: copy args and detect structure
    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            double_dash_index = Some(new_args.len());
            new_args.push(arg.clone());
            i += 1;
            continue;
        }

        // Check if this arg is a remote name (before --)
        if double_dash_index.is_none() && !arg.starts_with('-') && !found_remote_explicitly {
            if known_remotes.iter().any(|r| r == arg) || arg == remote {
                found_remote_explicitly = true;
                new_args.push(arg.clone());
                i += 1;
                continue;
            }
        }

        // Check if this is a refspec (after remote or after --)
        if (found_remote_explicitly || double_dash_index.is_some()) && !arg.starts_with('-') {
            found_refspec = true;
        }

        new_args.push(arg.clone());

        // Handle options that consume values
        if double_dash_index.is_none() && option_consumes_separate_value(arg.as_str()) {
            i += 1;
            if i < args.len() {
                new_args.push(args[i].clone());
            }
        }

        i += 1;
    }

    // Now inject the authorship refspec
    // Special case: if we have -- but no explicit remote, insert remote before --
    if let Some(dash_idx) = double_dash_index {
        if !found_remote_explicitly {
            // git push -- refs/heads/main -> git push origin -- refs/heads/main refs/notes/ai:refs/notes/ai
            new_args.insert(dash_idx, remote.to_string());
        }
        // Always append our refspec at the end when -- is present
        new_args.push(refspec);
    } else if !found_refspec {
        // No explicit refspecs - inject HEAD to push current branch + our notes
        // git push -> git push origin HEAD refs/notes/ai:refs/notes/ai
        // git push origin -> git push origin HEAD refs/notes/ai:refs/notes/ai
        if !found_remote_explicitly {
            new_args.push(remote.to_string());
        }
        new_args.push("HEAD".to_string());
        new_args.push(refspec);
    } else {
        // Has explicit refspecs without --, just append ours at the end
        // git push origin main -> git push origin main refs/notes/ai:refs/notes/ai
        new_args.push(refspec);
    }

    Some(new_args)
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
        // Treat -C<path> or -c<name>=<value> as inline values
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args_vec(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_inject_no_remote_no_refspec() {
        // git push -> git push origin HEAD refs/notes/ai:refs/notes/ai
        let args = args_vec(&[]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&["origin", "HEAD", "refs/notes/ai:refs/notes/ai"]))
        );
    }

    #[test]
    fn test_inject_with_remote_no_refspec() {
        // git push origin -> git push origin HEAD refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&["origin", "HEAD", "refs/notes/ai:refs/notes/ai"]))
        );
    }

    #[test]
    fn test_inject_with_remote_and_single_refspec() {
        // git push origin main -> git push origin main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&["origin", "main", "refs/notes/ai:refs/notes/ai"]))
        );
    }

    #[test]
    fn test_inject_with_refspec_mapping() {
        // git push origin main:develop -> git push origin main:develop refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "main:develop"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "main:develop",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_force_refspec() {
        // git push origin +main -> git push origin +main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "+main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "+main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_head() {
        // git push origin HEAD -> git push origin HEAD refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "HEAD"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&["origin", "HEAD", "refs/notes/ai:refs/notes/ai"]))
        );
    }

    #[test]
    fn test_inject_with_double_dash() {
        // git push origin -- refs/heads/main -> git push origin -- refs/heads/main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "--", "refs/heads/main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "--",
                "refs/heads/main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_double_dash_no_explicit_remote() {
        // git push -- refs/heads/main -> git push origin -- refs/heads/main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["--", "refs/heads/main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "--",
                "refs/heads/main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_force_with_lease() {
        // git push --force-with-lease origin main -> git push --force-with-lease origin main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["--force-with-lease", "origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "--force-with-lease",
                "origin",
                "main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_set_upstream() {
        // git push -u origin main -> git push -u origin main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["-u", "origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "-u",
                "origin",
                "main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_push_option() {
        // git push -o option origin main -> git push -o option origin main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["-o", "ci.skip", "origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "-o",
                "ci.skip",
                "origin",
                "main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_full_refspec() {
        // git push origin refs/heads/main:refs/heads/main -> ... refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "refs/heads/main:refs/heads/main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "refs/heads/main:refs/heads/main",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    // Tests for cases that should NOT inject

    #[test]
    fn test_skip_dry_run() {
        // git push --dry-run origin main -> should return None
        let args = args_vec(&["--dry-run", "origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_delete_flag() {
        // git push origin --delete branch -> should return None
        let args = args_vec(&["origin", "--delete", "branch"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_delete_short_flag() {
        // git push -d origin branch -> should return None
        let args = args_vec(&["-d", "origin", "branch"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_mirror() {
        // git push --mirror origin -> should return None
        let args = args_vec(&["--mirror", "origin"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_all() {
        // git push origin --all -> should return None
        let args = args_vec(&["origin", "--all"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_tags() {
        // git push origin --tags -> should return None
        let args = args_vec(&["origin", "--tags"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_skip_deletion_refspec() {
        // git push origin :branch -> should return None (deleting remote branch)
        let args = args_vec(&["origin", ":branch"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_multiple_refspecs() {
        // git push origin main develop -> git push origin main develop refs/notes/ai:refs/notes/ai
        let args = args_vec(&["origin", "main", "develop"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "main",
                "develop",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_with_repo_flag() {
        // git push --repo=origin main -> git push --repo=origin main refs/notes/ai:refs/notes/ai
        let args = args_vec(&["--repo=origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        // Note: --repo=origin is treated as a flag, not the remote name in positional sense
        // The remote is still determined by the caller, so we just append the refspec
        assert!(result.is_some());
        let result_args = result.unwrap();
        assert!(result_args.contains(&"refs/notes/ai:refs/notes/ai".to_string()));
    }

    // This test ensures we don't try to modify any manual notes pushes
    #[test]
    fn test_skip_if_authorship_refspec_already_present() {
        // git push origin HEAD refs/notes/ai:refs/notes/ai -> should return None (already has it)
        let args = args_vec(&["origin", "HEAD", "refs/notes/ai:refs/notes/ai"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_push_all_notes_refs() {
        // git push origin refs/notes/*:refs/notes/* -> should inject our refspec
        // This ensures we explicitly push our authorship notes even with glob patterns
        let args = args_vec(&["origin", "refs/notes/*:refs/notes/*"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], false);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "refs/notes/*:refs/notes/*",
                "refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    // Tests for force push behavior (when notes don't exist on remote yet)

    #[test]
    fn test_inject_force_no_remote_no_refspec() {
        // git push -> git push origin HEAD +refs/notes/ai:refs/notes/ai (force on first push)
        let args = args_vec(&[]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], true);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "HEAD",
                "+refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_force_with_remote_and_refspec() {
        // git push origin main -> git push origin main +refs/notes/ai:refs/notes/ai (force on first push)
        let args = args_vec(&["origin", "main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], true);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "main",
                "+refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_inject_force_with_double_dash() {
        // git push origin -- refs/heads/main -> ... +refs/notes/ai:refs/notes/ai (force on first push)
        let args = args_vec(&["origin", "--", "refs/heads/main"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], true);
        assert_eq!(
            result,
            Some(args_vec(&[
                "origin",
                "--",
                "refs/heads/main",
                "+refs/notes/ai:refs/notes/ai"
            ]))
        );
    }

    #[test]
    fn test_skip_if_force_authorship_refspec_already_present() {
        // git push origin HEAD +refs/notes/ai:refs/notes/ai -> should return None (already has it)
        let args = args_vec(&["origin", "HEAD", "+refs/notes/ai:refs/notes/ai"]);
        let result = inject_authorship_refspec(&args, "origin", &["origin".to_string()], true);
        assert_eq!(result, None);
    }
}
