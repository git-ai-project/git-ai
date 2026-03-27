use crate::git::cli_parser::ParsedGitInvocation;

/// Returns true if the given git subcommand is guaranteed to never mutate
/// repository state (refs, objects, config, worktree). Used to skip expensive
/// trace2 ingestion work and suppress trace2 emission for read-only commands.
///
/// This list covers both porcelain and plumbing commands that IDEs (VS Code,
/// JetBrains), git clients (GitLens, Graphite CLI), and other tools call at
/// high frequency. Only commands that are unconditionally read-only regardless
/// of arguments belong here; commands with mixed read/write modes (symbolic-ref,
/// reflog, notes, etc.) are handled by `is_read_only_invocation` instead.
pub fn is_definitely_read_only_command(command: &str) -> bool {
    matches!(
        command,
        // Porcelain read-only
        "blame"
            | "cherry"
            | "describe"
            | "diff"
            | "grep"
            | "help"
            | "log"
            | "shortlog"
            | "show"
            | "show-branch"
            | "status"
            | "version"
            | "whatchanged"
            // Plumbing — object/ref inspection
            | "cat-file"
            | "diff-files"
            | "diff-index"
            | "diff-tree"
            | "for-each-ref"
            | "ls-files"
            | "ls-remote"
            | "ls-tree"
            | "merge-base"
            | "name-rev"
            | "rev-list"
            | "rev-parse"
            | "show-ref"
            | "var"
            | "verify-commit"
            | "verify-pack"
            | "verify-tag"
            // Plumbing — query/validation helpers
            | "check-attr"
            | "check-ignore"
            | "check-mailmap"
            | "check-ref-format"
            | "column"
            | "count-objects"
            | "fmt-merge-msg"
            | "fsck"
            | "get-tar-commit-id"
            | "patch-id"
            | "stripspace"
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
        Some("symbolic-ref") => is_read_only_symbolic_ref_invocation(parsed),
        Some("reflog") => is_read_only_reflog_invocation(parsed),
        Some("notes") => is_read_only_notes_invocation(parsed),
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

    // Flags that *trigger* list mode — their presence alone means read-only.
    let list_mode_triggers = [
        "--all",
        "--contains",
        "--format",
        "--list",
        "--merged",
        "--no-contains",
        "--no-merged",
        "--points-at",
        "--remotes",
        "--show-current",
        "--sort",
        "-a",
        "-l",
        "-r",
    ];

    // Flags that only *modify* list output (e.g. -v, --no-color) but do NOT
    // trigger list mode on their own. `git branch -v feature` creates a branch
    // named "feature" — -v only means "verbose" in list mode, it does not
    // activate list mode.
    command_args_contain_any(&parsed.command_args, &list_mode_triggers)
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

    // Only explicit query actions are safe to fast-path. Other config flags like
    // --type or --show-origin are modifiers and can still participate in writes.
    let read_only_actions = [
        "--blob",
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--list",
        "-l",
    ];

    command_args_contain_any(&parsed.command_args, &read_only_actions)
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

/// `git symbolic-ref HEAD` reads; `git symbolic-ref HEAD refs/heads/main` writes.
/// -d/--delete and -m (reason) with a target are also mutating.
fn is_read_only_symbolic_ref_invocation(parsed: &ParsedGitInvocation) -> bool {
    if command_args_contain_any(&parsed.command_args, &["-d", "--delete"]) {
        return false;
    }
    // Read mode: exactly one positional (the ref name to read).
    // Write mode: two positionals (ref name + target).
    parsed.pos_command(1).is_none()
}

/// Only `git reflog expire` and `git reflog delete` are mutating.
/// Everything else — bare `git reflog`, `git reflog show`, `git reflog HEAD`,
/// `git reflog --all`, `git reflog exists` — is read-only (bare reflog and
/// unrecognized first args are interpreted by git as `reflog show`).
fn is_read_only_reflog_invocation(parsed: &ParsedGitInvocation) -> bool {
    !matches!(
        parsed.command_args.first().map(String::as_str),
        Some("expire" | "delete")
    )
}

/// `git notes list`, `git notes show` are read-only.
/// `git notes add/append/copy/edit/merge/prune/remove` are mutating.
/// Bare `git notes` defaults to `list`.
fn is_read_only_notes_invocation(parsed: &ParsedGitInvocation) -> bool {
    matches!(
        parsed.command_args.first().map(String::as_str),
        None | Some("list" | "show" | "get-ref")
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
        // High-volume plumbing used by IDEs and git clients
        assert!(is_definitely_read_only_command("ls-remote"));
        assert!(is_definitely_read_only_command("show-ref"));
        assert!(is_definitely_read_only_command("cherry"));
        assert!(is_definitely_read_only_command("show-branch"));
        assert!(is_definitely_read_only_command("for-each-ref"));
        assert!(is_definitely_read_only_command("verify-pack"));
        assert!(is_definitely_read_only_command("check-ref-format"));
        assert!(is_definitely_read_only_command("fsck"));
        assert!(is_definitely_read_only_command("whatchanged"));
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
    fn read_only_invocation_rejects_branch_create_with_verbose_flag() {
        // `git branch -v feature` creates a branch; -v is a list-output modifier,
        // not a list-mode trigger.
        let parsed = parse_git_cli_args(&[
            "branch".to_string(),
            "-v".to_string(),
            "feature".to_string(),
        ]);
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

    #[test]
    fn read_only_invocation_rejects_config_write_with_type_modifier() {
        let parsed = parse_git_cli_args(&[
            "config".to_string(),
            "--type=bool".to_string(),
            "demo.enabled".to_string(),
            "true".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_accepts_config_get_with_modifiers() {
        let parsed = parse_git_cli_args(&[
            "config".to_string(),
            "--show-origin".to_string(),
            "--type=bool".to_string(),
            "--get".to_string(),
            "demo.enabled".to_string(),
        ]);
        assert!(is_read_only_invocation(&parsed));
    }

    // symbolic-ref classifier
    #[test]
    fn read_only_invocation_detects_symbolic_ref_read() {
        let parsed = parse_git_cli_args(&["symbolic-ref".to_string(), "HEAD".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_symbolic_ref_read_with_short() {
        let parsed = parse_git_cli_args(&[
            "symbolic-ref".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_symbolic_ref_write() {
        let parsed = parse_git_cli_args(&[
            "symbolic-ref".to_string(),
            "HEAD".to_string(),
            "refs/heads/main".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_symbolic_ref_delete() {
        let parsed = parse_git_cli_args(&[
            "symbolic-ref".to_string(),
            "--delete".to_string(),
            "HEAD".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    // reflog classifier
    #[test]
    fn read_only_invocation_detects_bare_reflog() {
        let parsed = parse_git_cli_args(&["reflog".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_reflog_show() {
        let parsed =
            parse_git_cli_args(&["reflog".to_string(), "show".to_string(), "HEAD".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_reflog_implicit_show_with_ref() {
        // `git reflog HEAD` is `git reflog show HEAD`
        let parsed = parse_git_cli_args(&["reflog".to_string(), "HEAD".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_reflog_implicit_show_with_flags() {
        // `git reflog --all` is `git reflog show --all`
        let parsed = parse_git_cli_args(&["reflog".to_string(), "--all".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_reflog_exists() {
        let parsed = parse_git_cli_args(&[
            "reflog".to_string(),
            "exists".to_string(),
            "HEAD".to_string(),
        ]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_reflog_expire() {
        let parsed = parse_git_cli_args(&[
            "reflog".to_string(),
            "expire".to_string(),
            "--all".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_reflog_delete() {
        let parsed = parse_git_cli_args(&[
            "reflog".to_string(),
            "delete".to_string(),
            "HEAD@{0}".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    // notes classifier
    #[test]
    fn read_only_invocation_detects_bare_notes() {
        let parsed = parse_git_cli_args(&["notes".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_notes_list() {
        let parsed = parse_git_cli_args(&["notes".to_string(), "list".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_notes_show() {
        let parsed =
            parse_git_cli_args(&["notes".to_string(), "show".to_string(), "HEAD".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_notes_add() {
        let parsed = parse_git_cli_args(&[
            "notes".to_string(),
            "add".to_string(),
            "-m".to_string(),
            "note".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_rejects_notes_remove() {
        let parsed = parse_git_cli_args(&[
            "notes".to_string(),
            "remove".to_string(),
            "HEAD".to_string(),
        ]);
        assert!(!is_read_only_invocation(&parsed));
    }

    // ls-remote and show-ref as invocations
    #[test]
    fn read_only_invocation_detects_ls_remote() {
        let parsed = parse_git_cli_args(&[
            "ls-remote".to_string(),
            "--heads".to_string(),
            "origin".to_string(),
        ]);
        assert!(is_read_only_invocation(&parsed));
    }

    #[test]
    fn read_only_invocation_detects_show_ref() {
        let parsed = parse_git_cli_args(&["show-ref".to_string(), "--heads".to_string()]);
        assert!(is_read_only_invocation(&parsed));
    }
}
