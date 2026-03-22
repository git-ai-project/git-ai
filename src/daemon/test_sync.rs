use crate::daemon::CheckpointRunRequest;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::repository::config_get_str_for_path_no_git_exec;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const TEST_SYNC_SESSION_CONFIG_KEY: &str = "git-ai.testSyncSession";

pub fn tracks_primary_command_for_test_sync(
    primary_command: Option<&str>,
    invoked_args: &[String],
) -> bool {
    let Some(primary_command) = primary_command else {
        return false;
    };

    match primary_command {
        "branch" => !invoked_args.iter().any(|arg| arg == "--show-current"),
        "checkout" | "cherry-pick" | "clone" | "commit" | "fetch" | "init" | "merge" | "pull"
        | "push" | "rebase" | "reset" | "revert" | "switch" | "tag" | "update-ref" | "worktree" => {
            true
        }
        "remote" => invoked_args.first().is_some_and(|subcommand| {
            matches!(
                subcommand.as_str(),
                "add" | "remove" | "rm" | "rename" | "set-head" | "set-branches" | "set-url"
            )
        }),
        "stash" => !invoked_args
            .first()
            .is_some_and(|subcommand| matches!(subcommand.as_str(), "list" | "show")),
        _ => false,
    }
}

pub fn tracks_parsed_git_invocation_for_test_sync(invocation: &ParsedGitInvocation) -> bool {
    tracks_primary_command_for_test_sync(invocation.command.as_deref(), &invocation.command_args)
}

pub fn tracked_parsed_git_invocation_for_test_sync(
    argv: &[String],
    cwd: &Path,
) -> ParsedGitInvocation {
    let parsed = parse_git_cli_args(argv);
    if tracks_parsed_git_invocation_for_test_sync(&parsed) {
        return parsed;
    }

    let repo_lookup = resolve_repo_lookup_for_git_invocation(&parsed, cwd);
    resolve_alias_invocation_no_git_exec(&parsed, &repo_lookup).unwrap_or(parsed)
}

pub fn tracks_checkpoint_request_for_test_sync(request: &CheckpointRunRequest) -> bool {
    !request.is_pre_commit()
}

pub fn test_sync_session_from_invocation(invocation: &ParsedGitInvocation) -> Option<String> {
    let mut idx = 0usize;
    while idx < invocation.global_args.len() {
        let token = &invocation.global_args[idx];

        if token == "-c" {
            let Some(config_arg) = invocation.global_args.get(idx + 1) else {
                break;
            };
            if let Some(value) = test_sync_session_from_config_arg(config_arg) {
                return Some(value);
            }
            idx += 2;
            continue;
        }

        if let Some(config_arg) = token.strip_prefix("-c")
            && let Some(value) = test_sync_session_from_config_arg(config_arg)
        {
            return Some(value);
        }

        idx += 1;
    }

    None
}

fn test_sync_session_from_config_arg(config_arg: &str) -> Option<String> {
    let (key, value) = config_arg.split_once('=')?;
    (key == TEST_SYNC_SESSION_CONFIG_KEY).then(|| value.to_string())
}

fn resolve_repo_lookup_for_git_invocation(parsed: &ParsedGitInvocation, cwd: &Path) -> PathBuf {
    let base = resolve_command_base_dir_from_cwd(&parsed.global_args, cwd);
    if base.is_dir() {
        base
    } else {
        base.parent().map(PathBuf::from).unwrap_or(base)
    }
}

fn resolve_command_base_dir_from_cwd(global_args: &[String], cwd: &Path) -> PathBuf {
    let mut base = cwd.to_path_buf();
    let mut idx = 0usize;

    while idx < global_args.len() {
        let token = &global_args[idx];

        if token == "-C" {
            let Some(path_arg) = global_args.get(idx + 1) else {
                break;
            };
            let next_base = PathBuf::from(path_arg);
            base = if next_base.is_absolute() {
                next_base
            } else {
                base.join(next_base)
            };
            idx += 2;
            continue;
        }

        if token != "-C" && token.starts_with("-C") {
            let next_base = PathBuf::from(&token[2..]);
            base = if next_base.is_absolute() {
                next_base
            } else {
                base.join(next_base)
            };
            idx += 1;
            continue;
        }

        idx += 1;
    }

    base
}

fn resolve_alias_invocation_no_git_exec(
    parsed_args: &ParsedGitInvocation,
    repo_lookup: &Path,
) -> Option<ParsedGitInvocation> {
    let mut current = parsed_args.clone();
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        let command = match current.command.as_deref() {
            Some(command) => command,
            None => return Some(current),
        };

        if !seen.insert(command.to_string()) {
            return None;
        }

        let key = format!("alias.{}", command);
        let alias_value = match config_get_str_for_path_no_git_exec(repo_lookup, &key) {
            Ok(Some(value)) => value,
            _ => return Some(current),
        };

        let alias_tokens = parse_alias_tokens(&alias_value)?;

        let mut expanded_args = Vec::new();
        expanded_args.extend(current.global_args.iter().cloned());
        expanded_args.extend(alias_tokens);
        expanded_args.extend(current.command_args.iter().cloned());
        current = parse_git_cli_args(&expanded_args);
    }
}

fn parse_alias_tokens(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim_start();
    if trimmed.starts_with('!') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' => escaped = true,
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("create tempdir");
        let status = Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init should succeed");
        temp
    }

    fn git_config(repo: &Path, key: &str, value: &str) {
        let status = Command::new("git")
            .args(["config", key, value])
            .current_dir(repo)
            .status()
            .expect("run git config");
        assert!(status.success(), "git config should succeed");
    }

    #[test]
    fn tracked_invocation_resolves_commit_alias_without_git_exec() {
        let repo = init_repo();
        git_config(repo.path(), "alias.ci", "commit -v");

        let argv = vec!["ci".to_string(), "-m".to_string(), "msg".to_string()];
        let parsed = tracked_parsed_git_invocation_for_test_sync(&argv, repo.path());

        assert_eq!(parsed.command.as_deref(), Some("commit"));
        assert_eq!(
            parsed.command_args,
            vec!["-v".to_string(), "-m".to_string(), "msg".to_string()]
        );
        assert!(tracks_parsed_git_invocation_for_test_sync(&parsed));
    }

    #[test]
    fn tracked_invocation_keeps_branch_show_current_alias_untracked() {
        let repo = init_repo();
        git_config(repo.path(), "alias.sc", "branch --show-current");

        let argv = vec!["sc".to_string()];
        let parsed = tracked_parsed_git_invocation_for_test_sync(&argv, repo.path());

        assert_eq!(parsed.command.as_deref(), Some("branch"));
        assert_eq!(parsed.command_args, vec!["--show-current".to_string()]);
        assert!(!tracks_parsed_git_invocation_for_test_sync(&parsed));
    }

    #[test]
    fn tracked_invocation_does_not_create_ai_dir_for_untracked_commands() {
        let repo = init_repo();
        let ai_dir = repo.path().join(".git").join("ai");
        assert!(
            !ai_dir.exists(),
            "sanity check: .git/ai should not exist yet"
        );

        let argv = vec!["add".to_string(), "file.txt".to_string()];
        let parsed = tracked_parsed_git_invocation_for_test_sync(&argv, repo.path());

        assert_eq!(parsed.command.as_deref(), Some("add"));
        assert!(
            !ai_dir.exists(),
            "tracking helper should not create .git/ai while checking aliases"
        );
    }
}
