use crate::git::cli_parser::ParsedGitInvocation;

/// Returns true if the given git subcommand is guaranteed to never mutate
/// repository state (refs, objects, config, worktree). Used to skip expensive
/// trace2 ingestion work and suppress trace2 emission for read-only commands.
pub fn is_definitely_read_only_command(command: &str) -> bool {
    matches!(
        command,
        "blame"
            | "cat-file"
            | "check-attr"
            | "check-ignore"
            | "check-mailmap"
            | "count-objects"
            | "describe"
            | "diff"
            | "diff-files"
            | "diff-index"
            | "diff-tree"
            | "for-each-ref"
            | "grep"
            | "help"
            | "log"
            | "ls-files"
            | "ls-tree"
            | "merge-base"
            | "name-rev"
            | "rev-list"
            | "rev-parse"
            | "shortlog"
            | "show"
            | "status"
            | "var"
            | "verify-commit"
            | "verify-tag"
            | "version"
    )
}

pub fn is_read_only_invocation(parsed: &ParsedGitInvocation) -> bool {
    if parsed.is_help || parsed.command.is_none() {
        return true;
    }

    if parsed
        .command
        .as_deref()
        .is_some_and(is_definitely_read_only_command)
    {
        return true;
    }

    match parsed.command.as_deref() {
        Some("branch") => is_read_only_branch_invocation(parsed),
        Some("stash") => is_read_only_stash_invocation(parsed),
        Some("tag") => is_read_only_tag_invocation(parsed),
        Some("remote") => is_read_only_remote_invocation(parsed),
        Some("config") => is_read_only_config_invocation(parsed),
        Some("worktree") => is_read_only_worktree_invocation(parsed),
        Some("submodule") => is_read_only_submodule_invocation(parsed),
        _ => false,
    }
}

fn command_args_contain_any(command_args: &[String], flags: &[&str]) -> bool {
    command_args.iter().any(|arg| {
        flags
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    })
}

fn is_read_only_branch_invocation(parsed: &ParsedGitInvocation) -> bool {
    let mutating_flags = [
        "-c",
        "-C",
        "-d",
        "-D",
        "-f",
        "-m",
        "-M",
        "-u",
        "--copy",
        "--create-reflog",
        "--delete",
        "--delete-force",
        "--edit-description",
        "--force",
        "--move",
        "--no-track",
        "--recurse-submodules",
        "--set-upstream-to",
        "--track",
        "--unset-upstream",
    ];
    if command_args_contain_any(&parsed.command_args, &mutating_flags) {
        return false;
    }

    let read_only_listing_flags = [
        "--all",
        "--contains",
        "--format",
        "--ignore-case",
        "--list",
        "--merged",
        "--no-color",
        "--no-column",
        "--no-contains",
        "--no-merged",
        "--points-at",
        "--remotes",
        "--show-current",
        "--sort",
        "--verbose",
        "-a",
        "-l",
        "-r",
        "-v",
    ];

    command_args_contain_any(&parsed.command_args, &read_only_listing_flags)
        || parsed.pos_command(0).is_none()
}

fn is_read_only_stash_invocation(parsed: &ParsedGitInvocation) -> bool {
    matches!(
        parsed.command_args.first().map(String::as_str),
        Some("list" | "show")
    )
}

fn is_read_only_tag_invocation(parsed: &ParsedGitInvocation) -> bool {
    let mutating_flags = [
        "-a",
        "-d",
        "-e",
        "-f",
        "-F",
        "-m",
        "-s",
        "-u",
        "--annotate",
        "--cleanup",
        "--create-reflog",
        "--delete",
        "--edit",
        "--file",
        "--force",
        "--local-user",
        "--message",
        "--no-sign",
        "--sign",
        "--trailer",
    ];
    if command_args_contain_any(&parsed.command_args, &mutating_flags) {
        return false;
    }

    let read_only_listing_flags = [
        "--column",
        "--contains",
        "--format",
        "--ignore-case",
        "--list",
        "--merged",
        "--no-column",
        "--no-contains",
        "--no-merged",
        "--points-at",
        "--sort",
        "-l",
    ];

    command_args_contain_any(&parsed.command_args, &read_only_listing_flags)
        || parsed.pos_command(0).is_none()
}

