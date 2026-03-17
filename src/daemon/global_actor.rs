use crate::daemon::analyzers::AnalyzerRegistry;
use crate::daemon::domain::{AppliedCommand, GlobalSnapshot, GlobalState, NormalizedCommand};
use crate::daemon::reducer;
use crate::error::GitAiError;
use std::collections::VecDeque;
use tokio::sync::{mpsc, oneshot};

pub enum GlobalMsg {
    Apply(
        Box<NormalizedCommand>,
        oneshot::Sender<Result<AppliedCommand, GitAiError>>,
    ),
    Snapshot(oneshot::Sender<Result<GlobalSnapshot, GitAiError>>),
    Barrier(u64, oneshot::Sender<Result<(), GitAiError>>),
    Shutdown,
}

#[derive(Clone)]
pub struct GlobalActorHandle {
    tx: mpsc::Sender<GlobalMsg>,
}

impl GlobalActorHandle {
    pub async fn apply(&self, cmd: NormalizedCommand) -> Result<AppliedCommand, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(GlobalMsg::Apply(Box::new(cmd), tx))
            .await
            .map_err(|_| GitAiError::Generic("global actor apply send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("global actor apply receive failed".to_string()))?
    }

    pub async fn snapshot(&self) -> Result<GlobalSnapshot, GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(GlobalMsg::Snapshot(tx))
            .await
            .map_err(|_| GitAiError::Generic("global actor snapshot send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("global actor snapshot receive failed".to_string()))?
    }

    pub async fn barrier(&self, seq: u64) -> Result<(), GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(GlobalMsg::Barrier(seq, tx))
            .await
            .map_err(|_| GitAiError::Generic("global actor barrier send failed".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("global actor barrier receive failed".to_string()))?
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        self.tx
            .send(GlobalMsg::Shutdown)
            .await
            .map_err(|_| GitAiError::Generic("global actor shutdown send failed".to_string()))
    }
}

pub fn spawn_global_actor() -> GlobalActorHandle {
    let (tx, mut rx) = mpsc::channel::<GlobalMsg>(1024);
    let handle = GlobalActorHandle { tx };

    tokio::spawn(async move {
        let analyzers = AnalyzerRegistry::new();
        let mut state = GlobalState {
            recent_commands: VecDeque::new(),
            applied_seq: 0,
        };
        let mut waiters: Vec<(u64, oneshot::Sender<Result<(), GitAiError>>)> = Vec::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                GlobalMsg::Apply(cmd, respond_to) => {
                    let result = reducer::reduce_global_command(&mut state, *cmd, &analyzers)
                        .map(|(applied, _)| applied);
                    let seq = result
                        .as_ref()
                        .map(|applied| applied.seq)
                        .unwrap_or(state.applied_seq);
                    let _ = respond_to.send(result);
                    satisfy_barriers(seq, &mut waiters);
                }
                GlobalMsg::Snapshot(respond_to) => {
                    let _ = respond_to.send(Ok(GlobalSnapshot {
                        recent_commands: state.recent_commands.iter().cloned().collect(),
                        applied_seq: state.applied_seq,
                    }));
                }
                GlobalMsg::Barrier(seq, respond_to) => {
                    if state.applied_seq >= seq {
                        let _ = respond_to.send(Ok(()));
                    } else {
                        waiters.push((seq, respond_to));
                    }
                }
                GlobalMsg::Shutdown => break,
            }
        }
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
    use crate::daemon::domain::{CommandScope, Confidence, NormalizedCommand};

    fn global_cmd(seq: u128) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: format!("global-{}", seq),
            raw_argv: vec!["git".to_string(), "help".to_string()],
            primary_command: Some("help".to_string()),
            invoked_command: Some("help".to_string()),
            invoked_args: Vec::new(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: seq,
            finished_at_ns: seq + 1,
            pre_repo: None,
            post_repo: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[tokio::test]
    async fn global_actor_applies_and_barrier_waits_for_applied_seq() {
        let actor = spawn_global_actor();
        let ack = actor.apply(global_cmd(1)).await.unwrap();
        assert_eq!(ack.seq, 1);
        actor.barrier(1).await.unwrap();
        actor.shutdown().await.unwrap();
    }
}
