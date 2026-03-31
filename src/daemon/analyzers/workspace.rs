use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer, command_args, normalized_args};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, SemanticEvent, StashOpKind,
};
use crate::error::GitAiError;
use crate::git::cli_parser::stash_target_spec;

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
                    stash_ref: stash_target_spec(&stash_args).map(ToString::to_string),
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
        .or_else(|| cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
        .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
        .filter(|head| !head.trim().is_empty())
}

fn current_branch_ref(cmd: &NormalizedCommand) -> Option<String> {
    let branch = cmd
        .pre_repo
        .as_ref()
        .and_then(|repo| repo.branch.clone())
        .or_else(|| cmd.post_repo.as_ref().and_then(|repo| repo.branch.clone()))?;
    let branch = branch.trim();
    if branch.is_empty() {
        return None;
    }
    if branch.starts_with("refs/") {
        Some(branch.to_string())
    } else {
        Some(format!("refs/heads/{}", branch))
    }
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
            pre_repo: None,
            post_repo: None,
            inflight_rebase_original_head: None,
            merge_squash_source_head: None,
            carryover_snapshot_id: None,
            stash_target_oid: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_invocation_id: None,
        }
    }

    #[test]
    fn stash_apply_maps_to_stash_operation() {
        let analyzer = WorkspaceAnalyzer;
        let mut refs = std::collections::HashMap::new();
        refs.insert("refs/heads/main".to_string(), "abc123".to_string());
        let mut cmd = command("stash", &["git", "stash", "apply", "stash@{0}"]);
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("abc123".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
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
}