fn is_read_only_remote_invocation(parsed: &ParsedGitInvocation) -> bool {
    let mutating_subcommands = [
        "add",
        "rename",
        "remove",
        "rm",
        "set-head",
        "set-branches",
        "set-url",
        "prune",
        "update",
    ];

    match parsed.pos_command(0).as_deref() {
        None => true,
        Some(subcommand) if mutating_subcommands.contains(&subcommand) => false,
        Some("show" | "get-url") => true,
        Some(_) => false,
    }
}

fn is_read_only_config_invocation(parsed: &ParsedGitInvocation) -> bool {
    let mutating_flags = [
        "--add",
        "--replace-all",
        "--unset",
        "--unset-all",
        "--rename-section",
        "--remove-section",
        "--edit",
    ];
    if command_args_contain_any(&parsed.command_args, &mutating_flags) {
        return false;
    }

    let read_only_flags = [
        "--blob",
        "--default",
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--includes",
        "--list",
        "--name-only",
        "--no-includes",
        "--null",
        "--show-origin",
        "--show-scope",
        "--type",
        "-l",
        "-z",
    ];

    command_args_contain_any(&parsed.command_args, &read_only_flags)
}

fn is_read_only_worktree_invocation(parsed: &ParsedGitInvocation) -> bool {
    matches!(
        parsed.command_args.first().map(String::as_str),
        Some("list")
    )
}

fn is_read_only_submodule_invocation(parsed: &ParsedGitInvocation) -> bool {
    matches!(
        parsed.command_args.first().map(String::as_str),
        Some("status" | "summary")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::cli_parser::parse_git_cli_args;

    #[test]
    fn read_only_commands_detected() {
        assert!(is_definitely_read_only_command("check-ignore"));
        assert!(is_definitely_read_only_command("rev-parse"));
        assert!(is_definitely_read_only_command("status"));
        assert!(is_definitely_read_only_command("diff"));
        assert!(is_definitely_read_only_command("log"));
        assert!(is_definitely_read_only_command("cat-file"));
        assert!(is_definitely_read_only_command("ls-files"));
    }

    #[test]
    fn mutating_commands_not_read_only() {
        assert!(!is_definitely_read_only_command("commit"));
        assert!(!is_definitely_read_only_command("push"));
        assert!(!is_definitely_read_only_command("pull"));
        assert!(!is_definitely_read_only_command("rebase"));
        assert!(!is_definitely_read_only_command("merge"));
        assert!(!is_definitely_read_only_command("checkout"));
        assert!(!is_definitely_read_only_command("stash"));
        assert!(!is_definitely_read_only_command("reset"));
        assert!(!is_definitely_read_only_command("fetch"));
    }

    #[test]
    fn unknown_commands_not_read_only() {
        assert!(!is_definitely_read_only_command("my-custom-alias"));
        assert!(!is_definitely_read_only_command(""));
    }

    #[test]
    fn read_only_invocation_detects_branch_show_current() {
        let parsed = parse_git_cli_args(&["branch".to_string(), "--show-current".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_branch_listing_without_positionals() {
        let parsed = parse_git_cli_args(&["branch".to_string(), "-v".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_branch_creation() {
        let parsed = parse_git_cli_args(&["branch".to_string(), "feature".to_string()]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_tag_listing() {
        let parsed = parse_git_cli_args(&["tag".to_string(), "--list".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_tag_creation() {
        let parsed = parse_git_cli_args(&["tag".to_string(), "v1.2.3".to_string()]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_stash_list() {
        let parsed = parse_git_cli_args(&["stash".to_string(), "list".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_top_level_version() {
        let parsed = parse_git_cli_args(&["--version".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_commit_help() {
        let parsed = parse_git_cli_args(&["commit".to_string(), "--help".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_remote_listing() {
        let parsed = parse_git_cli_args(&["remote".to_string(), "-v".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_remote_add() {
        let parsed = parse_git_cli_args(&[
            "remote".to_string(),
            "add".to_string(),
            "origin".to_string(),
            "https://example.com/repo".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_config_list() {
        let parsed = parse_git_cli_args(&[
            "config".to_string(),
            "--list".to_string(),
            "--show-origin".to_string(),
        ]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_config_set() {
        let parsed = parse_git_cli_args(&[
            "config".to_string(),
            "--add".to_string(),
            "demo.key".to_string(),
            "value".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }
}
