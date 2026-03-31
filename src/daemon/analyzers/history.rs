use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer, command_args};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, ResetKind, SemanticEvent,
};
use crate::error::GitAiError;
use crate::git::cli_parser::{explicit_rebase_branch_arg, parse_git_cli_args};
use crate::git::repo_state::{
    is_valid_git_oid, resolve_reflog_old_oid_for_ref_new_oid_in_worktree,
    resolve_worktree_head_reflog_old_oid_for_new_head,
};
#[cfg(test)]
use std::fs;

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
                let amend = args.iter().any(|arg| arg == "--amend");
                let post_head =
                    non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()));
                if let Some((mut old_head, new_head)) = head_change(cmd, state.refs) {
                    if amend
                        && (!is_valid_git_oid(&old_head) || is_zero_oid(&old_head))
                        && let Some(pre_head) =
                            non_empty_opt(cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
                        && is_valid_git_oid(&pre_head)
                        && !is_zero_oid(&pre_head)
                        && pre_head != new_head
                    {
                        old_head = pre_head;
                    }
                    if amend {
                        events.push(SemanticEvent::CommitAmended { old_head, new_head });
                    } else {
                        events.push(SemanticEvent::CommitCreated {
                            base: sanitize_base(Some(old_head), &new_head),
                            new_head,
                        });
                    }
                } else if cmd.exit_code == 0
                    && let Some(new_head) = post_head
                {
                    if amend {
                        let old_head = commit_base_hint(cmd, state.refs, &new_head);
                        if let Some(old_head) = old_head {
                            events.push(SemanticEvent::CommitAmended { old_head, new_head });
                        } else {
                            events.push(SemanticEvent::CommitCreated {
                                base: None,
                                new_head,
                            });
                        }
                    } else {
                        let base = commit_base_hint(cmd, state.refs, &new_head);
                        events.push(SemanticEvent::CommitCreated { base, new_head });
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
                } else if let Some((old_head, new_head)) = rebase_change(cmd, state.refs) {
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
                    let source_ref = merge_source_ref(&args).ok_or_else(|| {
                        GitAiError::Generic("merge --squash missing source ref".to_string())
                    })?;
                    events.push(SemanticEvent::MergeSquash {
                        base_branch: cmd.pre_repo.as_ref().and_then(|repo| repo.branch.clone()),
                        base_head: cmd
                            .pre_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                        source_ref,
                        source_head: cmd.merge_squash_source_head.clone().unwrap_or_default(),
                    });
                } else if let Some((old_head, new_head)) = head_change(cmd, state.refs) {
                    events.push(SemanticEvent::RefUpdated {
                        reference: "HEAD".to_string(),
                        old: old_head,
                        new: new_head,
                    });
                }
            }
            "update-ref" => {
                if let Some((ref_name, old_oid, new_oid)) =
                    parse_update_ref_heads(cmd, &args, state.refs)
                {
                    events.push(SemanticEvent::RefUpdated {
                        reference: ref_name,
                        old: old_oid,
                        new: new_oid,
                    });
                }
            }
            _ => unreachable!("registry should not route '{}' to HistoryAnalyzer", name),
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

fn merge_source_ref(args: &[String]) -> Option<String> {
    let mut invocation = vec!["merge".to_string()];
    invocation.extend(args.iter().cloned());
    parse_git_cli_args(&invocation).pos_command(0)
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

fn is_zero_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c == '0')
}

fn sanitize_base(base: Option<String>, new_head: &str) -> Option<String> {
    base.filter(|candidate| candidate != new_head && !is_zero_oid(candidate))
}

fn head_change(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    if let Some(branch_ref) = branch_ref_hint(cmd) {
        let branch_specific_span = cmd
            .ref_changes
            .iter()
            .filter(|change| {
                change.reference == branch_ref
                    && !change.new.trim().is_empty()
                    && change.old.trim() != change.new.trim()
            })
            .collect::<Vec<_>>();
        if let Some((old_head, new_head)) = change_span(&branch_specific_span) {
            return Some((old_head, new_head));
        }
    }

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

    if let Some(old_head) = old_head_from_worktree_head_reflog(cmd, &new_head) {
        return Some((old_head, new_head));
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
            }),
    );
    let old_head = old_head?;

    if old_head == new_head {
        if let Some(alternate_old_head) = old_head_from_refs(cmd, refs)
            && alternate_old_head != new_head
        {
            return Some((alternate_old_head, new_head));
        }
        return None;
    }
    Some((old_head, new_head))
}

