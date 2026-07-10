use crate::daemon::domain::{
    AppliedCommand, ApplyAck, CommandScope, FamilyKey, FamilyStatus, NormalizedCommand,
    WatermarkState,
};
use crate::daemon::family_actor::{FamilyActorHandle, spawn_family_actor};
use crate::daemon::git_backend::GitBackend;
use crate::daemon::global_actor::{GlobalActorHandle, spawn_global_actor};
use crate::error::GitAiError;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

const MAX_RETAINED_FAMILY_ACTORS: usize = 1_024;

struct FamilyActorRegistry {
    actors: HashMap<String, FamilyActorHandle>,
    insertion_order: VecDeque<String>,
    saturated: bool,
}

impl FamilyActorRegistry {
    fn new() -> Self {
        Self {
            actors: HashMap::new(),
            insertion_order: VecDeque::new(),
            saturated: false,
        }
    }

    fn evict_oldest_idle(&mut self) -> Option<FamilyActorHandle> {
        for _ in 0..self.insertion_order.len() {
            let key = self.insertion_order.pop_front()?;
            let Some(actor) = self.actors.get(&key) else {
                continue;
            };
            if actor.is_closed() || actor.is_idle_for_eviction() {
                return self.actors.remove(&key);
            }
            self.insertion_order.push_back(key);
        }
        None
    }
}

fn configured_family_actor_capacity() -> usize {
    let is_test_daemon = std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some();
    if is_test_daemon
        && let Some(capacity) = std::env::var("GIT_AI_TEST_MAX_FAMILY_ACTORS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|capacity| *capacity > 0)
    {
        return capacity;
    }
    MAX_RETAINED_FAMILY_ACTORS
}

pub struct Coordinator<B: GitBackend> {
    backend: Arc<B>,
    global: GlobalActorHandle,
    families: Mutex<FamilyActorRegistry>,
    max_family_actors: usize,
}

impl<B: GitBackend> Coordinator<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            global: spawn_global_actor(),
            families: tokio::sync::Mutex::new(FamilyActorRegistry::new()),
            max_family_actors: configured_family_actor_capacity(),
        }
    }

    pub async fn route_command(
        &self,
        cmd: NormalizedCommand,
    ) -> Result<AppliedCommand, GitAiError> {
        match &cmd.scope {
            CommandScope::Global => self.global.apply(cmd).await,
            CommandScope::Family(key) => {
                let actor = self.get_or_create_family_actor(key.clone()).await;
                actor.apply(cmd).await
            }
        }
    }

    pub async fn apply_checkpoint(&self, repo_working_dir: &Path) -> Result<ApplyAck, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.apply_checkpoint().await
    }

    pub async fn watermarks_family(
        &self,
        repo_working_dir: &Path,
    ) -> Result<WatermarkState, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.watermarks().await
    }

    pub async fn update_watermarks_family(
        &self,
        repo_working_dir: &Path,
        update: WatermarkState,
    ) -> Result<(), GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.update_watermarks(update).await
    }

    pub async fn status_family(&self, repo_working_dir: &Path) -> Result<FamilyStatus, GitAiError> {
        let family = self.backend.resolve_family(repo_working_dir)?;
        let actor = self.get_or_create_family_actor(family).await;
        actor.status().await
    }

    pub async fn shutdown(&self) -> Result<(), GitAiError> {
        let actors = {
            let registry = self.families.lock().await;
            registry.actors.values().cloned().collect::<Vec<_>>()
        };
        for actor in actors {
            let _ = actor.shutdown().await;
        }
        self.global.shutdown().await
    }

    async fn get_or_create_family_actor(&self, family_key: FamilyKey) -> FamilyActorHandle {
        let mut registry = self.families.lock().await;
        if let Some(existing) = registry.actors.get(&family_key.0) {
            return existing.clone();
        }

        while registry.actors.len() >= self.max_family_actors {
            let Some(evicted) = registry.evict_oldest_idle() else {
                if !registry.saturated {
                    registry.saturated = true;
                    tracing::warn!(
                        capacity = self.max_family_actors,
                        "family actor registry is full; using a transient actor for a new repository"
                    );
                }
                return spawn_family_actor(family_key);
            };
            tracing::debug!(
                family = %evicted.family_key.0,
                capacity = self.max_family_actors,
                "evicted idle family actor"
            );
            drop(evicted);
        }

        registry.saturated = false;
        let created = spawn_family_actor(family_key.clone());
        registry.insertion_order.push_back(family_key.0.clone());
        registry
            .actors
            .insert(family_key.0.clone(), created.clone());
        created
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{CommandScope, Confidence, FamilyKey, NormalizedCommand};
    use crate::daemon::git_backend::GitBackend;
    use crate::git::cli_parser::parse_git_cli_args;
    use std::path::{Path, PathBuf};
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

        fn resolve_primary_command(
            &self,
            _worktree: &Path,
            argv: &[String],
        ) -> Result<Option<String>, GitAiError> {
            let tokens: &[String] = if argv
                .first()
                .is_some_and(|value| value == "git" || value == "git.exe")
            {
                &argv[1..]
            } else {
                argv
            };
            Ok(parse_git_cli_args(tokens).command)
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
            invoked_command: Some("help".to_string()),
            invoked_args: Vec::new(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            reflog_start_offsets: std::collections::HashMap::new(),
            stash_target_oid: None,
            cherry_pick_source_oids: Vec::new(),
            revert_source_oids: Vec::new(),
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
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
            invoked_command: Some("status".to_string()),
            invoked_args: Vec::new(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            reflog_start_offsets: std::collections::HashMap::new(),
            stash_target_oid: None,
            cherry_pick_source_oids: Vec::new(),
            revert_source_oids: Vec::new(),
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
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
    async fn family_actor_registry_is_bounded() {
        const EXPECTED_FAMILY_ACTOR_LIMIT: usize = 1_024;

        let coordinator = Coordinator::new(Arc::new(MockBackend::default()));
        for index in 0..=EXPECTED_FAMILY_ACTOR_LIMIT {
            let actor = coordinator
                .get_or_create_family_actor(FamilyKey::new(format!("family-{index}")))
                .await;
            drop(actor);
        }

        assert_eq!(
            coordinator.families.lock().await.actors.len(),
            EXPECTED_FAMILY_ACTOR_LIMIT
        );
    }

    #[tokio::test]
    async fn family_actor_registry_never_evicts_a_borrowed_actor() {
        let coordinator = Coordinator {
            backend: Arc::new(MockBackend::default()),
            global: spawn_global_actor(),
            families: tokio::sync::Mutex::new(FamilyActorRegistry::new()),
            max_family_actors: 1,
        };
        let active = coordinator
            .get_or_create_family_actor(FamilyKey::new("active"))
            .await;

        let transient = coordinator
            .get_or_create_family_actor(FamilyKey::new("transient"))
            .await;
        {
            let registry = coordinator.families.lock().await;
            assert!(registry.actors.contains_key("active"));
            assert!(!registry.actors.contains_key("transient"));
        }
        drop(transient);

        active.apply_checkpoint().await.unwrap();
        drop(active);
        let replacement = coordinator
            .get_or_create_family_actor(FamilyKey::new("replacement"))
            .await;
        let registry = coordinator.families.lock().await;
        assert!(!registry.actors.contains_key("active"));
        assert!(registry.actors.contains_key("replacement"));
        drop(registry);
        replacement.shutdown().await.unwrap();
    }
}
