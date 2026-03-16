use crate::daemon::analyzers::AnalyzerRegistry;
use crate::daemon::domain::{
    ApplyAck, CheckpointObserved, EnvOverrideSet, FamilyKey, FamilySnapshot, FamilyState,
    FamilyStatus, NormalizedCommand, ReconcileSnapshot,
};
use crate::daemon::effects::{
    EffectIntent, EffectRunnerMode, FamilyEffectRunner, NoopEffectExecutor, plan_effects,
};
use crate::daemon::reducer;
use crate::error::GitAiError;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

pub enum FamilyMsg {
    Apply(
        NormalizedCommand,
        oneshot::Sender<Result<ApplyAck, GitAiError>>,
    ),
    ApplyCheckpoint(
        CheckpointObserved,
        oneshot::Sender<Result<ApplyAck, GitAiError>>,
    ),
    ApplyEnvOverride(
        EnvOverrideSet,
        oneshot::Sender<Result<ApplyAck, GitAiError>>,
    ),
    Reconcile(
        ReconcileSnapshot,
        oneshot::Sender<Result<ApplyAck, GitAiError>>,
    ),
    Status(oneshot::Sender<Result<FamilyStatus, GitAiError>>),
    Snapshot(oneshot::Sender<Result<FamilySnapshot, GitAiError>>),
    Barrier(u64, oneshot::Sender<Result<(), GitAiError>>),
    Shutdown,
}

#[derive(Clone)]
pub struct FamilyActorHandle {
    pub family_key: FamilyKey,
    tx: mpsc::Sender<FamilyMsg>,
}

impl FamilyActorHandle {
    pub async fn apply(&self, cmd: NormalizedCommand) -> Result<ApplyAck, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Apply(cmd, tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor apply send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor apply receive failed".to_string()))?
    }

    pub async fn apply_checkpoint(
        &self,
        checkpoint: CheckpointObserved,
    ) -> Result<ApplyAck, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::ApplyCheckpoint(checkpoint, tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor checkpoint send failed".to_string()))?;
        rx.await.map_err(|_| {
            GitAiError::Generic("family actor checkpoint receive failed".to_string())
        })?
    }

