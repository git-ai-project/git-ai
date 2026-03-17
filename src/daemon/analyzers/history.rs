use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, ResetKind, SemanticEvent,
};
use crate::error::GitAiError;
use std::path::Path;

#[derive(Default)]
pub struct HistoryAnalyzer;

impl CommandAnalyzer for HistoryAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = command_args(cmd);

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
                } else if reset_mode_flag_present(&args)
                    && let Some(head) = best_effort_head(cmd, state.refs)
                {
                    // Trace ingestion can lag behind fast command bursts and miss a
                    // precise old->new boundary. Preserve explicit reset intent with
                    // a best-effort head value so rewrite side effects still run.
                    events.push(SemanticEvent::Reset {
                        kind: infer_reset_kind(&args),
                        old_head: head.clone(),
                        new_head: head,
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

fn command_args(cmd: &NormalizedCommand) -> Vec<String> {
    if !cmd.invoked_args.is_empty() {
        return cmd.invoked_args.clone();
    }
    normalized_args(&cmd.raw_argv)
}

fn normalized_args(argv: &[String]) -> Vec<String> {
    let start = argv
        .first()
        .and_then(|arg| Path::new(arg).file_name().and_then(|name| name.to_str()))
        .is_some_and(|name| name == "git" || name == "git.exe");
    if start {
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
    let preferred_span = cmd
        .ref_changes
        .iter()
        .filter(|change| {
            change.reference == "HEAD"
                && !change.new.trim().is_empty()
                && change.old.trim() != change.new.trim()
        })
        .collect::<Vec<_>>();
    if let Some((old_head, new_head)) = change_span(&preferred_span) {
        return Some((old_head, new_head));
    }

    let branch_span = cmd
        .ref_changes
        .iter()
        .filter(|change| {
            change.reference.starts_with("refs/heads/")
                && !change.new.trim().is_empty()
                && change.old.trim() != change.new.trim()
        })
        .collect::<Vec<_>>();
    if let Some((old_head, new_head)) = change_span(&branch_span) {
        return Some((old_head, new_head));
    }

    let any_span = cmd
        .ref_changes
        .iter()
        .filter(|change| !change.new.trim().is_empty() && change.old.trim() != change.new.trim())
        .collect::<Vec<_>>();
    if let Some((old_head, new_head)) = change_span(&any_span) {
        return Some((old_head, new_head));
    }

    let new_head = non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))?;

    if let Some(orig_head) = cmd
        .ref_changes
        .iter()
        .find(|change| change.reference == "ORIG_HEAD")
        .and_then(|change| non_empty(change.new.clone()))
        && orig_head != new_head
    {
        return Some((orig_head, new_head));
    }

    let old_head = non_empty_opt(
        cmd.pre_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .or_else(|| {
                cmd.pre_repo
                    .as_ref()
                    .and_then(|repo| repo.branch.as_deref())
                    .and_then(|branch| refs.get(&format!("refs/heads/{}", branch)).cloned())
            })
            .or_else(|| {
                cmd.post_repo
                    .as_ref()
                    .and_then(|repo| repo.branch.as_deref())
                    .and_then(|branch| refs.get(&format!("refs/heads/{}", branch)).cloned())
            })
            .or_else(|| refs.get("HEAD").cloned()),
    )
    .or_else(|| {
        refs.iter().find_map(|(reference, oid)| {
            if reference.starts_with("refs/heads/") {
                non_empty(oid.clone())
            } else {
                None
            }
        })
    })
    .unwrap_or_default();

    if old_head == new_head {
        return None;
    }
    Some((old_head, new_head))
}

fn change_span(changes: &[&crate::daemon::domain::RefChange]) -> Option<(String, String)> {
    let first = changes.first()?;
    let last = changes.last()?;
    let old_head = first.old.trim();
    let new_head = last.new.trim();
    if old_head.is_empty() || new_head.is_empty() || old_head == new_head {
        return None;
    }
    Some((old_head.to_string(), new_head.to_string()))
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

fn reset_mode_flag_present(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--soft" | "--mixed" | "--hard" | "--merge" | "--keep"
        )
    })
}

fn best_effort_head(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<String> {
    non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()))
        .or_else(|| non_empty_opt(cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone())))
        .or_else(|| non_empty_opt(refs.get("HEAD").cloned()))
        .or_else(|| {
            cmd.post_repo
                .as_ref()
                .and_then(|repo| repo.branch.as_deref())
                .and_then(|branch| refs.get(&format!("refs/heads/{}", branch)).cloned())
                .and_then(non_empty)
        })
        .or_else(|| {
            refs.iter().find_map(|(reference, oid)| {
                if reference.starts_with("refs/heads/") {
                    non_empty(oid.clone())
                } else {
                    None
                }
            })
        })
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
            invoked_command: Some(primary.to_string()),
            invoked_args: argv.iter().skip(2).map(|s| s.to_string()).collect(),
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
            cherry_pick_head: None,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
            cherry_pick_head: None,
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

    #[test]
    fn cherry_pick_uses_full_head_ref_change_span() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command(
            "cherry-pick",
            &["git", "cherry-pick", "source-1", "source-2", "source-3"],
        );
        cmd.ref_changes = vec![
            RefChange {
                reference: "HEAD".to_string(),
                old: "a".to_string(),
                new: "b".to_string(),
            },
            RefChange {
                reference: "HEAD".to_string(),
                old: "b".to_string(),
                new: "c".to_string(),
            },
            RefChange {
                reference: "HEAD".to_string(),
                old: "c".to_string(),
                new: "d".to_string(),
            },
        ];

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
                SemanticEvent::CherryPickComplete {
                    original_head,
                    new_head
                } if original_head == "a" && new_head == "d"
            )),
            "expected cherry-pick span event, got {:?}",
            result.events
        );
    }
}
