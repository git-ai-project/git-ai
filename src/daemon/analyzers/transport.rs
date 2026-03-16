use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, PullStrategy, SemanticEvent,
};
use crate::error::GitAiError;
use std::path::PathBuf;

#[derive(Default)]
pub struct TransportAnalyzer;

impl CommandAnalyzer for TransportAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        _state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = normalized_args(&cmd.raw_argv);

        let mut events = Vec::new();
        match name {
            "fetch" => events.push(SemanticEvent::FetchCompleted {
                remote: args.get(1).cloned().filter(|arg| !arg.starts_with('-')),
            }),
            "pull" => events.push(SemanticEvent::PullCompleted {
                remote: args.get(1).cloned().filter(|arg| !arg.starts_with('-')),
                strategy: infer_pull_strategy(&args),
            }),
            "push" => events.push(SemanticEvent::PushCompleted {
                remote: args.get(1).cloned().filter(|arg| !arg.starts_with('-')),
            }),
            "clone" => events.push(SemanticEvent::CloneCompleted {
                target: infer_clone_target(&args)
                    .or_else(|| cmd.worktree.clone())
                    .unwrap_or_else(|| PathBuf::from(".")),
            }),
            "ls-remote" => events.push(SemanticEvent::LsRemoteCompleted),
            _ => {
                return Err(GitAiError::Generic(format!(
                    "transport analyzer does not support command '{}'",
                    name
                )));
            }
        }

        Ok(AnalysisResult {
            class: CommandClass::Transport,
            events,
            confidence: if cmd.exit_code == 0 {
                Confidence::High
            } else {
                Confidence::Low
            },
        })
    }
}

fn normalized_args(argv: &[String]) -> Vec<String> {
    if argv.first().map(|a| a == "git").unwrap_or(false) {
        argv[1..].to_vec()
    } else {
        argv.to_vec()
    }
}

fn infer_pull_strategy(args: &[String]) -> PullStrategy {
    if args.iter().any(|arg| arg == "--ff-only") {
        return PullStrategy::FastForwardOnly;
    }
    if args
        .iter()
        .any(|arg| arg == "--rebase=merges" || arg == "--rebase-merges")
    {
        return PullStrategy::RebaseMerges;
    }
    if args.iter().any(|arg| arg == "--rebase") {
        return PullStrategy::Rebase;
    }
    PullStrategy::Merge
}

fn infer_clone_target(args: &[String]) -> Option<PathBuf> {
    if args.is_empty() {
        return None;
    }
    let mut filtered = Vec::new();
    let mut skip_next = false;
    for (idx, arg) in args.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "-C" || arg == "--origin" || arg == "--template" {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        filtered.push(arg.clone());
    }
    filtered.last().map(PathBuf::from)
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
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[test]
    fn pull_with_rebase_maps_strategy() {
        let analyzer = TransportAnalyzer;
        let result = analyzer
            .analyze(
                &command("pull", &["git", "pull", "--rebase"]),
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::PullCompleted {
                strategy: PullStrategy::Rebase,
                ..
            }
        )));
    }
}
