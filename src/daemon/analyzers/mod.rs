use crate::daemon::domain::{AnalysisResult, NormalizedCommand};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::sync::Arc;

pub mod generic;
pub mod history;
pub mod transport;
pub mod workspace;

#[derive(Debug, Clone)]
pub struct AnalysisView<'a> {
    pub refs: &'a HashMap<String, String>,
}

pub trait CommandAnalyzer: Send + Sync {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError>;
}

#[derive(Clone)]
pub struct AnalyzerRegistry {
    generic: Arc<dyn CommandAnalyzer>,
    by_command: HashMap<String, Arc<dyn CommandAnalyzer>>,
}

impl Default for AnalyzerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzerRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            generic: Arc::new(generic::GenericAnalyzer::default()),
            by_command: HashMap::new(),
        };

        let history: Arc<dyn CommandAnalyzer> = Arc::new(history::HistoryAnalyzer);
        for command in ["commit", "reset", "rebase", "cherry-pick", "merge"] {
            registry.register_command(command, history.clone());
        }

        let workspace: Arc<dyn CommandAnalyzer> = Arc::new(workspace::WorkspaceAnalyzer);
        for command in ["stash", "checkout", "switch"] {
            registry.register_command(command, workspace.clone());
        }

        let transport: Arc<dyn CommandAnalyzer> = Arc::new(transport::TransportAnalyzer);
        for command in ["fetch", "pull", "push", "clone"] {
            registry.register_command(command, transport.clone());
        }

        registry
    }

    pub fn register_command(
        &mut self,
        command: impl Into<String>,
        analyzer: Arc<dyn CommandAnalyzer>,
    ) {
        self.by_command
            .insert(command.into().to_ascii_lowercase(), analyzer);
    }

    pub fn analyze(
        &self,
        cmd: &NormalizedCommand,
        state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        if let Some(command) = cmd.primary_command.as_ref() {
            let key = command.to_ascii_lowercase();
            if let Some(analyzer) = self.by_command.get(&key) {
                return analyzer.analyze(cmd, state);
            }
        }
        self.generic.analyze(cmd, state)
    }
}