fn branch_ref_hint(cmd: &NormalizedCommand) -> Option<String> {
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

fn old_head_from_branch_ref_changes(cmd: &NormalizedCommand) -> Option<String> {
    let branch_ref = branch_ref_hint(cmd)?;
    cmd.ref_changes
        .iter()
        .find(|change| change.reference == branch_ref)
        .and_then(|change| non_empty(change.old.clone()))
        .filter(|old| !is_zero_oid(old))
}

fn old_head_from_refs(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<String> {
    non_empty_opt(
        cmd.pre_repo
            .as_ref()
            .and_then(|repo| repo.branch.as_deref())
            .and_then(|branch| refs.get(&format!("refs/heads/{}", branch)).cloned())
            .or_else(|| {
                cmd.post_repo
                    .as_ref()
                    .and_then(|repo| repo.branch.as_deref())
                    .and_then(|branch| refs.get(&format!("refs/heads/{}", branch)).cloned())
            }),
    )
}

fn commit_base_hint(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
    new_head: &str,
) -> Option<String> {
    sanitize_base(
        old_head_from_branch_ref_changes(cmd)
            .or_else(|| old_head_from_worktree_head_reflog(cmd, new_head))
            .or_else(|| non_empty_opt(cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone())))
            .or_else(|| old_head_from_refs(cmd, refs)),
        new_head,
    )
}

fn old_head_from_worktree_head_reflog(cmd: &NormalizedCommand, new_head: &str) -> Option<String> {
    let worktree = cmd.worktree.as_deref()?;
    resolve_worktree_head_reflog_old_oid_for_new_head(worktree, new_head)
        .ok()
        .flatten()
}

fn rebase_change(
    cmd: &NormalizedCommand,
    refs: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    if let Some((old_head, new_head)) = explicit_rebase_branch_change(cmd) {
        return Some((old_head, new_head));
    }

    if let Some((old_head, new_head)) = inferred_rebase_branch_change(cmd) {
        return Some((old_head, new_head));
    }

    let from_changes = head_change(cmd, refs);
    let new_head = from_changes
        .as_ref()
        .map(|(_, new_head)| new_head.clone())
        .or_else(|| non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone())))?;

    if let Some((old_head, new_head_from_changes)) = from_changes
        && old_head != new_head_from_changes
    {
        return Some((old_head, new_head_from_changes));
    }

    non_empty_opt(cmd.pre_repo.as_ref().and_then(|repo| repo.head.clone()))
        .filter(|old_head| old_head != &new_head)
        .map(|old_head| (old_head, new_head))
}

