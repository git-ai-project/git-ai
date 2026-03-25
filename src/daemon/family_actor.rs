use crate::daemon::analyzers::AnalyzerRegistry;
use crate::daemon::domain::{
    AppliedCommand, ApplyAck, FamilyKey, FamilyState, FamilyStatus, NormalizedCommand,
};
use crate::daemon::reducer;
use crate::error::GitAiError;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

pub enum FamilyMsg {
    Apply(
        Box<NormalizedCommand>,
        oneshot::Sender<Result<AppliedCommand, GitAiError>>,
    ),
    ApplyCheckpoint(oneshot::Sender<Result<ApplyAck, GitAiError>>),
    Status(oneshot::Sender<Result<FamilyStatus, GitAiError>>),
    Shutdown,
}

#[derive(Clone)]
pub struct FamilyActorHandle {
    pub family_key: FamilyKey,
    tx: mpsc::Sender<FamilyMsg>,
}

impl FamilyActorHandle {
    pub async fn apply(&self, cmd: NormalizedCommand) -> Result<AppliedCommand, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Apply(Box::new(cmd), tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor apply send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor apply receive failed".to_string()))?
    }

    pub async fn apply_checkpoint(&self) -> Result<ApplyAck, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::ApplyCheckpoint(tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor checkpoint send failed".to_string()))?;
        rx.await.map_err(|_| {
            GitAiError::Generic("family actor checkpoint receive failed".to_string())
        })?
    }

    pub async fn status(&self) -> Result<FamilyStatus, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Status(tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor status send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor status receive failed".to_string()))?
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        self.tx
            .send(FamilyMsg::Shutdown)
            .await
            .map_err(|_| GitAiError::Generic("family actor shutdown send failed".to_string()))
    }
}

pub fn spawn_family_actor(family_key: FamilyKey) -> FamilyActorHandle {
    let (tx, mut rx) = mpsc::channel::<FamilyMsg>(1024);
    let handle = FamilyActorHandle {
        family_key: family_key.clone(),
        tx,
    };

    tokio::spawn(async move {
        let analyzers = AnalyzerRegistry::new();
        let mut state = FamilyState {
            family_key: family_key.clone(),
            refs: HashMap::new(),
            worktrees: HashMap::new(),
            last_error: None,
            applied_seq: 0,
        };

        while let Some(msg) = rx.recv().await {
            match msg {
                FamilyMsg::Apply(cmd, respond_to) => {
                    let result = reducer::reduce_family_command(&mut state, *cmd, &analyzers)
                        .map(|(applied, _)| applied);
                    let _ = respond_to.send(result);
                }
                FamilyMsg::ApplyCheckpoint(respond_to) => {
                    reducer::reduce_checkpoint(&mut state);
                    let _ = respond_to.send(Ok(ApplyAck {
                        seq: state.applied_seq,
                        applied: true,
                    }));
                }
                FamilyMsg::Status(respond_to) => {
                    let _ = respond_to.send(Ok(FamilyStatus {
                        family_key: state.family_key.clone(),
                        applied_seq: state.applied_seq,
                        last_error: state.last_error.clone(),
                    }));
                }
                FamilyMsg::Shutdown => break,
            }
        }
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{CommandScope, Confidence, NormalizedCommand};
    use std::path::PathBuf;

    fn sample_normalized_cmd(family_key: &str, seq: u128) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new(family_key)),
            family_key: Some(FamilyKey::new(family_key)),
            worktree: Some(PathBuf::from("/tmp/repo")),
            root_sid: format!("sid-{}", seq),
            raw_argv: vec!["git".to_string(), "status".to_string()],
            primary_command: Some("status".to_string()),
            invoked_command: Some("status".to_string()),
            invoked_args: Vec::new(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: seq,
            finished_at_ns: seq + 1,
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

    #[tokio::test]
    async fn actor_applies_commands() {
        let actor = spawn_family_actor(FamilyKey::new("family-1"));
        let ack1 = actor
            .apply(sample_normalized_cmd("family-1", 10))
            .await
            .unwrap();
        let ack2 = actor
            .apply(sample_normalized_cmd("family-1", 20))
            .await
            .unwrap();
        assert_eq!(ack1.seq, 1);
        assert_eq!(ack2.seq, 2);
        actor.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn actor_status_reports_applied_seq() {
        let actor = spawn_family_actor(FamilyKey::new("family-2"));
        actor
            .apply(sample_normalized_cmd("family-2", 1))
            .await
            .unwrap();
        let status = actor.status().await.unwrap();
        assert_eq!(status.applied_seq, 1);
        actor.shutdown().await.unwrap();
    }
}