    pub async fn apply_env_override(&self, env: EnvOverrideSet) -> Result<ApplyAck, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::ApplyEnvOverride(env, tx))
            .await
            .map_err(|_| {
                GitAiError::Generic("family actor env_override send failed".to_string())
            })?;
        rx.await.map_err(|_| {
            GitAiError::Generic("family actor env_override receive failed".to_string())
        })?
    }

    pub async fn reconcile(&self, snapshot: ReconcileSnapshot) -> Result<ApplyAck, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Reconcile(snapshot, tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor reconcile send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor reconcile receive failed".to_string()))?
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

    pub async fn snapshot(&self) -> Result<FamilySnapshot, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Snapshot(tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor snapshot send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor snapshot receive failed".to_string()))?
    }

    pub async fn barrier(&self, seq: u64) -> Result<(), GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(FamilyMsg::Barrier(seq, tx))
            .await
            .map_err(|_| GitAiError::Generic("family actor barrier send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("family actor barrier receive failed".to_string()))?
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        self.tx
            .send(FamilyMsg::Shutdown)
            .await
            .map_err(|_| GitAiError::Generic("family actor shutdown send failed".to_string()))
    }
}

pub fn spawn_family_actor(family_key: FamilyKey) -> FamilyActorHandle {
    spawn_family_actor_with_mode(family_key, EffectRunnerMode::Shadow)
}

pub fn spawn_family_actor_with_mode(
    family_key: FamilyKey,
    effect_mode: EffectRunnerMode,
) -> FamilyActorHandle {
    let (tx, mut rx) = mpsc::channel::<FamilyMsg>(1024);
    let handle = FamilyActorHandle {
        family_key: family_key.clone(),
        tx,
    };

    tokio::spawn(async move {
        let analyzers = AnalyzerRegistry::new();
        let effect_runner = FamilyEffectRunner::spawn(effect_mode, Arc::new(NoopEffectExecutor));
        let mut state = FamilyState {
            family_key: family_key.clone(),
            refs: HashMap::new(),
            worktrees: HashMap::new(),
            recent_commands: VecDeque::new(),
            checkpoints: HashMap::new(),
            unresolved_transcripts: BTreeSet::new(),
            active_cherry_pick: HashMap::new(),
            env_overrides: HashMap::new(),
            last_error: None,
            last_reconcile_ns: None,
            applied_seq: 0,
        };
        let mut waiters: Vec<(u64, oneshot::Sender<Result<(), GitAiError>>)> = Vec::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                FamilyMsg::Apply(cmd, respond_to) => {
                    let result = reducer::reduce_family_command(&mut state, cmd, &analyzers).map(
                        |(applied, _)| {
                            let plan = plan_effects(&applied);
                            for intent in plan.intents {
                                if let Err(err) = effect_runner.try_enqueue(intent) {
                                    state.last_error = Some(err.to_string());
                                }
                            }
                            ApplyAck {
                                seq: applied.seq,
                                applied: true,
                            }
                        },
                    );
                    let seq = result
                        .as_ref()
                        .map(|ack| ack.seq)
                        .unwrap_or(state.applied_seq);
                    let _ = respond_to.send(result);
                    satisfy_barriers(seq, &mut waiters);
                }
                FamilyMsg::ApplyCheckpoint(checkpoint, respond_to) => {
                    let checkpoint_id = checkpoint.id.clone();
                    reducer::reduce_checkpoint(&mut state, checkpoint);
                    if let Err(err) = effect_runner.try_enqueue(EffectIntent::RunCheckpointEffect {
                        checkpoint_id: Some(checkpoint_id),
                    }) {
                        state.last_error = Some(err.to_string());
                    }
                    let _ = respond_to.send(Ok(ApplyAck {
                        seq: state.applied_seq,
                        applied: true,
                    }));
                    satisfy_barriers(state.applied_seq, &mut waiters);
                }
                FamilyMsg::ApplyEnvOverride(env, respond_to) => {
                    reducer::reduce_env_override(&mut state, env);
                    let _ = respond_to.send(Ok(ApplyAck {
                        seq: state.applied_seq,
                        applied: true,
                    }));
                    satisfy_barriers(state.applied_seq, &mut waiters);
                }
                FamilyMsg::Reconcile(snapshot, respond_to) => {
                    reducer::reduce_reconcile(&mut state, snapshot);
                    let _ = respond_to.send(Ok(ApplyAck {
                        seq: state.applied_seq,
                        applied: true,
                    }));
                    satisfy_barriers(state.applied_seq, &mut waiters);
                }
                FamilyMsg::Status(respond_to) => {
                    let _ = respond_to.send(Ok(FamilyStatus {
                        family_key: state.family_key.clone(),
                        applied_seq: state.applied_seq,
                        recent_command_count: state.recent_commands.len(),
                        unresolved_transcripts: state.unresolved_transcripts.len(),
                        effect_queue_depth: effect_runner.queue_depth(),
                        last_error: state.last_error.clone(),
                        last_reconcile_ns: state.last_reconcile_ns,
                    }));
                }
                FamilyMsg::Snapshot(respond_to) => {
                    let _ = respond_to.send(Ok(FamilySnapshot {
                        family_key: state.family_key.clone(),
                        refs: state.refs.clone(),
                        worktrees: state.worktrees.clone(),
                        recent_commands: state.recent_commands.iter().cloned().collect(),
                        checkpoints: state.checkpoints.clone(),
                        unresolved_transcripts: state
                            .unresolved_transcripts
                            .iter()
                            .cloned()
                            .collect(),
                        active_cherry_pick: state.active_cherry_pick.clone(),
                        env_overrides: state.env_overrides.clone(),
                        last_error: state.last_error.clone(),
                        last_reconcile_ns: state.last_reconcile_ns,
                        applied_seq: state.applied_seq,
                    }));
                }
                FamilyMsg::Barrier(seq, respond_to) => {
                    if state.applied_seq >= seq {
                        let _ = respond_to.send(Ok(()));
                    } else {
                        waiters.push((seq, respond_to));
                    }
                }
                FamilyMsg::Shutdown => break,
            }
        }
        let _ = effect_runner.shutdown().await;
    });

    handle
}

fn satisfy_barriers(
    applied_seq: u64,
    waiters: &mut Vec<(u64, oneshot::Sender<Result<(), GitAiError>>)>,
) {
    let mut idx = 0;
    while idx < waiters.len() {
        if applied_seq >= waiters[idx].0 {
            let (_, waiter) = waiters.remove(idx);
            let _ = waiter.send(Ok(()));
        } else {
            idx += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{AliasResolution, CommandScope, Confidence, NormalizedCommand};
    use std::path::PathBuf;

    fn sample_normalized_cmd(family_key: &str, seq: u128) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new(family_key)),
            family_key: Some(FamilyKey::new(family_key)),
            worktree: Some(PathBuf::from("/tmp/repo")),
            root_sid: format!("sid-{}", seq),
            raw_argv: vec!["git".to_string(), "status".to_string()],
            primary_command: Some("status".to_string()),
            alias_resolution: AliasResolution::None,
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: seq,
            finished_at_ns: seq + 1,
            pre_repo: None,
            post_repo: None,
            pre_stash_sha: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[tokio::test]
    async fn actor_applies_commands_and_barrier_is_state_based() {
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
        actor.barrier(2).await.unwrap();
        actor.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn actor_status_and_snapshot_report_applied_seq() {
        let actor = spawn_family_actor(FamilyKey::new("family-2"));
        actor
            .apply(sample_normalized_cmd("family-2", 1))
            .await
            .unwrap();
        let status = actor.status().await.unwrap();
        assert_eq!(status.applied_seq, 1);
        let snapshot = actor.snapshot().await.unwrap();
        assert_eq!(snapshot.applied_seq, 1);
        assert_eq!(snapshot.recent_commands.len(), 1);
        actor.shutdown().await.unwrap();
    }
}