fn inferred_rebase_branch_change(cmd: &NormalizedCommand) -> Option<(String, String)> {
    let mut candidates = cmd
        .ref_changes
        .iter()
        .filter(|change| {
            change.reference.starts_with("refs/heads/")
                && !change.old.trim().is_empty()
                && !change.new.trim().is_empty()
                && change.old.trim() != change.new.trim()
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let post_head = non_empty_opt(cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()));
    if let Some(post_head) = post_head
        && let Some(change) = candidates
            .iter()
            .find(|change| change.new.trim() == post_head)
    {
        return Some((change.old.trim().to_string(), change.new.trim().to_string()));
    }

    if candidates.len() == 1 {
        let change = candidates.pop()?;
        return Some((change.old.trim().to_string(), change.new.trim().to_string()));
    }

    None
}

fn explicit_rebase_branch_change(cmd: &NormalizedCommand) -> Option<(String, String)> {
    let args = command_args(cmd);
    let branch = explicit_rebase_branch_arg(&args)?;
    let branch_ref = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{}", branch)
    };
    cmd.ref_changes
        .iter()
        .find(|change| {
            change.reference == branch_ref
                && !change.old.trim().is_empty()
                && !change.new.trim().is_empty()
                && change.old.trim() != change.new.trim()
        })
        .map(|change| (change.old.trim().to_string(), change.new.trim().to_string()))
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

/// Parse `update-ref <ref> <new_oid> [<old_oid>]` arguments, returning
/// (ref_name, old_oid, new_oid) for branch refs only.  Falls back to
/// `state.refs` for the old OID when the command provides only two positionals,
/// then to the reflog of the target ref.
fn parse_update_ref_heads(
    cmd: &NormalizedCommand,
    args: &[String],
    refs: &std::collections::HashMap<String, String>,
) -> Option<(String, String, String)> {
    let mut positionals = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "update-ref" => {
                i += 1;
                continue;
            }
            "--stdin" | "--batch-updates" | "-d" | "--delete" => return None,
            "-m" | "--message" => {
                i += 2;
                continue;
            }
            "--create-reflog" | "--no-deref" => {
                i += 1;
                continue;
            }
            _ if arg.starts_with("--message=") => {
                i += 1;
                continue;
            }
            _ if arg.starts_with('-') => return None,
            _ => {
                positionals.push(arg.clone());
                i += 1;
            }
        }
    }

    let (ref_name, new_oid, old_oid_arg) = match positionals.as_slice() {
        [ref_name, new_oid] => (ref_name.clone(), new_oid.clone(), None),
        [ref_name, new_oid, old_oid] => (ref_name.clone(), new_oid.clone(), Some(old_oid.clone())),
        _ => return None,
    };

    if !ref_name.starts_with("refs/heads/") {
        return None;
    }

    let old_oid = old_oid_arg
        .filter(|oid| !oid.is_empty() && !is_zero_oid(oid))
        .or_else(|| refs.get(&ref_name).cloned())
        .or_else(|| {
            // The command has already executed; read the old value from the
            // reflog entry that recorded this update-ref.
            let worktree = cmd.worktree.as_deref()?;
            resolve_reflog_old_oid_for_ref_new_oid_in_worktree(worktree, &ref_name, &new_oid)
                .or_else(|| {
                    resolve_worktree_head_reflog_old_oid_for_new_head(worktree, &new_oid)
                        .ok()
                        .flatten()
                })
        })
        .filter(|oid| !oid.is_empty() && !is_zero_oid(oid))?;

    if old_oid == new_oid {
        return None;
    }

    Some((ref_name, old_oid, new_oid))
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
    use tempfile::tempdir;

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
            ref_changes: vec![RefChange {
                reference: "HEAD".to_string(),
                old: "a".to_string(),
                new: "b".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_invocation_id: None,
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
    fn amend_prefers_pre_head_over_zero_old_reflog_change() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("commit", &["git", "commit", "--amend", "-m", "x"]);
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.ref_changes = vec![
            RefChange {
                reference: "refs/heads/main".to_string(),
                old: "0000000000000000000000000000000000000000".to_string(),
                new: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            },
            RefChange {
                reference: "refs/heads/main".to_string(),
                old: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                new: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
            RefChange {
                reference: "HEAD".to_string(),
                old: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                new: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
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

        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::CommitAmended { old_head, new_head }
                if old_head == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    && new_head == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        )));
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
                    base,
                    new_head,
                } if base.as_deref() == Some("old-head") && new_head == "new-head"
            )),
            "expected commit-created event from pre/post head fallback, got {:?}",
            result.events
        );
    }

    #[test]
    fn commit_fallback_prefers_pre_head_over_family_refs() {
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
        let refs = std::collections::HashMap::from([(
            "refs/heads/main".to_string(),
            "wrong-family-head".to_string(),
        )]);

        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();

        assert!(
            result.events.iter().any(|event| matches!(
                event,
                SemanticEvent::CommitCreated {
                    base,
                    new_head
                } if base.as_deref() == Some("old-head") && new_head == "new-head"
            )),
            "expected commit-created event to prefer pre-head over family refs, got {:?}",
            result.events
        );
    }

    #[test]
    fn commit_emits_created_when_only_post_head_is_available() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("commit", &["git", "commit", "-m", "x"]);
        cmd.ref_changes.clear();
        cmd.pre_repo = None;
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        let refs = std::collections::HashMap::from([(
            "refs/heads/main".to_string(),
            "old-head".to_string(),
        )]);

        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                SemanticEvent::CommitCreated {
                    base,
                    new_head
                } if base.as_deref() == Some("old-head") && new_head == "new-head"
            )),
            "expected commit-created event from post-head fallback, got {:?}",
            result.events
        );
    }

    #[test]
    fn commit_falls_back_to_head_reflog_when_pre_and_post_are_contaminated() {
        let analyzer = HistoryAnalyzer;
        let dir = tempdir().expect("tempdir");
        let worktree = dir.path();
        let git_dir = worktree.join(".git");
        fs::create_dir_all(git_dir.join("logs")).expect("create logs");
        fs::write(
            git_dir.join("logs").join("HEAD"),
            concat!(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ",
                "Test User <test@example.com> 0 +0000\tcommit: first\n",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ",
                "cccccccccccccccccccccccccccccccccccccccc ",
                "Test User <test@example.com> 0 +0000\tcommit: squash\n"
            ),
        )
        .expect("write HEAD reflog");

        let mut cmd = command("commit", &["git", "commit", "-m", "x"]);
        cmd.ref_changes.clear();
        cmd.worktree = Some(worktree.to_path_buf());
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("cccccccccccccccccccccccccccccccccccccccc".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("cccccccccccccccccccccccccccccccccccccccc".to_string()),
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
                SemanticEvent::CommitCreated { base, new_head }
                    if base.as_deref() == Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                        && new_head == "cccccccccccccccccccccccccccccccccccccccc"
            )),
            "expected commit-created event from HEAD reflog fallback, got {:?}",
            result.events
        );
    }

    #[test]
    fn commit_prefers_post_head_when_family_ref_changes_are_contaminated() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("commit", &["git", "-C", "/repo-b", "commit", "-m", "x"]);
        cmd.ref_changes = vec![
            RefChange {
                reference: "refs/heads/branch-a".to_string(),
                old: "0000000000000000000000000000000000000000".to_string(),
                new: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            },
            RefChange {
                reference: "refs/heads/branch-b".to_string(),
                old: "0000000000000000000000000000000000000000".to_string(),
                new: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
            RefChange {
                reference: "HEAD".to_string(),
                old: "0000000000000000000000000000000000000000".to_string(),
                new: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            },
        ];
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            branch: Some("branch-b".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            branch: Some("branch-b".to_string()),
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
                } if new_head == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            )),
            "expected commit-created event to use branch-b post head, got {:?}",
            result.events
        );
    }

    #[test]
    fn head_change_prefers_branch_hint_over_head_change() {
        let mut cmd = command("commit", &["git", "commit", "-m", "x"]);
        cmd.ref_changes = vec![
            RefChange {
                reference: "HEAD".to_string(),
                old: "old-head".to_string(),
                new: "wrong-head".to_string(),
            },
            RefChange {
                reference: "refs/heads/main".to_string(),
                old: "old-main".to_string(),
                new: "new-main".to_string(),
            },
        ];
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("old-main".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-main".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });

        let change = head_change(&cmd, &Default::default());
        assert_eq!(
            change,
            Some(("old-main".to_string(), "new-main".to_string())),
            "expected branch-specific change to win over generic HEAD change"
        );
    }

    #[test]
    fn rebase_continue_prefers_branch_ref_change_over_head_span() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("rebase", &["git", "rebase", "--continue"]);
        cmd.ref_changes = vec![
            RefChange {
                reference: "refs/heads/feature".to_string(),
                old: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                new: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
            RefChange {
                reference: "HEAD".to_string(),
                old: "cccccccccccccccccccccccccccccccccccccccc".to_string(),
                new: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
        ];
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            branch: Some("feature".to_string()),
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
                SemanticEvent::RebaseComplete {
                    old_head,
                    new_head,
                    ..
                } if old_head == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    && new_head == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            )),
            "expected rebase-complete to use branch ref span, got {:?}",
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

    #[test]
    fn cherry_pick_prefers_ref_state_when_pre_head_matches_post_head() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("cherry-pick", &["git", "cherry-pick", "--continue"]);
        cmd.ref_changes.clear();
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("new-head".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        let refs = std::collections::HashMap::from([
            ("HEAD".to_string(), "old-head".to_string()),
            ("refs/heads/main".to_string(), "old-head".to_string()),
        ]);

        let result = analyzer
            .analyze(&cmd, AnalysisView { refs: &refs })
            .unwrap();
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                SemanticEvent::CherryPickComplete {
                    original_head,
                    new_head
                } if original_head == "old-head" && new_head == "new-head"
            )),
            "expected cherry-pick complete event from ref-state fallback, got {:?}",
            result.events
        );
    }

    #[test]
    fn merge_squash_emits_resolved_source_ref_and_head() {
        let analyzer = HistoryAnalyzer;
        let mut cmd = command("merge", &["git", "merge", "--squash", "feature"]);
        cmd.pre_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });
        cmd.merge_squash_source_head = Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());

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
                SemanticEvent::MergeSquash {
                    base_branch,
                    base_head,
                    source_ref,
                    source_head,
                } if base_branch.as_deref() == Some("main")
                    && base_head == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    && source_ref == "feature"
                    && source_head == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )),
            "expected merge-squash event with resolved source head, got {:?}",
            result.events
        );
    }
}
