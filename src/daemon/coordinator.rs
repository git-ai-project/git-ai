use crate::daemon::domain::{
    ApplyAck, CheckpointObserved, CommandScope, EnvOverrideSet, FamilyKey, FamilySnapshot,
    FamilyStatus, GlobalSnapshot, NormalizedCommand, ReconcileSnapshot,
};
use crate::daemon::family_actor::{FamilyActorHandle, spawn_family_actor};
use crate::daemon::git_backend::GitBackend;
use crate::daemon::global_actor::{GlobalActorHandle, spawn_global_actor};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct Coordinator<B: GitBackend> {
    backend: Arc<B>,
    global: GlobalActorHandle,
    families: Mutex<HashMap<String, FamilyActorHandle>>,
}

impl<B: GitBackend> Coordinator<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            global: spawn_global_actor(),
            families: Mutex::new(HashMap::new()),
        }
    }

    pub async fn route_command(&self, cmd: NormalizedCommand) -> Result<ApplyAck, GitAiError> {
        match &cmd.scope {
            CommandScope::Global => self.global.apply(cmd).await,
            CommandScope::Family(key) => {
                let actor = self.get_or_create_family_actor(key.clone()).await;
                actor.apply(cmd).await
            }
        }
    }

    pub async fn apply_checkpoint(
        &self,
        checkpoint: CheckpointObserved,
    ) -> Result<ApplyAck, GitAiError> {
        let family = self.backend.resolve_family(&checkpoint.repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.apply_checkpoint(checkpoint).await
    }

    pub async fn apply_env_override(&self, env: EnvOverrideSet) -> Result<ApplyAck, GitAiError> {
        let family = self.backend.resolve_family(&env.repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.apply_env_override(env).await
    }

    pub async fn reconcile_family(
        &self,
        repo_working_dir: &Path,
        snapshot: ReconcileSnapshot,
    ) -> Result<ApplyAck, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.reconcile(snapshot).await
    }

    pub async fn status_family(&self, repo_working_dir: &Path) -> Result<FamilyStatus, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.status().await
    }

    pub async fn snapshot_family(
        &self,
        repo_working_dir: &Path,
    ) -> Result<FamilySnapshot, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.snapshot().await
    }

    pub async fn barrier_family(
        &self,
        repo_working_dir: &Path,
        seq: u64,
    ) -> Result<(), GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.barrier(seq).await
    }

    pub async fn snapshot_global(&self) -> Result<GlobalSnapshot, GitAiError> {
        self.global.snapshot().await
    }

    pub async fn barrier_global(&self, seq: u64) -> Result<(), GitAiError> {
        self.global.barrier(seq).await
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        let actors = {
            let map = self.families.lock().await;
            map.values().cloned().collect::<Vec<_>>()
        };
        for actor in actors {
            let _ = actor.shutdown().await;
        }
        self.global.shutdown().await
    }

    async fn get_or_create_family_actor(&self, family_key: FamilyKey) -> FamilyActorHandle {
        let mut map = self.families.lock().await;
        if let Some(existing) = map.get(&family_key.0) {
            return existing.clone();
        }
        let created = spawn_family_actor(family_key.clone());
        map.insert(family_key.0.clone(), created.clone());
        created
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{CommandScope, Confidence, FamilyKey, NormalizedCommand, RepoContext};
    use crate::daemon::git_backend::{GitBackend, ReflogCut};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockBackend {
        families: Mutex<HashMap<String, FamilyKey>>,
    }

    impl MockBackend {
        fn with_family(self, worktree: &str, family_key: &str) -> Self {
            self.families
                .lock()
                .unwrap()
                .insert(worktree.to_string(), FamilyKey::new(family_key.to_string()));
            self
        }
    }

    impl GitBackend for MockBackend {
        fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError> {
            self.families
                .lock()
                .unwrap()
                .get(worktree.to_string_lossy().as_ref())
                .cloned()
                .ok_or_else(|| GitAiError::Generic("family not found".to_string()))
        }

        fn repo_context(&self, _worktree: &Path) -> Result<RepoContext, GitAiError> {
            Err(GitAiError::Generic("unused".to_string()))
        }

        fn ref_snapshot(&self, _family: &FamilyKey) -> Result<HashMap<String, String>, GitAiError> {
            Ok(HashMap::new())
        }

        fn reflog_cut(&self, _family: &FamilyKey) -> Result<ReflogCut, GitAiError> {
            Err(GitAiError::Generic("unused".to_string()))
        }

        fn reflog_delta(
            &self,
            _family: &FamilyKey,
            _start: &ReflogCut,
            _end: &ReflogCut,
        ) -> Result<Vec<crate::daemon::domain::RefChange>, GitAiError> {
            Ok(Vec::new())
        }

        fn clone_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            None
        }

        fn init_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            None
        }
    }

    fn global_cmd() -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: "g1".to_string(),
            raw_argv: vec!["git".to_string(), "help".to_string()],
            primary_command: Some("help".to_string()),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    fn family_cmd(family: &str, worktree: &str) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new(family.to_string())),
            family_key: Some(FamilyKey::new(family.to_string())),
            worktree: Some(PathBuf::from(worktree)),
            root_sid: "f1".to_string(),
            raw_argv: vec!["git".to_string(), "status".to_string()],
            primary_command: Some("status".to_string()),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[tokio::test]
    async fn routes_global_and_family_commands() {
        let backend = Arc::new(MockBackend::default().with_family("/repo", "family:/repo"));
        let coordinator = Coordinator::new(backend);

        let g = coordinator.route_command(global_cmd()).await.unwrap();
        assert_eq!(g.seq, 1);

        let f = coordinator
            .route_command(family_cmd("family:/repo", "/repo"))
            .await
            .unwrap();
        assert_eq!(f.seq, 1);

        coordinator.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn barrier_waits_on_family_applied_seq() {
        let backend = Arc::new(MockBackend::default().with_family("/repo", "family:/repo"));
        let coordinator = Coordinator::new(backend);
        let _ = coordinator
            .route_command(family_cmd("family:/repo", "/repo"))
            .await
            .unwrap();
        coordinator
            .barrier_family(Path::new("/repo"), 1)
            .await
            .unwrap();
        coordinator.shutdown().await.unwrap();
    }
}
