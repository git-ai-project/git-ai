use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, SemanticEvent,
};
use crate::error::GitAiError;

#[derive(Default)]
pub struct RefAdminAnalyzer;

impl CommandAnalyzer for RefAdminAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        _state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = normalized_args(&cmd.raw_argv);
        let mut events = Vec::new();

        match name {
            "branch" => {
                for change in &cmd.ref_changes {
                    if let Some(branch) = change.reference.strip_prefix("refs/heads/") {
                        if change.old.trim().is_empty() {
                            events.push(SemanticEvent::BranchCreated {
                                name: branch.to_string(),
                                target: change.new.clone(),
                            });
                        } else if change.new.trim().is_empty() {
                            events.push(SemanticEvent::BranchDeleted {
                                name: branch.to_string(),
                                old: change.old.clone(),
                            });
                        } else {
                            events.push(SemanticEvent::RefUpdated {
                                reference: change.reference.clone(),
                                old: change.old.clone(),
                                new: change.new.clone(),
                            });
                        }
                    }
                }
                if args.iter().any(|arg| arg == "-m" || arg == "-M") && args.len() >= 4 {
                    events.push(SemanticEvent::BranchRenamed {
                        old_name: args[2].clone(),
                        new_name: args[3].clone(),
                        target: cmd.post_repo.as_ref().and_then(|repo| repo.head.clone()),
                    });
                }
            }
            "tag" => {
                for change in &cmd.ref_changes {
                    if let Some(tag) = change.reference.strip_prefix("refs/tags/") {
                        if change.old.trim().is_empty() {
                            events.push(SemanticEvent::TagCreated {
                                name: tag.to_string(),
                                target: change.new.clone(),
                            });
                        } else if change.new.trim().is_empty() {
                            events.push(SemanticEvent::TagDeleted {
                                name: tag.to_string(),
                                old: change.old.clone(),
                            });
                        } else {
                            events.push(SemanticEvent::RefUpdated {
                                reference: change.reference.clone(),
                                old: change.old.clone(),
                                new: change.new.clone(),
                            });
                        }
                    }
                }
            }
            "update-ref" => {
                for change in &cmd.ref_changes {
                    events.push(SemanticEvent::RefUpdated {
                        reference: change.reference.clone(),
                        old: change.old.clone(),
                        new: change.new.clone(),
                    });
                }
            }
            "symbolic-ref" => {
                if args.len() >= 3 {
                    events.push(SemanticEvent::SymbolicRefUpdated {
                        reference: args[1].clone(),
                        old_target: None,
                        new_target: Some(args[2].clone()),
                    });
                } else {
                    events.push(SemanticEvent::SymbolicRefUpdated {
                        reference: "HEAD".to_string(),
                        old_target: None,
                        new_target: cmd.post_repo.as_ref().and_then(|repo| repo.branch.clone()),
                    });
                }
            }
            "notes" => events.push(SemanticEvent::NotesUpdated),
            "replace" => events.push(SemanticEvent::ReplaceUpdated),
            "pack-refs" => events.push(SemanticEvent::PackRefsRun),
            "reflog" => {
                if args.iter().any(|arg| arg == "expire") {
                    events.push(SemanticEvent::ReflogExpireRun);
                } else {
                    events.push(SemanticEvent::OpaqueCommand);
                }
            }
            _ => {
                return Err(GitAiError::Generic(format!(
                    "ref_admin analyzer does not support command '{}'",
                    name
                )));
            }
        }

        if events.is_empty() {
            events.push(SemanticEvent::OpaqueCommand);
        }

        Ok(AnalysisResult {
            class: CommandClass::RefMutation,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{AliasResolution, CommandScope, RefChange};

    #[test]
    fn branch_create_emits_branch_created() {
        let analyzer = RefAdminAnalyzer;
        let cmd = NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: "r".to_string(),
            raw_argv: vec![
                "git".to_string(),
                "branch".to_string(),
                "feature".to_string(),
            ],
            primary_command: Some("branch".to_string()),
            alias_resolution: AliasResolution::None,
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            pre_stash_sha: None,
            ref_changes: vec![RefChange {
                reference: "refs/heads/feature".to_string(),
                old: "".to_string(),
                new: "abc".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_mirror: false,
        };
        let out = analyzer
            .analyze(
                &cmd,
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();
        assert!(
            out.events
                .iter()
                .any(|e| matches!(e, SemanticEvent::BranchCreated { .. }))
        );
    }
}
