use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, SemanticEvent,
};
use crate::error::GitAiError;

#[derive(Default)]
pub struct ReadOnlyAnalyzer;

impl CommandAnalyzer for ReadOnlyAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        _state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd
            .primary_command
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !is_supported_read_only(&name) {
            return Err(GitAiError::Generic(format!(
                "read_only analyzer does not support command '{}'",
                name
            )));
        }

        Ok(AnalysisResult {
            class: CommandClass::ReadOnly,
            events: vec![SemanticEvent::ReadOnlyCommand],
            confidence: Confidence::High,
        })
    }
}

fn is_supported_read_only(command: &str) -> bool {
    matches!(
        command,
        "status"
            | "diff"
            | "log"
            | "show"
            | "rev-parse"
            | "for-each-ref"
            | "cat-file"
            | "blame"
            | "grep"
            | "help"
            | "version"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{AliasResolution, CommandScope};

    #[test]
    fn status_maps_to_read_only() {
        let analyzer = ReadOnlyAnalyzer;
        let cmd = NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: "r".to_string(),
            raw_argv: vec!["git".to_string(), "status".to_string()],
            primary_command: Some("status".to_string()),
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
        let result = analyzer
            .analyze(
                &cmd,
                AnalysisView {
                    refs: &Default::default(),
                },
            )
            .unwrap();
        assert!(matches!(result.class, CommandClass::ReadOnly));
    }
}
