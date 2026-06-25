use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer, command_args, normalized_args};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, SemanticEvent, StashOpKind,
};
use crate::error::GitAiError;

#[derive(Default)]
pub struct WorkspaceAnalyzer;

impl CommandAnalyzer for WorkspaceAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = command_args(cmd);

        let mut events = Vec::new();
        match name {
            "stash" => {
                let stash_args = stash_command_args(cmd);
                events.push(SemanticEvent::StashOperation {
                    kind: infer_stash_kind(&stash_args),
                    head: current_head_for_workspace_command(cmd, state.refs),
                });
            }
            "checkout" => {
                if is_path_checkout(&args) {
                    events.push(SemanticEvent::CheckoutPaths);
                } else if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::RefUpdated {
                        reference: change.reference.clone(),
                        old: change.old.clone(),
                        new: change.new.clone(),
                    });
                }
            }
            "switch" => {
                if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::RefUpdated {
                        reference: change.reference.clone(),
                        old: change.old.clone(),
                        new: change.new.clone(),
                    });
                }
            }
            "restore" => {
                // Only `git restore --source <commit> -- <files>` moves attributed
                // content across a commit boundary. A no-source restore pulls from
                // the index/HEAD and is not attribution-bearing, so it falls through
                // to OpaqueCommand. `restore_source_oid` is resolved by the ref cursor.
                let summary = crate::git::cli_parser::summarize_restore_args(&command_args(cmd));
                if cmd.restore_source_oid.is_some() && !summary.pathspecs.is_empty() {
                    events.push(SemanticEvent::RestorePaths {
                        source_oid: cmd.restore_source_oid.clone(),
                        pathspecs: summary.pathspecs,
                        head: current_head_for_workspace_command(cmd, state.refs),
                    });
                }
            }
            _ => unreachable!("registry should not route '{}' to WorkspaceAnalyzer", name),
        }

        if events.is_empty() {
            events.push(SemanticEvent::OpaqueCommand);
        }

        Ok(AnalysisResult {
            class: CommandClass::WorkspaceMutation,
            events,
            confidence: if cmd.exit_code == 0 {
                Confidence::High
            } else {
                Confidence::Low
            },
        })
    }
}

fn stash_command_args(cmd: &NormalizedCommand) -> Vec<String> {
    let args = normalized_args(&cmd.raw_argv);
    if let Some(index) = args.iter().position(|arg| arg == "stash")
        && let Some(stash_args) = args.get(index + 1..)
    {
        return stash_args.to_vec();
    }
    command_args(cmd)
}

fn infer_stash_kind(args: &[String]) -> StashOpKind {
    match args.first().map(String::as_str).unwrap_or("push") {
        "push" | "save" => StashOpKind::Push,
        "apply" => StashOpKind::Apply,
        "pop" => StashOpKind::Pop,
        "drop" => StashOpKind::Drop,
        "list" => StashOpKind::List,
        "branch" => StashOpKind::Branch,
        "show" => StashOpKind::Show,
        _ => StashOpKind::Unknown,
    }
}

fn is_path_checkout(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--")
        || args
            .iter()
            .any(|arg| arg.starts_with("--pathspec") || arg == "--ours" || arg == "--theirs")
}

fn current_head_for_workspace_command(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<String> {
    current_branch_ref(cmd)
        .and_then(|reference| refs.get(&reference).cloned())
        .or_else(|| refs.get("HEAD").cloned())
        .or_else(|| {
            cmd.ref_changes
                .iter()
                .find(|change| change.reference == "HEAD")
                .map(|change| change.old.clone())
        })
        .filter(|head| !head.trim().is_empty())
}

fn current_branch_ref(_cmd: &NormalizedCommand) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::CommandScope;

    fn command(primary: &str, argv: &[&str]) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: "r".to_string(),
            raw_argv: argv.iter().map(|s| s.to_string()).collect(),
            primary_command: Some(primary.to_string()),
            invoked_command: Some(primary.to_string()),
            invoked_args: argv.iter().skip(2).map(|s| s.to_string()).collect(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            reflog_start_offsets: std::collections::HashMap::new(),
            stash_target_oid: None,
            cherry_pick_source_oids: Vec::new(),
            revert_source_oids: Vec::new(),
            restore_source_oid: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
        }
    }

    #[test]
    fn stash_apply_maps_to_stash_operation() {
        let analyzer = WorkspaceAnalyzer;
        let mut refs = std::collections::HashMap::new();
        refs.insert("HEAD".to_string(), "abc123".to_string());
        let cmd = command("stash", &["git", "stash", "apply", "stash@{0}"]);
        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::StashOperation {
                kind: StashOpKind::Apply,
                head: Some(head),
                ..
            } if head == "abc123"
        )));
    }

    #[test]
    fn restore_with_source_emits_restore_paths() {
        let analyzer = WorkspaceAnalyzer;
        let mut refs = std::collections::HashMap::new();
        refs.insert("HEAD".to_string(), "head_oid".to_string());
        let mut cmd = command(
            "restore",
            &[
                "git",
                "restore",
                "--source",
                "src_ref",
                "--staged",
                "--worktree",
                "--",
                "f.ts",
            ],
        );
        // The ref cursor resolves --source to a full OID before analysis.
        cmd.restore_source_oid = Some("resolved_src_oid".to_string());
        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::RestorePaths { source_oid: Some(src), pathspecs, head: Some(head) }
                if src == "resolved_src_oid" && pathspecs == &["f.ts".to_string()] && head == "head_oid"
        )));
    }

    #[test]
    fn restore_without_source_is_opaque() {
        let analyzer = WorkspaceAnalyzer;
        let mut refs = std::collections::HashMap::new();
        refs.insert("HEAD".to_string(), "head_oid".to_string());
        // No --source => ref cursor leaves restore_source_oid None.
        let cmd = command("restore", &["git", "restore", "--", "f.ts"]);
        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, SemanticEvent::RestorePaths { .. })),
            "no-source restore must not emit RestorePaths"
        );
        assert!(
            result
                .events
                .iter()
                .any(|event| matches!(event, SemanticEvent::OpaqueCommand)),
            "no-source restore should fall through to OpaqueCommand"
        );
    }

    #[test]
    fn restore_staged_and_worktree_flags_parse_pathspecs_identically() {
        let analyzer = WorkspaceAnalyzer;
        let mut refs = std::collections::HashMap::new();
        refs.insert("HEAD".to_string(), "head_oid".to_string());
        for flags in [
            vec!["git", "restore", "--source", "s", "--staged", "--", "f.ts"],
            vec![
                "git",
                "restore",
                "--source",
                "s",
                "--worktree",
                "--",
                "f.ts",
            ],
            vec![
                "git",
                "restore",
                "--source",
                "s",
                "--staged",
                "--worktree",
                "--",
                "f.ts",
            ],
        ] {
            let mut cmd = command("restore", &flags);
            cmd.restore_source_oid = Some("resolved".to_string());
            let result = analyzer
                .analyze(&cmd, AnalysisView { refs: &refs })
                .unwrap();
            assert!(
                result.events.iter().any(|event| matches!(
                    event,
                    SemanticEvent::RestorePaths { pathspecs, .. }
                        if pathspecs == &["f.ts".to_string()]
                )),
                "flags {flags:?} should yield pathspec f.ts"
            );
        }
    }
}
