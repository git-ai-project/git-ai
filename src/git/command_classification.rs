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

/// Returns true if the git invocation identified by `command` and optional
/// `subcommand` is guaranteed to never mutate repository state.
///
/// Extends `is_definitely_read_only_command` to handle commands like `stash`
/// and `worktree` whose read-only status depends on the subcommand:
/// - `git stash list` / `git stash show` are read-only
/// - `git stash pop` / `git stash apply` are not
/// - `git worktree list` is read-only
/// - `git worktree add` / `git worktree remove` are not
///
/// IDEs like Zed issue thousands of `stash list` and `worktree list` calls
/// per minute for their git panel UI. These must be identified as read-only
/// so the trace2 pipeline can drop them without processing.
pub fn is_definitely_read_only_invocation(command: &str, subcommand: Option<&str>) -> bool {
    if is_definitely_read_only_command(command) {
        return true;
    }
    match command {
        "stash" => matches!(subcommand, Some("list" | "show")),
        "worktree" => matches!(subcommand, Some("list")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- is_definitely_read_only_invocation tests ---

    #[test]
    fn stash_list_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("stash", Some("list")));
    }

    #[test]
    fn stash_show_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("stash", Some("show")));
    }

    #[test]
    fn stash_mutating_subcommands_are_not_read_only() {
        assert!(!is_definitely_read_only_invocation("stash", Some("pop")));
        assert!(!is_definitely_read_only_invocation("stash", Some("apply")));
        assert!(!is_definitely_read_only_invocation("stash", Some("drop")));
        assert!(!is_definitely_read_only_invocation("stash", Some("branch")));
        assert!(!is_definitely_read_only_invocation("stash", Some("push")));
        assert!(!is_definitely_read_only_invocation("stash", Some("save")));
        // stash with no subcommand defaults to stash push (mutating)
        assert!(!is_definitely_read_only_invocation("stash", None));
    }

    #[test]
    fn worktree_list_is_read_only_invocation() {
        assert!(is_definitely_read_only_invocation("worktree", Some("list")));
    }

    #[test]
    fn worktree_mutating_subcommands_are_not_read_only() {
        assert!(!is_definitely_read_only_invocation("worktree", Some("add")));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("remove")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("move")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("lock")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("unlock")
        ));
        assert!(!is_definitely_read_only_invocation(
            "worktree",
            Some("prune")
        ));
        assert!(!is_definitely_read_only_invocation("worktree", None));
    }

    #[test]
    fn standard_read_only_commands_are_read_only_invocations_regardless_of_subcommand() {
        for cmd in &[
            "status",
            "diff",
            "show",
            "log",
            "cat-file",
            "rev-parse",
            "for-each-ref",
            "blame",
            "grep",
            "ls-files",
            "ls-tree",
        ] {
            assert!(
                is_definitely_read_only_invocation(cmd, None),
                "{cmd} should be read-only with no subcommand"
            );
            assert!(
                is_definitely_read_only_invocation(cmd, Some("anything")),
                "{cmd} should be read-only regardless of subcommand"
            );
        }
    }

    #[test]
    fn mutating_commands_are_not_read_only_invocations() {
        for cmd in &[
            "commit", "push", "pull", "rebase", "merge", "checkout", "reset", "fetch",
        ] {
            assert!(
                !is_definitely_read_only_invocation(cmd, None),
                "{cmd} should not be read-only"
            );
        }
    }
}
