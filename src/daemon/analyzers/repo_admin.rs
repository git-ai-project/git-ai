use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, SemanticEvent,
};
use crate::error::GitAiError;
use std::path::PathBuf;

#[derive(Default)]
pub struct RepoAdminAnalyzer;

impl CommandAnalyzer for RepoAdminAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        _state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = normalized_args(&cmd.raw_argv);
        let mut events = Vec::new();
        match name {
            "init" => events.push(SemanticEvent::RepoInitialized {
                path: cmd.worktree.clone().unwrap_or_else(|| {
                    infer_init_target(&args).unwrap_or_else(|| PathBuf::from("."))
                }),
            }),
            "worktree" => {
                if args.iter().any(|a| a == "add") {
                    if let Some(path) = args.last() {
                        events.push(SemanticEvent::WorktreeAdded {
                            path: PathBuf::from(path),
                        });
                    }
                } else if args.iter().any(|a| a == "remove") {
                    if let Some(path) = args.last() {
                        events.push(SemanticEvent::WorktreeRemoved {
                            path: PathBuf::from(path),
                        });
                    }
                } else {
                    events.push(SemanticEvent::OpaqueCommand);
                }
            }
            "config" => {
                if args.iter().any(|a| a.contains("remote.")) {
                    events.push(SemanticEvent::RemoteConfigChanged);
                } else {
                    events.push(SemanticEvent::ConfigChanged);
                }
            }
            "credential" => events.push(SemanticEvent::ConfigChanged),
            "gc" => events.push(SemanticEvent::GcRun),
            "maintenance" => events.push(SemanticEvent::MaintenanceRun),
            "fsck" | "prune" => events.push(SemanticEvent::OpaqueCommand),
            _ => {
                return Err(GitAiError::Generic(format!(
                    "repo_admin analyzer does not support command '{}'",
                    name
                )));
            }
        }

        Ok(AnalysisResult {
            class: CommandClass::RepoAdmin,
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

fn infer_init_target(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .skip(1)
        .find(|arg| !arg.starts_with('-'))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{AliasResolution, CommandScope};

    #[test]
    fn init_emits_repo_initialized() {
        let analyzer = RepoAdminAnalyzer;
        let cmd = NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: Some(PathBuf::from("/tmp/repo")),
            root_sid: "r".to_string(),
            raw_argv: vec!["git".to_string(), "init".to_string()],
            primary_command: Some("init".to_string()),
            alias_resolution: AliasResolution::None,
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            pre_stash_sha: None,
            ref_changes: Vec::new(),
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
                .any(|e| matches!(e, SemanticEvent::RepoInitialized { .. }))
        );
    }
}
