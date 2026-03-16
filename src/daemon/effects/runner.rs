use crate::daemon::effects::planner::EffectIntent;
use crate::error::GitAiError;
use crate::utils::debug_log;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectRunnerMode {
    Shadow,
    Write,
}

impl EffectRunnerMode {
    pub fn apply_side_effects(self) -> bool {
        matches!(self, Self::Write)
    }
}

pub trait EffectExecutor: Send + Sync + 'static {
    fn execute(&self, intent: &EffectIntent) -> Result<(), GitAiError>;
}

#[derive(Default)]
pub struct NoopEffectExecutor;

impl EffectExecutor for NoopEffectExecutor {
    fn execute(&self, intent: &EffectIntent) -> Result<(), GitAiError> {
        debug_log(&format!("effect runner noop executed intent: {:?}", intent));
        Ok(())
    }
}

enum RunnerMsg {
    Enqueue(EffectIntent),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct FamilyEffectRunner {
    tx: mpsc::Sender<RunnerMsg>,
    pending: Arc<AtomicUsize>,
}

impl FamilyEffectRunner {
    pub fn spawn(mode: EffectRunnerMode, executor: Arc<dyn EffectExecutor>) -> Self {
        let (tx, mut rx) = mpsc::channel::<RunnerMsg>(1024);
        let pending = Arc::new(AtomicUsize::new(0));
        let pending_for_task = pending.clone();

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    RunnerMsg::Enqueue(intent) => {
                        if mode.apply_side_effects() {
                            if let Err(err) = executor.execute(&intent) {
                                debug_log(&format!("effect execution failed: {}", err));
                            }
                        } else {
                            debug_log(&format!("shadow effect intent planned: {:?}", intent));
                        }
                        pending_for_task.fetch_sub(1, Ordering::SeqCst);
                    }
                    RunnerMsg::Shutdown(done) => {
                        let _ = done.send(());
                        break;
                    }
                }
            }
        });

        Self { tx, pending }
    }

    pub async fn enqueue(&self, intent: EffectIntent) -> Result<(), GitAiError> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if let Err(_send_error) = self.tx.send(RunnerMsg::Enqueue(intent)).await {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return Err(GitAiError::Generic(
                "failed enqueueing effect intent".to_string(),
            ));
        }
        Ok(())
    }

    pub fn try_enqueue(&self, intent: EffectIntent) -> Result<(), GitAiError> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if let Err(_send_error) = self.tx.try_send(RunnerMsg::Enqueue(intent)) {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return Err(GitAiError::Generic(
                "failed enqueueing effect intent".to_string(),
            ));
        }
        Ok(())
    }

    pub fn queue_depth(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(RunnerMsg::Shutdown(tx))
            .await
            .map_err(|_| GitAiError::Generic("failed to send effect shutdown".to_string()))?;
        rx.await
            .map_err(|_| GitAiError::Generic("failed to wait for effect shutdown".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::effects::planner::EffectIntent;

    #[tokio::test]
    async fn shadow_runner_tracks_queue_depth() {
        let runner = FamilyEffectRunner::spawn(EffectRunnerMode::Shadow, Arc::new(NoopEffectExecutor));
        runner
            .enqueue(EffectIntent::SyncAuthorshipNotes {
                reason: "fetch".to_string(),
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(runner.queue_depth(), 0);
        runner.shutdown().await.unwrap();
    }
}
