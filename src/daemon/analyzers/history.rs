use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, ResetKind, SemanticEvent,
};
use crate::error::GitAiError;

#[derive(Default)]
pub struct HistoryAnalyzer;

impl CommandAnalyzer for HistoryAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = normalized_args(&cmd.raw_argv);

        let mut events = Vec::new();
        match name {
            "commit" => {
                if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    if args.iter().any(|arg| arg == "--amend") {
                        events.push(SemanticEvent::CommitAmended { old_head, new_head });
                    } else {
                        events.push(SemanticEvent::CommitCreated {
                            base: non_empty(old_head),
                            new_head,
                        });
                    }
                }
            }
            "reset" => {
                if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    events.push(SemanticEvent::Reset {
                        kind: infer_reset_kind(&args),
                        old_head,
                        new_head,
                    });
                }
            }
            "rebase" => {
                if args.iter().any(|arg| arg == "--abort") {
                    events.push(SemanticEvent::RebaseAbort {
                        head: cmd
                            .post_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                    });
                } else if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    events.push(SemanticEvent::RebaseComplete {
                        old_head,
                        new_head,
                        interactive: args.iter().any(|arg| arg == "-i" || arg == "--interactive"),
                    });
                }
            }
            "cherry-pick" => {
                if args.iter().any(|arg| arg == "--abort") {
                    events.push(SemanticEvent::CherryPickAbort {
                        head: cmd
                            .post_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                    });
                } else if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    events.push(SemanticEvent::CherryPickComplete {
                        original_head: old_head,
                        new_head,
                    });
                }
            }
            "merge" => {
                if args.iter().any(|arg| arg == "--squash") {
                    events.push(SemanticEvent::MergeSquash {
                        base_branch: cmd.pre_repo.as_ref().and_then(|repo| repo.branch.clone()),
                        base_head: cmd
                            .pre_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                        source: args.last().cloned().unwrap_or_default(),
                    });
                } else if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    events.push(SemanticEvent::RefUpdated {
                        reference: "HEAD".to_string(),
                        old: old_head,
                        new: new_head,
                    });
                }
            }
            _ => {
                return Err(GitAiError::Generic(format!(
                    "history analyzer does not support command '{}'",
                    name
                )));
            }
        }

        if events.is_empty() {
            events.push(SemanticEvent::OpaqueCommand);
        }

        Ok(AnalysisResult {
            class: CommandClass::HistoryRewrite,
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

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn non_empty_opt(value: Option<String>) -> Option<String> {
    value.and_then(non_empty)
}

fn head_change(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    if let Some(change) = cmd.ref_changes.first()
        && !change.new.trim().is_empty()
        && change.old != change.new
    {
        return Some((change.old.clone(), change.new.clone()));
    }

    let new_head = non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))?;

    let old_head = non_empty_opt(
        cmd.pre_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .or_else(|| refs.get("HEAD").cloned()),
    )
    .unwrap_or_default();

    if old_head == new_head {
        return None;
    }
    Some((old_head, new_head))
}

fn infer_reset_kind(args: &[String]) -> ResetKind {
    if args.iter().any(|arg| arg == "--soft") {
        return ResetKind::Soft;
    }
    if args.iter().any(|arg| arg == "--mixed") {
        return ResetKind::Mixed;
    }
    if args.iter().any(|arg| arg == "--hard") {
        return ResetKind::Hard;
    }
    if args.iter().any(|arg| arg == "--merge") {
        return ResetKind::Merge;
    }
    if args.iter().any(|arg| arg == "--keep") {
        return ResetKind::Keep;
    }
    ResetKind::Mixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{CommandScope, RefChange};

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
            ref_changes: vec![RefChange {
                reference: "HEAD".to_string(),
                old: "a".to_string(),
                new: "b".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[test]
    fn commit_without_amend_emits_commit_created() {
        let analyzer = HistoryAnalyzer;
        let result = analyzer
            .analyze(
                &command("commit", &["git", "commit", "-m", "x"]),
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();
        assert!(
            result
                .events
                .iter()
                .any(|event| matches!(event, SemanticEvent::CommitCreated { .. }))
        );
    }

    #[test]
    fn reset_emits_reset_kind() {
        let analyzer = HistoryAnalyzer;
        let result = analyzer
            .analyze(
                &command("reset", &["git", "reset", "--hard", "HEAD~1"]),
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::Reset {
                kind: ResetKind::Hard,
                ..
            }
        )));
    }

    #[test]
    fn commit_uses_pre_post_head_when_reflog_delta_is_empty() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("commit", &["git", "commit", "-m", "x"]);
        cmd.ref_changes.clear();
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("old-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });

        let result = analyzer
            .analyze(
                &cmd,
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();

        assert!(
            result.events.iter().any(|event| matches!(
                event,
                SemanticEvent::CommitCreated {
                    new_head,
                    ..
                } if new_head == "new-head"
            )),
            "expected commit-created event from pre/post head fallback, got {:?}",
            result.events
        );
    }
}
