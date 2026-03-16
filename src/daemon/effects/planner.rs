use crate::daemon::domain::{AppliedCommand, PullStrategy, SemanticEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectIntent {
    SyncAuthorshipNotes { reason: String },
    RunPrePushHook { remote: Option<String> },
    ApplyRewriteLogEvent { reason: String },
    RunCheckpointEffect { checkpoint_id: Option<String> },
    RewriteAgentMetadataPaths,
    RenameWorkingLog,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectPlan {
    pub intents: Vec<EffectIntent>,
}

pub fn plan_effects(applied: &AppliedCommand) -> EffectPlan {
    let mut intents = Vec::new();

    for event in &applied.analysis.events {
        match event {
            SemanticEvent::FetchCompleted { .. } => intents.push(EffectIntent::SyncAuthorshipNotes {
                reason: "fetch".to_string(),
            }),
            SemanticEvent::CloneCompleted { .. } => intents.push(EffectIntent::SyncAuthorshipNotes {
                reason: "clone".to_string(),
            }),
            SemanticEvent::PullCompleted { strategy, .. } => {
                intents.push(EffectIntent::SyncAuthorshipNotes {
                    reason: "pull".to_string(),
                });
                if matches!(strategy, PullStrategy::FastForwardOnly) {
                    intents.push(EffectIntent::RenameWorkingLog);
                }
            }
            SemanticEvent::PushCompleted { remote } => {
                intents.push(EffectIntent::RunPrePushHook {
                    remote: remote.clone(),
                });
            }
            SemanticEvent::CommitCreated { .. }
            | SemanticEvent::CommitAmended { .. }
            | SemanticEvent::Reset { .. }
            | SemanticEvent::RebaseComplete { .. }
            | SemanticEvent::RebaseAbort { .. }
            | SemanticEvent::CherryPickComplete { .. }
            | SemanticEvent::CherryPickAbort { .. }
            | SemanticEvent::MergeSquash { .. } => {
                intents.push(EffectIntent::ApplyRewriteLogEvent {
                    reason: format!("{:?}", event),
                });
            }
            SemanticEvent::StashOperation { .. } => intents.push(EffectIntent::ApplyRewriteLogEvent {
                reason: "stash".to_string(),
            }),
            SemanticEvent::ConfigChanged | SemanticEvent::RemoteConfigChanged => {
                intents.push(EffectIntent::RewriteAgentMetadataPaths);
            }
            _ => {}
        }
    }

    EffectPlan { intents }
}

