use crate::daemon::domain::{
    CommandScope, Confidence, FamilyKey, NormalizedCommand, RefChange, RepoContext,
};
use crate::daemon::git_backend::{GitBackend, ReflogCut};
use crate::error::GitAiError;
use crate::git::cli_parser::{
    explicit_rebase_branch_arg, parse_git_cli_args, rebase_has_control_mode,
};
use crate::git::repo_state::{
    common_dir_for_repo_path, common_dir_for_worktree, git_dir_for_worktree,
    read_ref_oid_for_common_dir, worktree_root_for_path,
};
use crate::observability;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PendingTraceCommand {
    pub root_sid: String,
    pub raw_argv: Vec<String>,
    pub root_cmd_name: Option<String>,
    pub observed_child_commands: Vec<String>,
    pub invocation_worktree: Option<PathBuf>,
    pub worktree: Option<PathBuf>,
    pub family_key: Option<FamilyKey>,
    pub started_at_ns: u128,
    pub exit_code: Option<i32>,
    pub finished_at_ns: Option<u128>,
    pub pre_repo: Option<RepoContext>,
    pub post_repo: Option<RepoContext>,
    pub merge_squash_source_head: Option<String>,
    pub reflog_start_cut: Option<ReflogCut>,
    pub reflog_end_cut: Option<ReflogCut>,
    pub captured_ref_changes: Vec<RefChange>,
    pub stash_target_oid: Option<String>,
    pub stash_target_error: Option<String>,
    pub carryover_snapshot_id: Option<String>,
    pub worktree_head_start_offset: Option<u64>,
    pub worktree_head_end_offset: Option<u64>,
    pub saw_def_repo: bool,
    pub rebase_original_head_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TraceNormalizerState {
    pub pending: HashMap<String, PendingTraceCommand>,
    pub deferred_exits: HashMap<String, DeferredRootExit>,
    pub completed_roots: HashSet<String>,
    pub completed_root_order: VecDeque<String>,
    pub sid_to_worktree: HashMap<String, PathBuf>,
    pub sid_to_family: HashMap<String, FamilyKey>,
    pub prestart_root_cmd_names: HashMap<String, String>,
    pub root_wrapper_invocation_id: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DeferredRootExit {
    pub exit_code: i32,
    pub finished_at_ns: u128,
    pub pre_repo: Option<RepoContext>,
    pub post_repo: Option<RepoContext>,
    pub merge_squash_source_head: Option<String>,
    pub worktree_head_start_offset: Option<u64>,
    pub worktree_head_end_offset: Option<u64>,
    pub reflog_start_cut: Option<ReflogCut>,
    pub reflog_end_cut: Option<ReflogCut>,
    pub captured_ref_changes: Vec<RefChange>,
    pub carryover_snapshot_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrphanTraceRoot {
    pub root_sid: String,
    pub raw_argv: Vec<String>,
    pub deferred_exit_only: bool,
}

pub struct TraceNormalizer<B: GitBackend> {
    backend: Arc<B>,
    state: TraceNormalizerState,
}

const COMPLETED_ROOT_RETENTION_LIMIT: usize = 16_384;

impl<B: GitBackend> TraceNormalizer<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            state: TraceNormalizerState::default(),
        }
    }

    pub fn state(&self) -> &TraceNormalizerState {
        &self.state
    }

    fn is_completed_root(&self, root_sid: &str) -> bool {
        self.state.completed_roots.contains(root_sid)
    }

    fn mark_completed_root_with_limit(&mut self, root_sid: &str, limit: usize) {
        if self.state.completed_roots.insert(root_sid.to_string()) {
            self.state
                .completed_root_order
                .push_back(root_sid.to_string());
        }
        while self.state.completed_roots.len() > limit {
            let Some(oldest) = self.state.completed_root_order.pop_front() else {
                break;
            };
            self.state.completed_roots.remove(&oldest);
        }
    }

    fn mark_completed_root(&mut self, root_sid: &str) {
        self.mark_completed_root_with_limit(root_sid, COMPLETED_ROOT_RETENTION_LIMIT);
    }

    pub fn remove_pending_root(&mut self, root_sid: &str) -> Option<PendingTraceCommand> {
        let removed = self.state.pending.remove(root_sid);
        if removed.is_some() {
            let _ = self.state.sid_to_worktree.remove(root_sid);
            let _ = self.state.sid_to_family.remove(root_sid);
            let _ = self.state.prestart_root_cmd_names.remove(root_sid);
        }
        removed
    }

    pub fn sweep_orphans(&mut self) -> Vec<OrphanTraceRoot> {
        let mut removed = Vec::new();

        let pending_roots = self.state.pending.keys().cloned().collect::<Vec<_>>();
        for root_sid in pending_roots {
            if let Some(pending) = self.remove_pending_root(&root_sid) {
                removed.push(OrphanTraceRoot {
                    root_sid,
                    raw_argv: pending.raw_argv,
                    deferred_exit_only: false,
                });
            }
        }

        let deferred_roots = self
            .state
            .deferred_exits
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for root_sid in deferred_roots {
            self.state.deferred_exits.remove(&root_sid);
            let _ = self.state.sid_to_worktree.remove(&root_sid);
            let _ = self.state.sid_to_family.remove(&root_sid);
            let _ = self.state.prestart_root_cmd_names.remove(&root_sid);
            removed.push(OrphanTraceRoot {
                root_sid,
                raw_argv: Vec::new(),
                deferred_exit_only: true,
            });
        }

        removed
    }

    pub fn sweep_orphans_for_roots(&mut self, roots: &[String]) -> Vec<OrphanTraceRoot> {
        let mut removed = Vec::new();

        for root_sid in roots {
            if let Some(pending) = self.remove_pending_root(root_sid) {
                removed.push(OrphanTraceRoot {
                    root_sid: root_sid.clone(),
                    raw_argv: pending.raw_argv,
                    deferred_exit_only: false,
                });
                continue;
            }

            if self.state.deferred_exits.remove(root_sid).is_some() {
                let _ = self.state.sid_to_worktree.remove(root_sid);
                let _ = self.state.sid_to_family.remove(root_sid);
                let _ = self.state.prestart_root_cmd_names.remove(root_sid);
                removed.push(OrphanTraceRoot {
                    root_sid: root_sid.clone(),
                    raw_argv: Vec::new(),
                    deferred_exit_only: true,
                });
            }
        }

        removed
    }

    fn resolve_primary_hint(
        &self,
        root_cmd_name: Option<&str>,
        observed_child_commands: &[String],
        raw_argv: &[String],
        worktree: Option<&Path>,
        family_key: Option<&FamilyKey>,
    ) -> Result<Option<String>, GitAiError> {
        let argv_primary = argv_primary_command(raw_argv);
        let selected = select_primary_command(root_cmd_name, observed_child_commands, raw_argv)
            .or_else(|| argv_primary.clone());
        let should_resolve_alias = match (&selected, &argv_primary) {
            // Keep child/root-derived command if it differs from the argv command.
            // Alias resolution should only rewrite the invoked command token.
            (Some(selected_cmd), Some(argv_cmd)) => selected_cmd == argv_cmd,
            (None, Some(_)) => true,
            _ => false,
        };
        if should_resolve_alias
            && let (Some(worktree), Some(_family)) = (worktree, family_key)
            && let Some(resolved) = self.backend.resolve_primary_command(worktree, raw_argv)?
        {
            return Ok(Some(resolved));
        }
        Ok(selected)
    }

    fn refresh_pending_mutation_capture(&mut self, root_sid: &str) -> Result<(), GitAiError> {
        let primary_hint = {
            let pending = match self.state.pending.get(root_sid) {
                Some(pending) => pending,
                None => return Ok(()),
            };

            let (Some(worktree), Some(family)) =
                (pending.worktree.as_deref(), pending.family_key.as_ref())
            else {
                return Ok(());
            };

            self.resolve_primary_hint(
                pending.root_cmd_name.as_deref(),
                &pending.observed_child_commands,
                &pending.raw_argv,
                Some(worktree),
                Some(family),
            )?
        };
        if !command_may_mutate_refs(primary_hint.as_deref()) {
            return Ok(());
        }
        // Reflog/HEAD cuts are injected at ingress-time on exit payloads.
        // Avoid any live snapshotting here to keep normalization race-free.
        Ok(())
    }

    fn merge_pending_worktree_head_offsets(
        &mut self,
        root_sid: &str,
        start_offset: Option<u64>,
        end_offset: Option<u64>,
    ) {
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            if let Some(start_offset) = start_offset {
                match pending.worktree_head_start_offset {
                    Some(existing) if existing <= start_offset => {}
                    _ => pending.worktree_head_start_offset = Some(start_offset),
                }
            }
            if let Some(end_offset) = end_offset {
                match pending.worktree_head_end_offset {
                    Some(existing) if existing >= end_offset => {}
                    _ => pending.worktree_head_end_offset = Some(end_offset),
                }
            }
        }
    }

    fn merge_pending_family_reflog_cuts(
        &mut self,
        root_sid: &str,
        start_cut: Option<ReflogCut>,
        end_cut: Option<ReflogCut>,
    ) {
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            merge_reflog_cut(&mut pending.reflog_start_cut, start_cut, MergeCutMode::Min);
            merge_reflog_cut(&mut pending.reflog_end_cut, end_cut, MergeCutMode::Max);
        }
    }

    fn merge_pending_ref_changes(&mut self, root_sid: &str, incoming: Vec<RefChange>) {
        if incoming.is_empty() {
            return;
        }
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            for change in incoming {
                let duplicate = pending.captured_ref_changes.iter().any(|existing| {
                    existing.reference == change.reference
                        && existing.old == change.old
                        && existing.new == change.new
                });
                if !duplicate {
                    pending.captured_ref_changes.push(change);
                }
            }
        }
    }

    fn merge_pending_stash_metadata(
        &mut self,
        root_sid: &str,
        stash_target_oid: Option<String>,
        stash_target_error: Option<String>,
    ) {
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            if let Some(stash_target_oid) = stash_target_oid {
                pending.stash_target_oid = Some(stash_target_oid);
                pending.stash_target_error = None;
            } else if pending.stash_target_oid.is_none()
                && let Some(stash_target_error) = stash_target_error
            {
                pending.stash_target_error = Some(stash_target_error);
            }
        }
    }

    fn merge_pending_carryover_snapshot_id(
        &mut self,
        root_sid: &str,
        carryover_snapshot_id: Option<String>,
    ) {
        if let Some(pending) = self.state.pending.get_mut(root_sid)
            && let Some(carryover_snapshot_id) = carryover_snapshot_id
        {
            pending.carryover_snapshot_id = Some(carryover_snapshot_id);
        }
    }

    fn merge_pending_merge_squash_source_head(
        &mut self,
        root_sid: &str,
        source_head: Option<String>,
    ) {
        if let Some(pending) = self.state.pending.get_mut(root_sid)
            && let Some(source_head) = source_head
        {
            pending.merge_squash_source_head = Some(source_head);
        }
    }

    pub fn ingest_payload(
        &mut self,
        payload: &Value,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("trace payload missing event".to_string()))?;
        let sid = payload
            .get("sid")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("trace payload missing sid".to_string()))?;
        let root_sid = root_sid(sid).to_string();
        if self.is_completed_root(&root_sid) {
            return Ok(None);
        }
        let ts = payload_timestamp_ns(payload)?;
        let (payload_head_start, payload_head_end) = payload_worktree_head_offsets(payload);
        self.merge_pending_worktree_head_offsets(&root_sid, payload_head_start, payload_head_end);
        let (payload_reflog_start, payload_reflog_end) = payload_family_reflog_cuts(payload);
        self.merge_pending_family_reflog_cuts(&root_sid, payload_reflog_start, payload_reflog_end);
        self.merge_pending_ref_changes(&root_sid, payload_reflog_changes(payload));
        self.merge_pending_stash_metadata(
            &root_sid,
            payload_string_field(payload, "git_ai_stash_target_oid"),
            payload_string_field(payload, "git_ai_stash_target_oid_error"),
        );
        self.merge_pending_merge_squash_source_head(
            &root_sid,
            payload_string_field(payload, "git_ai_merge_squash_source_head"),
        );
        self.merge_pending_carryover_snapshot_id(
            &root_sid,
            payload_string_field(payload, "git_ai_carryover_snapshot_id"),
        );

        match event {
            "start" => self.handle_start(payload, sid, &root_sid, ts),
            "def_repo" => self.handle_def_repo(payload, sid, &root_sid),
            "cmd_name" => self.handle_cmd_name(payload, sid, &root_sid),
            "def_param" => self.handle_def_param(payload, &root_sid),
            "exec" => Ok(None),
            "exit" => self.handle_exit(payload, sid, &root_sid, ts),
            "atexit" => self.handle_exit(payload, sid, &root_sid, ts),
            _ => Ok(None),
        }
    }

    fn handle_start(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
        started_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        if sid != root_sid {
            return Ok(None);
        }
        if self.is_completed_root(root_sid) {
            return Ok(None);
        }

        let raw_argv = payload_argv(payload);
        let worktree = payload_worktree(payload)
            .or_else(|| worktree_from_argv(&raw_argv))
            .or_else(|| payload_cwd(payload))
            .or_else(|| self.state.sid_to_worktree.get(root_sid).cloned());

        let family_key = if let Some(worktree) = worktree.as_deref() {
            if let Some(common_dir) = common_dir_for_worktree(worktree) {
                let family = FamilyKey::new(
                    common_dir
                        .canonicalize()
                        .unwrap_or(common_dir)
                        .to_string_lossy()
                        .to_string(),
                );
                self.state
                    .sid_to_family
                    .insert(root_sid.to_string(), family.clone());
                Some(family)
            } else {
                self.state.sid_to_family.get(root_sid).cloned()
            }
        } else {
            self.state.sid_to_family.get(root_sid).cloned()
        };

        let primary_hint = self.resolve_primary_hint(
            None,
            &[],
            &raw_argv,
            worktree.as_deref(),
            family_key.as_ref(),
        )?;
        let should_capture_mutation_state =
            command_may_mutate_refs(primary_hint.as_deref()) && family_key.is_some();
        let (_invoked_command, invoked_args) =
            canonical_invocation(&raw_argv, primary_hint.as_deref());
        let rebase_original_head_hint = if primary_hint.as_deref() == Some("rebase")
            && !rebase_has_control_mode(&invoked_args)
        {
            family_key.as_ref().and_then(|family| {
                explicit_rebase_branch_arg(&invoked_args)
                    .as_ref()
                    .and_then(|branch| resolve_rebase_branch_head_hint(family, branch))
            })
        } else {
            None
        };
        let reflog_start_cut = if should_capture_mutation_state {
            payload_reflog_cut(payload, "git_ai_family_reflog_start")
        } else {
            None
        };
        let worktree_head_start_offset = if should_capture_mutation_state {
            payload
                .get("git_ai_worktree_head_reflog_start")
                .and_then(Value::as_u64)
        } else {
            None
        };
        let pre_repo = payload_repo_context(payload, "git_ai_pre_repo");
        let stash_target_oid = payload_string_field(payload, "git_ai_stash_target_oid");
        let stash_target_error = payload_string_field(payload, "git_ai_stash_target_oid_error");
        let merge_squash_source_head =
            payload_string_field(payload, "git_ai_merge_squash_source_head");
        let carryover_snapshot_id = payload_string_field(payload, "git_ai_carryover_snapshot_id");

        let pending = PendingTraceCommand {
            root_sid: root_sid.to_string(),
            raw_argv,
            root_cmd_name: None,
            observed_child_commands: Vec::new(),
            invocation_worktree: worktree.clone(),
            worktree,
            family_key,
            started_at_ns,
            exit_code: None,
            finished_at_ns: None,
            pre_repo,
            post_repo: None,
            merge_squash_source_head,
            reflog_start_cut,
            reflog_end_cut: None,
            captured_ref_changes: Vec::new(),
            stash_target_oid,
            stash_target_error,
            carryover_snapshot_id,
            worktree_head_start_offset,
            worktree_head_end_offset: None,
            saw_def_repo: false,
            rebase_original_head_hint,
        };
        trace_debug_lifecycle(&format!(
            "trace normalizer start sid={} argv={:?} worktree={:?}",
            root_sid, pending.raw_argv, pending.worktree
        ));
        self.state.pending.insert(root_sid.to_string(), pending);
        if let Some(prestart_cmd_name) = self.state.prestart_root_cmd_names.remove(root_sid)
            && let Some(pending) = self.state.pending.get_mut(root_sid)
            && pending.root_cmd_name.is_none()
        {
            pending.root_cmd_name = Some(prestart_cmd_name);
        }
        if let Some(deferred) = self.state.deferred_exits.remove(root_sid) {
            if let Some(pre_repo) = deferred.pre_repo
                && let Some(pending) = self.state.pending.get_mut(root_sid)
                && pending.pre_repo.is_none()
            {
                pending.pre_repo = Some(pre_repo);
            }
            if let Some(post_repo) = deferred.post_repo
                && let Some(pending) = self.state.pending.get_mut(root_sid)
            {
                pending.post_repo = Some(post_repo);
            }
            self.merge_pending_worktree_head_offsets(
                root_sid,
                deferred.worktree_head_start_offset,
                deferred.worktree_head_end_offset,
            );
            self.merge_pending_family_reflog_cuts(
                root_sid,
                deferred.reflog_start_cut,
                deferred.reflog_end_cut,
            );
            self.merge_pending_ref_changes(root_sid, deferred.captured_ref_changes);
            self.merge_pending_merge_squash_source_head(
                root_sid,
                deferred.merge_squash_source_head,
            );
            self.merge_pending_carryover_snapshot_id(root_sid, deferred.carryover_snapshot_id);
            return self.finalize_root_exit(root_sid, deferred.exit_code, deferred.finished_at_ns);
        }

        Ok(None)
    }

    fn handle_def_param(
        &mut self,
        payload: &Value,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        if let Some(param) = payload.get("param").and_then(Value::as_str)
            && param == "GIT_AI_WRAPPER_INVOCATION_ID"
            && let Some(value) = payload.get("value").and_then(Value::as_str)
            && !value.is_empty()
        {
            self.state
                .root_wrapper_invocation_id
                .insert(root_sid.to_string(), value.to_string());
        }
        Ok(None)
    }

    fn handle_def_repo(
        &mut self,
        payload: &Value,
        _sid: &str,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let payload_pre_repo = payload_repo_context(payload, "git_ai_pre_repo");
        let payload_worktree = payload_worktree(payload);
        let payload_repo = payload
            .get("repo")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .map(|repo| worktree_from_def_repo_repo(&repo).unwrap_or(repo))
            .map(|repo| worktree_root_for_path(&repo).unwrap_or(repo));

        let pending_worktree = self
            .state
            .pending
            .get(root_sid)
            .and_then(|pending| pending.worktree.clone());
        let prefer_def_repo_target = self
            .state
            .pending
            .get(root_sid)
            .and_then(|pending| argv_primary_command(&pending.raw_argv))
            .is_some_and(|command| matches!(command.as_str(), "clone" | "init"));

        // For clone/init the root process's def_repo carries the newly created
        // repo path.  Child processes (remote-https, index-pack, rev-list, …)
        // inherit the parent CWD and their def_repo reports that CWD — not the
        // clone destination.  Once we've captured the root def_repo (first
        // arrival), skip subsequent child def_repo events entirely to prevent
        // overwriting the correct worktree, family, and sid lookup maps.
        if prefer_def_repo_target {
            let already_saw_def_repo = self
                .state
                .pending
                .get(root_sid)
                .is_some_and(|pending| pending.saw_def_repo);
            if already_saw_def_repo {
                return Ok(None);
            }
        }

        // Trace2 `def_repo.repo` may point at a common-dir `.git` path for worktrees.
        // For normal in-repo commands we keep the start/cwd-derived worktree when available.
        // For clone/init the `def_repo` target is the repo we actually created and must win.
        let repo = if prefer_def_repo_target {
            payload_worktree
                .or(payload_repo)
                .or(pending_worktree)
                .ok_or_else(|| GitAiError::Generic("def_repo missing repo path".to_string()))?
        } else {
            payload_worktree
                .or(pending_worktree)
                .or(payload_repo)
                .ok_or_else(|| GitAiError::Generic("def_repo missing repo path".to_string()))?
        };
        let repo = worktree_root_for_path(&repo).unwrap_or(repo);

        self.state
            .sid_to_worktree
            .insert(root_sid.to_string(), repo.clone());

        let family = common_dir_for_repo_path(&repo).map(|common_dir| {
            FamilyKey::new(
                common_dir
                    .canonicalize()
                    .unwrap_or(common_dir)
                    .to_string_lossy()
                    .to_string(),
            )
        });
        if let Some(family) = family.as_ref() {
            self.state
                .sid_to_family
                .insert(root_sid.to_string(), family.clone());
        }
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            pending.saw_def_repo = true;
            pending.worktree = Some(repo);
            if let Some(family) = family.as_ref() {
                pending.family_key = Some(family.clone());
            }
            if pending.pre_repo.is_none()
                && let Some(pre_repo) = payload_pre_repo
            {
                pending.pre_repo = Some(pre_repo);
            }
        }
        self.refresh_pending_mutation_capture(root_sid)?;
        Ok(None)
    }

    fn handle_cmd_name(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let cmd = payload
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("cmd_name missing name".to_string()))?
            .to_string();

        if is_internal_cmd_name(&cmd) {
            return Ok(None);
        }

        if sid == root_sid {
            if let Some(pending) = self.state.pending.get_mut(root_sid) {
                pending.root_cmd_name = Some(cmd);
            } else {
                self.state
                    .prestart_root_cmd_names
                    .insert(root_sid.to_string(), cmd);
                return Ok(None);
            }
            self.refresh_pending_mutation_capture(root_sid)?;
            return Ok(None);
        }

        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            pending.observed_child_commands.push(cmd);
        }
        self.refresh_pending_mutation_capture(root_sid)?;
        Ok(None)
    }

    fn handle_exit(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
        finished_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        if sid != root_sid {
            let _ = payload;
            let _ = finished_at_ns;
            return Ok(None);
        }
        if self.is_completed_root(root_sid) {
            return Ok(None);
        }

        let exit_code = payload
            .get("code")
            .or_else(|| payload.get("exit_code"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        let payload_pre_repo = payload_repo_context(payload, "git_ai_pre_repo");
        let payload_post_repo = payload_repo_context(payload, "git_ai_post_repo");
        let (payload_head_start, payload_head_end) = payload_worktree_head_offsets(payload);
        let payload_ref_changes = payload_reflog_changes(payload);
        let payload_merge_squash_source_head =
            payload_string_field(payload, "git_ai_merge_squash_source_head");
        let payload_carryover_snapshot_id =
            payload_string_field(payload, "git_ai_carryover_snapshot_id");

        if !self.state.pending.contains_key(root_sid) {
            let (payload_reflog_start, payload_reflog_end) = payload_family_reflog_cuts(payload);
            let deferred = self
                .state
                .deferred_exits
                .entry(root_sid.to_string())
                .or_insert(DeferredRootExit {
                    exit_code,
                    finished_at_ns,
                    pre_repo: payload_pre_repo.clone(),
                    post_repo: payload_post_repo.clone(),
                    merge_squash_source_head: payload_merge_squash_source_head.clone(),
                    worktree_head_start_offset: payload_head_start,
                    worktree_head_end_offset: payload_head_end,
                    reflog_start_cut: payload_reflog_start.clone(),
                    reflog_end_cut: payload_reflog_end.clone(),
                    captured_ref_changes: payload_ref_changes.clone(),
                    carryover_snapshot_id: payload_carryover_snapshot_id.clone(),
                });
            deferred.exit_code = exit_code;
            if deferred.pre_repo.is_none() {
                deferred.pre_repo = payload_pre_repo;
            }
            if payload_post_repo.is_some() {
                deferred.post_repo = payload_post_repo;
            }
            if let Some(source_head) = payload_merge_squash_source_head {
                deferred.merge_squash_source_head = Some(source_head);
            }
            if finished_at_ns > deferred.finished_at_ns {
                deferred.finished_at_ns = finished_at_ns;
            }
            if let Some(start) = payload_head_start {
                match deferred.worktree_head_start_offset {
                    Some(current) if current <= start => {}
                    _ => deferred.worktree_head_start_offset = Some(start),
                }
            }
            if let Some(end) = payload_head_end {
                match deferred.worktree_head_end_offset {
                    Some(current) if current >= end => {}
                    _ => deferred.worktree_head_end_offset = Some(end),
                }
            }
            merge_reflog_cut(
                &mut deferred.reflog_start_cut,
                payload_reflog_start,
                MergeCutMode::Min,
            );
            merge_reflog_cut(
                &mut deferred.reflog_end_cut,
                payload_reflog_end,
                MergeCutMode::Max,
            );
            if payload_carryover_snapshot_id.is_some() {
                deferred.carryover_snapshot_id = payload_carryover_snapshot_id;
            }
            for change in payload_ref_changes {
                let duplicate = deferred.captured_ref_changes.iter().any(|existing| {
                    existing.reference == change.reference
                        && existing.old == change.old
                        && existing.new == change.new
                });
                if !duplicate {
                    deferred.captured_ref_changes.push(change);
                }
            }
            trace_debug_lifecycle(&format!(
                "trace normalizer deferred exit sid={} code={} (start not seen yet)",
                root_sid, exit_code
            ));
            return Ok(None);
        }

        if let Some(pre_repo) = payload_pre_repo
            && let Some(pending) = self.state.pending.get_mut(root_sid)
            && pending.pre_repo.is_none()
        {
            pending.pre_repo = Some(pre_repo);
        }
        if let Some(post_repo) = payload_post_repo
            && let Some(pending) = self.state.pending.get_mut(root_sid)
        {
            pending.post_repo = Some(post_repo);
        }
        self.merge_pending_worktree_head_offsets(root_sid, payload_head_start, payload_head_end);
        self.merge_pending_ref_changes(root_sid, payload_ref_changes);
        self.merge_pending_carryover_snapshot_id(root_sid, payload_carryover_snapshot_id);
        trace_debug_lifecycle(&format!(
            "trace normalizer exit sid={} code={} pending_before_finalize={}",
            root_sid,
            exit_code,
            self.state.pending.len()
        ));

        self.finalize_root_exit(root_sid, exit_code, finished_at_ns)
    }

    fn finalize_root_exit(
        &mut self,
        root_sid: &str,
        exit_code: i32,
        finished_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let mut pending = self.state.pending.remove(root_sid).ok_or_else(|| {
            GitAiError::Generic("missing pending command at finalize".to_string())
        })?;

        pending.exit_code = Some(exit_code);
        pending.finished_at_ns = Some(finished_at_ns);

        if pending.worktree.is_none()
            && let Some(worktree) = self.state.sid_to_worktree.get(root_sid)
        {
            pending.worktree = Some(worktree.clone());
        }
        if pending.family_key.is_none()
            && let Some(family) = self.state.sid_to_family.get(root_sid)
        {
            pending.family_key = Some(family.clone());
        }
        if pending.family_key.is_none()
            && let Some(worktree) = pending.worktree.as_deref()
        {
            pending.family_key = common_dir_for_worktree(worktree).map(|common_dir| {
                FamilyKey::new(
                    common_dir
                        .canonicalize()
                        .unwrap_or(common_dir)
                        .to_string_lossy()
                        .to_string(),
                )
            });
        }

        let mut primary_command = self.resolve_primary_hint(
            pending.root_cmd_name.as_deref(),
            &pending.observed_child_commands,
            &pending.raw_argv,
            pending.worktree.as_deref(),
            pending.family_key.as_ref(),
        )?;
        let (invoked_command, invoked_args) =
            canonical_invocation(&pending.raw_argv, primary_command.as_deref());
        if primary_command.is_none() {
            primary_command = invoked_command.clone();
        }
        let may_mutate_refs = command_may_mutate_refs(primary_command.as_deref());

        let mut confidence = Confidence::Low;
        let mut ref_changes = pending.captured_ref_changes.clone();
        if let Some(family) = pending.family_key.as_ref()
            && may_mutate_refs
        {
            if !ref_changes.is_empty() {
                confidence = Confidence::High;
            } else if let Some(end) = pending.reflog_end_cut.as_ref() {
                let start_cut = pending.reflog_start_cut.as_ref();
                if let Some(start_cut) = start_cut {
                    ref_changes = self.backend.reflog_delta(family, start_cut, end)?;
                    confidence = Confidence::High;
                } else if matches!(primary_command.as_deref(), Some("clone" | "init")) {
                    confidence = Confidence::High;
                } else {
                    return Err(GitAiError::Generic(format!(
                        "missing reflog start cut for mutating command sid={} primary={:?} family={}",
                        pending.root_sid, primary_command, family
                    )));
                }
            } else if matches!(primary_command.as_deref(), Some("clone" | "init")) {
                // Clone/init can resolve into a family only after the repository exists at exit.
                // In that flow there is no stable pre-command reflog cut to diff against.
            } else {
                return Err(GitAiError::Generic(format!(
                    "missing reflog end cut for mutating command sid={} primary={:?} family={}",
                    pending.root_sid, primary_command, family
                )));
            }
        }

        if may_mutate_refs
            && let (Some(worktree), Some(start), Some(end)) = (
                pending.worktree.as_deref(),
                pending.worktree_head_start_offset,
                pending.worktree_head_end_offset,
            )
        {
            let head_changes = worktree_head_reflog_delta(worktree, start, end)?;
            for change in head_changes {
                let duplicate = ref_changes.iter().any(|existing| {
                    existing.reference == change.reference
                        && existing.old == change.old
                        && existing.new == change.new
                });
                if !duplicate {
                    ref_changes.push(change);
                }
            }
        }

        let mut family_key = pending.family_key.clone();
        let mut scope = if let Some(key) = family_key.clone() {
            CommandScope::Family(key)
        } else {
            CommandScope::Global
        };

        if exit_code == 0 && matches!(primary_command.as_deref(), Some("clone" | "init")) {
            let cwd_hint = pending.invocation_worktree.as_deref();
            let target_from_def_repo = pending
                .saw_def_repo
                .then(|| pending.worktree.clone())
                .flatten();
            let target_from_argv = if primary_command.as_deref() == Some("clone") {
                self.backend.clone_target(&pending.raw_argv, cwd_hint)
            } else {
                self.backend.init_target(&pending.raw_argv, cwd_hint)
            };

            let mut candidates = Vec::new();
            // Prefer the def_repo target — it comes from git's own trace2
            // event and is always an absolute path.  The argv-derived target
            // may be relative and resolve against an unrelated ancestor repo.
            if let Some(target) = target_from_def_repo.as_ref() {
                candidates.push(target.clone());
            }
            if let Some(target) = target_from_argv.as_ref() {
                let duplicate = candidates.iter().any(|existing| existing == target);
                if !duplicate {
                    candidates.push(target.clone());
                }
            }

            let mut resolved = false;
            let mut last_error: Option<(PathBuf, GitAiError)> = None;
            for candidate in candidates {
                if let Some(common_dir) = common_dir_for_repo_path(&candidate) {
                    let resolved_family = FamilyKey::new(
                        common_dir
                            .canonicalize()
                            .unwrap_or(common_dir)
                            .to_string_lossy()
                            .to_string(),
                    );
                    pending.worktree = Some(candidate);
                    family_key = Some(resolved_family.clone());
                    scope = CommandScope::Family(resolved_family);
                    resolved = true;
                    break;
                } else {
                    last_error = Some((
                        candidate.clone(),
                        GitAiError::Generic(format!(
                            "failed to resolve clone/init target family from filesystem: {}",
                            candidate.display()
                        )),
                    ));
                }
            }

            if !resolved {
                // Keep the best available worktree hint even when family resolution fails.
                if let Some(target) = target_from_def_repo.or(target_from_argv) {
                    pending.worktree = Some(target);
                }
                if let Some((target, error)) = last_error {
                    observability::log_error(
                        &error,
                        Some(serde_json::json!({
                            "component": "trace_normalizer",
                            "phase": "resolve_clone_or_init_target_family",
                            "root_sid": pending.root_sid,
                            "target": target,
                        })),
                    );
                }
            }
        }

        let inflight_rebase_original_head = pending
            .worktree
            .as_deref()
            .and_then(|worktree| pending_rebase_original_head_from_inflight(&self.state, worktree))
            .or(pending.rebase_original_head_hint.clone());
        let merge_squash_source_head = pending.merge_squash_source_head;

        let normalized = NormalizedCommand {
            scope,
            family_key,
            worktree: pending.worktree,
            root_sid: pending.root_sid,
            raw_argv: pending.raw_argv,
            primary_command,
            invoked_command,
            invoked_args,
            observed_child_commands: pending.observed_child_commands,
            exit_code,
            started_at_ns: pending.started_at_ns,
            finished_at_ns,
            pre_repo: pending.pre_repo,
            post_repo: pending.post_repo,
            inflight_rebase_original_head,
            merge_squash_source_head,
            carryover_snapshot_id: pending.carryover_snapshot_id,
            stash_target_oid: pending.stash_target_oid,
            ref_changes,
            confidence,
            wrapper_invocation_id: self.state.root_wrapper_invocation_id.remove(root_sid),
        };

        trace_debug_lifecycle(&format!(
            "trace normalizer finalized sid={} primary={:?} pending_after_finalize={}",
            root_sid,
            normalized.primary_command,
            self.state.pending.len()
        ));
        self.mark_completed_root(root_sid);
        let _ = self.state.sid_to_worktree.remove(root_sid);
        let _ = self.state.sid_to_family.remove(root_sid);
        let _ = self.state.prestart_root_cmd_names.remove(root_sid);

        Ok(Some(normalized))
    }
}

fn trace_debug_lifecycle(message: &str) {
    if std::env::var("GIT_AI_DEBUG_DAEMON_TRACE").is_ok() {
        eprintln!("\u{1b}[1;33m[git-ai]\u{1b}[0m {}", message);
    }
}

fn is_valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c == '0')
}

fn worktree_head_reflog_delta(
    worktree: &Path,
    start_offset: u64,
    end_offset: u64,
) -> Result<Vec<RefChange>, GitAiError> {
    if end_offset < start_offset {
        return Err(GitAiError::Generic(format!(
            "worktree HEAD reflog cut regressed ({} < {})",
            end_offset, start_offset
        )));
    }
    if end_offset == start_offset {
        return Ok(Vec::new());
    }

    let path = git_dir_for_worktree(worktree)
        .ok_or_else(|| {
            GitAiError::Generic(format!(
                "missing gitdir for worktree while reading HEAD reflog: {}",
                worktree.display()
            ))
        })?
        .join("logs")
        .join("HEAD");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() < end_offset {
        return Err(GitAiError::Generic(format!(
            "worktree HEAD reflog shorter than cut ({} < {}) at {}",
            metadata.len(),
            end_offset,
            path.display()
        )));
    }

    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let mut file = fs::File::open(&path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let reader = BufReader::new(file.take(end_offset.saturating_sub(start_offset)));
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let head = line.split('\t').next().unwrap_or_default();
        let mut parts = head.split_whitespace();
        let Some(old) = parts.next().map(str::trim) else {
            continue;
        };
        let Some(new) = parts.next().map(str::trim) else {
            continue;
        };
        if !is_valid_oid(old) || !is_valid_oid(new) || old == new {
            continue;
        }
        out.push(RefChange {
            reference: "HEAD".to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
    Ok(out)
}

fn payload_worktree_head_offsets(payload: &Value) -> (Option<u64>, Option<u64>) {
    let start = payload
        .get("git_ai_worktree_head_reflog_start")
        .and_then(Value::as_u64);
    let end = payload
        .get("git_ai_worktree_head_reflog_end")
        .and_then(Value::as_u64);
    (start, end)
}

fn payload_reflog_cut(payload: &Value, key: &str) -> Option<ReflogCut> {
    let object = payload.get(key)?.as_object()?;
    let mut offsets = HashMap::with_capacity(object.len());
    for (reference, value) in object {
        let offset = value.as_u64()?;
        offsets.insert(reference.clone(), offset);
    }
    Some(ReflogCut { offsets })
}

fn payload_family_reflog_cuts(payload: &Value) -> (Option<ReflogCut>, Option<ReflogCut>) {
    (
        payload_reflog_cut(payload, "git_ai_family_reflog_start"),
        payload_reflog_cut(payload, "git_ai_family_reflog_end"),
    )
}

fn payload_reflog_changes(payload: &Value) -> Vec<RefChange> {
    payload
        .get("git_ai_family_reflog_changes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<RefChange>(item.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn payload_repo_context(payload: &Value, key: &str) -> Option<RepoContext> {
    serde_json::from_value(payload.get(key)?.clone()).ok()
}

fn payload_string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[derive(Clone, Copy)]
enum MergeCutMode {
    Min,
    Max,
}

fn merge_reflog_cut(
    target: &mut Option<ReflogCut>,
    incoming: Option<ReflogCut>,
    mode: MergeCutMode,
) {
    let Some(incoming) = incoming else {
        return;
    };
    let existing = target.get_or_insert_with(ReflogCut::default);
    for (reference, offset) in incoming.offsets {
        match existing.offsets.get_mut(&reference) {
            Some(current) => match mode {
                MergeCutMode::Min => {
                    if offset < *current {
                        *current = offset;
                    }
                }
                MergeCutMode::Max => {
                    if offset > *current {
                        *current = offset;
                    }
                }
            },
            None => {
                existing.offsets.insert(reference, offset);
            }
        }
    }
}

fn payload_timestamp_ns(payload: &Value) -> Result<u128, GitAiError> {
    if let Some(time) = payload
        .get("ts")
        .or_else(|| payload.get("time"))
        .or_else(|| payload.get("time_ns"))
        .and_then(Value::as_u64)
    {
        return Ok(time as u128);
    }
    if let Some(seconds) = payload.get("t_abs").and_then(Value::as_f64) {
        return Ok((seconds * 1_000_000_000_f64) as u128);
    }
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

fn payload_argv(payload: &Value) -> Vec<String> {
    payload
        .get("argv")
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn payload_worktree(payload: &Value) -> Option<PathBuf> {
    payload
        .get("worktree")
        .or_else(|| payload.get("repo_working_dir"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|path| worktree_root_for_path(&path).unwrap_or(path))
}

fn payload_cwd(payload: &Value) -> Option<PathBuf> {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|path| worktree_root_for_path(&path).unwrap_or(path))
}

fn worktree_from_def_repo_repo(repo: &Path) -> Option<PathBuf> {
    if repo.file_name().and_then(|name| name.to_str()) == Some(".git") {
        return repo.parent().map(PathBuf::from);
    }

    let linked_gitdir = repo.join("gitdir");
    if linked_gitdir.is_file() {
        let content = fs::read_to_string(&linked_gitdir).ok()?;
        let path = PathBuf::from(content.trim());
        if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
            return path.parent().map(PathBuf::from);
        }
    }

    None
}

fn trace_argv_has_executable_prefix(argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    let file_name = std::path::Path::new(first)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(first);
    file_name.eq_ignore_ascii_case("git") || file_name.eq_ignore_ascii_case("git.exe")
}

fn trace_argv_invocation_tokens(argv: &[String]) -> &[String] {
    if trace_argv_has_executable_prefix(argv) {
        &argv[1..]
    } else {
        argv
    }
}

fn canonical_invocation(
    raw_argv: &[String],
    primary_command: Option<&str>,
) -> (Option<String>, Vec<String>) {
    let tokens = trace_argv_invocation_tokens(raw_argv);
    let parsed = parse_git_cli_args(tokens);
    if let Some(command) = parsed.command {
        return (Some(command), parsed.command_args);
    }
    if let Some(command) = primary_command.filter(|value| !value.trim().is_empty()) {
        return (
            Some(command.to_string()),
            args_after_command(tokens, command),
        );
    }
    (None, Vec::new())
}

fn args_after_command(argv: &[String], command: &str) -> Vec<String> {
    argv.iter()
        .position(|arg| arg == command)
        .and_then(|idx| argv.get(idx + 1..))
        .map(|args| args.to_vec())
        .unwrap_or_default()
}

fn root_sid(sid: &str) -> &str {
    sid.split('/').next().unwrap_or(sid)
}

fn is_internal_cmd_name(name: &str) -> bool {
    name.starts_with("_run_")
}

fn worktree_from_argv(argv: &[String]) -> Option<PathBuf> {
    let mut idx = 0;
    while idx < argv.len() {
        if argv[idx] == "-C" && idx + 1 < argv.len() {
            let path = PathBuf::from(argv[idx + 1].clone());
            return Some(worktree_root_for_path(&path).unwrap_or(path));
        }
        idx += 1;
    }
    None
}

fn argv_primary_command(argv: &[String]) -> Option<String> {
    let mut idx = 0;
    if argv.first().map(|v| is_git_binary(v)).unwrap_or(false) {
        idx = 1;
    }
    while idx < argv.len() {
        let token = argv[idx].as_str();
        if token == "-C" {
            idx += 2;
            continue;
        }
        if takes_value_option(token) {
            idx += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn is_git_binary(token: &str) -> bool {
    if token == "git" || token == "git.exe" {
        return true;
    }
    std::path::Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "git" || name == "git.exe")
        .unwrap_or(false)
}

fn takes_value_option(token: &str) -> bool {
    matches!(
        token,
        "-c" | "--config-env"
            | "--git-dir"
            | "--work-tree"
            | "--namespace"
            | "--super-prefix"
            | "--exec-path"
            | "--worktree-attributes"
            | "--attr-source"
    )
}

fn command_may_mutate_refs(primary_command: Option<&str>) -> bool {
    matches!(
        primary_command,
        Some(
            "cherry-pick"
                | "checkout"
                | "clone"
                | "commit"
                | "fetch"
                | "init"
                | "merge"
                | "pull"
                | "push"
                | "rebase"
                | "reset"
                | "stash"
                | "switch"
        )
    )
}

fn pending_is_non_control_rebase(pending: &PendingTraceCommand) -> bool {
    let primary = select_primary_command(
        pending.root_cmd_name.as_deref(),
        &pending.observed_child_commands,
        &pending.raw_argv,
    );
    if primary.as_deref() != Some("rebase") {
        return false;
    }
    let (_invoked_command, invoked_args) =
        canonical_invocation(&pending.raw_argv, primary.as_deref());
    !rebase_has_control_mode(&invoked_args)
}

fn pending_rebase_original_head_from_inflight(
    state: &TraceNormalizerState,
    worktree: &Path,
) -> Option<String> {
    let target = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    state
        .pending
        .values()
        .filter_map(|pending| {
            let pending_worktree = pending
                .worktree
                .as_deref()
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
            Some((pending, pending_worktree?))
        })
        .filter(|(_, pending_worktree)| *pending_worktree == target)
        .filter(|(pending, _)| pending_is_non_control_rebase(pending))
        .filter_map(|(pending, _)| {
            pending
                .pre_repo
                .as_ref()
                .and_then(|repo| repo.head.clone())
                .filter(|head| is_valid_oid(head) && !is_zero_oid(head))
                .map(|head| (pending.started_at_ns, head))
        })
        .min_by_key(|(started_at_ns, _)| *started_at_ns)
        .map(|(_, head)| head)
}

fn resolve_rebase_branch_head_hint(family: &FamilyKey, branch_spec: &str) -> Option<String> {
    if is_valid_oid(branch_spec) && !is_zero_oid(branch_spec) {
        return Some(branch_spec.to_string());
    }
    let ref_name = if branch_spec.starts_with("refs/") {
        branch_spec.to_string()
    } else {
        format!("refs/heads/{}", branch_spec)
    };
    read_ref_oid_for_common_dir(&PathBuf::from(&family.0), &ref_name)
        .filter(|oid| is_valid_oid(oid) && !is_zero_oid(oid))
}

fn select_primary_command(
    root_cmd_name: Option<&str>,
    observed_child_commands: &[String],
    argv: &[String],
) -> Option<String> {
    if let Some(name) = root_cmd_name
        && !is_internal_cmd_name(name)
        && !is_git_binary(name)
    {
        return Some(name.to_string());
    }

    for child in observed_child_commands {
        if !is_internal_cmd_name(child) && !is_git_binary(child) {
            return Some(child.clone());
        }
    }

    argv_primary_command(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::RefChange;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    fn normalize_path_key_from_str(path: &str) -> String {
        PathBuf::from(path).to_string_lossy().replace('\\', "/")
    }

    fn normalize_path_key(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    #[derive(Default)]
    struct MockBackend {
        family_by_worktree: Mutex<HashMap<String, FamilyKey>>,
        context_by_worktree: Mutex<HashMap<String, RepoContext>>,
        alias_by_worktree_command: Mutex<HashMap<String, HashMap<String, String>>>,
    }

    impl MockBackend {
        fn set_family(&self, worktree: &str, family: &str) {
            self.family_by_worktree.lock().unwrap().insert(
                normalize_path_key_from_str(worktree),
                FamilyKey::new(family.to_string()),
            );
        }

        fn set_context(&self, worktree: &str, head: &str) {
            self.context_by_worktree.lock().unwrap().insert(
                normalize_path_key_from_str(worktree),
                RepoContext {
                    head: Some(head.to_string()),
                    branch: Some("main".to_string()),
                    detached: false,
                },
            );
        }

        fn set_alias(&self, worktree: &str, alias: &str, target_command: &str) {
            self.alias_by_worktree_command
                .lock()
                .unwrap()
                .entry(normalize_path_key_from_str(worktree))
                .or_default()
                .insert(alias.to_string(), target_command.to_string());
        }
    }

    impl GitBackend for MockBackend {
        fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError> {
            self.family_by_worktree
                .lock()
                .unwrap()
                .get(&normalize_path_key(worktree))
                .cloned()
                .ok_or_else(|| GitAiError::Generic("family not found".to_string()))
        }

        fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError> {
            self.context_by_worktree
                .lock()
                .unwrap()
                .get(&normalize_path_key(worktree))
                .cloned()
                .ok_or_else(|| GitAiError::Generic("context not found".to_string()))
        }

        fn reflog_cut(&self, _family: &FamilyKey) -> Result<ReflogCut, GitAiError> {
            Ok(ReflogCut {
                offsets: HashMap::new(),
            })
        }

        fn reflog_delta(
            &self,
            _family: &FamilyKey,
            _start: &ReflogCut,
            _end: &ReflogCut,
        ) -> Result<Vec<RefChange>, GitAiError> {
            Ok(vec![])
        }

        fn resolve_primary_command(
            &self,
            worktree: &Path,
            argv: &[String],
        ) -> Result<Option<String>, GitAiError> {
            let raw = argv_primary_command(argv);
            let Some(command) = raw else {
                return Ok(None);
            };
            let worktree_key = normalize_path_key(worktree);
            let resolved = self
                .alias_by_worktree_command
                .lock()
                .unwrap()
                .get(&worktree_key)
                .and_then(|commands| commands.get(&command))
                .cloned()
                .unwrap_or(command);
            Ok(Some(resolved))
        }

        fn clone_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            let tokens: &[String] = if _argv
                .first()
                .is_some_and(|value| value == "git" || value == "git.exe")
            {
                &_argv[1..]
            } else {
                _argv
            };
            let parsed = parse_git_cli_args(tokens);
            if parsed.command.as_deref() != Some("clone") {
                return None;
            }

            let args = parsed.command_args;
            let mut positional = Vec::new();
            let mut idx = 0;
            while idx < args.len() {
                let arg = &args[idx];
                if arg == "--" {
                    positional.extend(args[idx + 1..].iter().cloned());
                    break;
                }
                if arg.starts_with('-') {
                    let takes_value = matches!(
                        arg.as_str(),
                        "-b" | "--branch"
                            | "--origin"
                            | "--upload-pack"
                            | "--template"
                            | "--separate-git-dir"
                            | "--reference"
                            | "--dissociate"
                            | "--config"
                            | "--object-format"
                    );
                    if takes_value && idx + 1 < args.len() {
                        idx += 2;
                        continue;
                    }
                    idx += 1;
                    continue;
                }
                positional.push(arg.clone());
                idx += 1;
            }
            if positional.is_empty() {
                return None;
            }
            let target = if positional.len() >= 2 {
                PathBuf::from(&positional[1])
            } else {
                let source = positional[0].trim_end_matches('/');
                let source = source.strip_suffix(".git").unwrap_or(source);
                let name = source.rsplit('/').next()?.rsplit(':').next()?.to_string();
                if name.is_empty() {
                    return None;
                }
                PathBuf::from(name)
            };
            Some(if target.is_absolute() {
                target
            } else if let Some(cwd) = _cwd_hint {
                cwd.join(target)
            } else {
                target
            })
        }

        fn init_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            let tokens: &[String] = if _argv
                .first()
                .is_some_and(|value| value == "git" || value == "git.exe")
            {
                &_argv[1..]
            } else {
                _argv
            };
            let parsed = parse_git_cli_args(tokens);
            if parsed.command.as_deref() != Some("init") {
                return None;
            }

            let args = parsed.command_args;
            let mut positional = Vec::new();
            let mut idx = 0;
            while idx < args.len() {
                let arg = &args[idx];
                if arg == "--" {
                    positional.extend(args[idx + 1..].iter().cloned());
                    break;
                }
                if arg.starts_with('-') {
                    idx += 1;
                    continue;
                }
                positional.push(arg.clone());
                idx += 1;
            }
            let target = positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            Some(if target.is_absolute() {
                target
            } else if let Some(cwd) = _cwd_hint {
                cwd.join(target)
            } else {
                target
            })
        }
    }

    fn payload(event: &str, sid: &str, ts: u64) -> Value {
        serde_json::json!({
            "event": event,
            "sid": sid,
            "ts": ts,
        })
    }

    #[test]
    fn normalizer_emits_one_command_for_start_exit() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s1",
            "ts":1,
            "argv":["git","status"],
            "worktree":"/repo"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s1",
            "ts":2,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s1");
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
        assert_eq!(cmd.exit_code, 0);
    }

    #[test]
    fn normalizer_uses_atexit_when_exit_is_missing() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s1-atexit",
            "ts":1,
            "argv":["git","status"],
            "worktree":"/repo"
        });
        let atexit = serde_json::json!({
            "event":"atexit",
            "sid":"s1-atexit",
            "ts":2,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&atexit).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s1-atexit");
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
        assert_eq!(cmd.exit_code, 0);
    }

    #[test]
    fn completed_root_retention_does_not_clear_all_recent_roots() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);

        normalizer.mark_completed_root_with_limit("root-a", 3);
        normalizer.mark_completed_root_with_limit("root-b", 3);
        normalizer.mark_completed_root_with_limit("root-c", 3);

        let late_payload = serde_json::json!({
            "event":"atexit",
            "sid":"root-a",
            "ts":10,
            "code":0
        });
        assert!(
            normalizer.ingest_payload(&late_payload).unwrap().is_none(),
            "late payloads for recently completed roots should stay ignored"
        );

        normalizer.mark_completed_root_with_limit("root-d", 3);
        assert_eq!(normalizer.state.completed_roots.len(), 3);
        assert!(normalizer.state.completed_roots.contains("root-b"));
        assert!(normalizer.state.completed_roots.contains("root-c"));
        assert!(normalizer.state.completed_roots.contains("root-d"));
        assert!(!normalizer.state.completed_roots.contains("root-a"));
        assert_eq!(normalizer.state.completed_root_order.len(), 3);
    }

    #[test]
    fn alias_commit_captures_mutation_state_at_start() {
        let backend = Arc::new(MockBackend::default());
        let temp = tempfile::tempdir().expect("create tempdir");
        let worktree = temp.path().join("repo");
        fs::create_dir_all(worktree.join(".git")).expect("create git dir");
        backend.set_alias(worktree.to_str().expect("utf8 worktree"), "ci", "commit");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"alias-commit",
            "ts":1,
            "argv":["git","ci","-m","msg"],
            "worktree":worktree,
            "git_ai_family_reflog_start": {"HEAD": 10}
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"alias-commit",
            "ts":2,
            "code":0,
            "git_ai_family_reflog_end": {"HEAD": 11}
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        let pending = normalizer
            .state()
            .pending
            .get("alias-commit")
            .expect("pending alias command");
        assert!(pending.reflog_start_cut.is_some());

        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.primary_command.as_deref(), Some("commit"));
    }

    #[test]
    fn normalizer_errors_on_exit_without_start() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s2",
            "ts":10,
            "code":0
        });
        let start = serde_json::json!({
            "event":"start",
            "sid":"s2",
            "ts":1,
            "argv":["git","status"],
            "worktree":"/repo"
        });

        assert!(normalizer.ingest_payload(&exit).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&start).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s2");
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
        assert_eq!(cmd.exit_code, 0);
    }

    #[test]
    fn child_cmd_name_enriches_root() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s3",
            "ts":1,
            "argv":["git","foo"],
            "worktree":"/repo"
        });
        let child = serde_json::json!({
            "event":"cmd_name",
            "sid":"s3/child1",
            "ts":2,
            "name":"status"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s3",
            "ts":3,
            "code":0
        });

        normalizer.ingest_payload(&start).unwrap();
        normalizer.ingest_payload(&child).unwrap();
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.observed_child_commands, vec!["status".to_string()]);
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
    }

    #[test]
    fn child_exit_does_not_finalize_without_root_exit() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s-exec",
            "ts":1,
            "argv":["git","notes","show","abc123"],
            "worktree":"/repo"
        });
        let cmd_name = serde_json::json!({
            "event":"cmd_name",
            "sid":"s-exec",
            "ts":2,
            "name":"notes"
        });
        let exec = serde_json::json!({
            "event":"exec",
            "sid":"s-exec",
            "ts":3,
            "argv":["git","show","def456"]
        });
        let child_exit = serde_json::json!({
            "event":"exit",
            "sid":"s-exec/child",
            "ts":4,
            "code":0
        });
        let root_exit = serde_json::json!({
            "event":"exit",
            "sid":"s-exec",
            "ts":5,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&cmd_name).unwrap().is_none());
        assert!(normalizer.ingest_payload(&exec).unwrap().is_none());
        assert!(normalizer.ingest_payload(&child_exit).unwrap().is_none());
        assert_eq!(normalizer.state().pending.len(), 1);

        let cmd = normalizer.ingest_payload(&root_exit).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s-exec");
        assert_eq!(cmd.primary_command.as_deref(), Some("notes"));
        assert_eq!(cmd.exit_code, 0);
        assert!(normalizer.state().pending.is_empty());
    }

    #[test]
    fn child_exit_before_root_exec_is_ignored_until_root_exit() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s-exec-oop",
            "ts":1,
            "argv":["git","notes","show","abc123"],
            "worktree":"/repo"
        });
        let cmd_name = serde_json::json!({
            "event":"cmd_name",
            "sid":"s-exec-oop",
            "ts":2,
            "name":"notes"
        });
        let child_exit = serde_json::json!({
            "event":"exit",
            "sid":"s-exec-oop/child",
            "ts":3,
            "code":0
        });
        let exec = serde_json::json!({
            "event":"exec",
            "sid":"s-exec-oop",
            "ts":4,
            "argv":["git","show","def456"]
        });
        let root_exit = serde_json::json!({
            "event":"exit",
            "sid":"s-exec-oop",
            "ts":5,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&cmd_name).unwrap().is_none());
        assert!(normalizer.ingest_payload(&child_exit).unwrap().is_none());
        assert_eq!(normalizer.state().pending.len(), 1);

        assert!(normalizer.ingest_payload(&exec).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&root_exit).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s-exec-oop");
        assert_eq!(cmd.primary_command.as_deref(), Some("notes"));
        assert_eq!(cmd.exit_code, 0);
        assert!(normalizer.state().pending.is_empty());
    }

    #[test]
    fn clone_relative_target_falls_back_to_argv_target_when_def_repo_candidate_fails() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let outer = temp.path().join("outer");
        let clone_dir = outer.join("nested").join("relative-clone");
        fs::create_dir_all(clone_dir.join(".git")).expect("create clone git dir");

        let start = serde_json::json!({
            "event":"start",
            "sid":"clone-rel",
            "ts":1,
            "argv":["git","clone","ssh://example/repo.git","nested/relative-clone"],
            "worktree":outer
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"clone-rel",
            "ts":2,
            "repo":clone_dir.join(".git")
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"clone-rel",
            "ts":3,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();

        assert_eq!(cmd.primary_command.as_deref(), Some("clone"));
        assert_eq!(cmd.worktree.as_ref(), Some(&clone_dir));
        assert!(matches!(cmd.scope, CommandScope::Family(_)));
    }

    #[test]
    fn clone_with_late_family_resolution_does_not_error_without_reflog_start_cut() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let outer = temp.path().join("outer");
        let clone_dir = outer.join("nested").join("relative-clone");
        fs::create_dir_all(clone_dir.parent().expect("clone parent")).expect("create clone parent");

        let start = serde_json::json!({
            "event":"start",
            "sid":"clone-late-family",
            "ts":1,
            "argv":["git","clone","ssh://example/repo.git","nested/relative-clone"],
            "worktree":outer
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"clone-late-family",
            "ts":2,
            "worktree":clone_dir
        });
        let cmd_name = serde_json::json!({
            "event":"cmd_name",
            "sid":"clone-late-family",
            "ts":3,
            "name":"clone"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"clone-late-family",
            "ts":4,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        assert!(normalizer.ingest_payload(&cmd_name).unwrap().is_none());

        // Simulate repo discoverability only once clone is about to exit.
        fs::create_dir_all(clone_dir.join(".git")).expect("create clone git dir");

        let cmd = normalizer
            .ingest_payload(&exit)
            .expect("clone finalize should not error")
            .expect("clone should emit a normalized command");

        assert_eq!(cmd.primary_command.as_deref(), Some("clone"));
        assert!(matches!(cmd.scope, CommandScope::Family(_)));
    }

    #[test]
    fn clone_prefers_target_family_over_source_cwd_family() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let source_repo = temp.path().join("source-repo");
        let cloned_repo = temp.path().join("cloned-repo");
        fs::create_dir_all(source_repo.join(".git")).expect("create source git dir");
        fs::create_dir_all(cloned_repo.join(".git")).expect("create cloned git dir");

        let start = serde_json::json!({
            "event":"start",
            "sid":"clone-source-cwd",
            "ts":1,
            "argv":["git","clone","ssh://example/repo.git",cloned_repo],
            "worktree":source_repo
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"clone-source-cwd",
            "ts":2,
            "repo":cloned_repo.join(".git")
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"clone-source-cwd",
            "ts":3,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();

        assert_eq!(cmd.primary_command.as_deref(), Some("clone"));
        assert_eq!(cmd.worktree.as_ref(), Some(&cloned_repo));
        let expected_family = cloned_repo
            .join(".git")
            .canonicalize()
            .unwrap_or_else(|_| cloned_repo.join(".git"));
        assert_eq!(
            cmd.family_key.as_ref().map(|family| family.0.as_str()),
            Some(expected_family.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn clone_child_def_repo_does_not_overwrite_root_worktree() {
        // Real git trace2 output shows child processes (remote-https, index-pack)
        // emit def_repo with the CWD as worktree, not the clone destination.
        // The root process's def_repo has the correct newly-created repo path.
        // Verify that child def_repo events don't clobber the root's worktree.
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let cwd = temp.path().join("projects"); // non-repo CWD
        let clone_dest = cwd.join("testing-git"); // the clone destination
        fs::create_dir_all(clone_dest.join(".git")).expect("create clone git dir");

        let root_sid = "20260327T000000.000000Z-Hdeadbeef-P00010000";
        let child_sid = format!("{}/20260327T000000.000001Z-Hdeadbeef-P00010001", root_sid);

        let start = serde_json::json!({
            "event": "start",
            "sid": root_sid,
            "ts": 1,
            "argv": ["git", "clone", "https://github.com/svarlamov/testing-git"]
            // No worktree or cwd — matches real trace2 start from non-repo dir
        });
        // Root def_repo: correct clone destination
        let root_def_repo = serde_json::json!({
            "event": "def_repo",
            "sid": root_sid,
            "ts": 2,
            "worktree": clone_dest
        });
        // Child def_repo from remote-https: reports CWD (parent), not destination
        let child_def_repo = serde_json::json!({
            "event": "def_repo",
            "sid": child_sid,
            "ts": 3,
            "worktree": cwd
        });
        let exit = serde_json::json!({
            "event": "exit",
            "sid": root_sid,
            "ts": 4,
            "code": 0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&root_def_repo).unwrap().is_none());
        // Child def_repo must NOT overwrite the root worktree
        assert!(
            normalizer
                .ingest_payload(&child_def_repo)
                .unwrap()
                .is_none()
        );

        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.primary_command.as_deref(), Some("clone"));
        assert_eq!(
            cmd.worktree.as_ref(),
            Some(&clone_dest),
            "clone worktree should be the destination, not the parent CWD"
        );
    }

    #[test]
    fn no_repo_routes_to_global_scope() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s4",
            "ts":1,
            "argv":["git","version"]
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s4",
            "ts":2,
            "code":0
        });

        normalizer.ingest_payload(&start).unwrap();
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert!(matches!(cmd.scope, CommandScope::Global));
    }

    #[test]
    fn ignores_non_supported_trace_events() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let p = payload("region_enter", "s5", 1);
        assert!(normalizer.ingest_payload(&p).unwrap().is_none());
    }

    #[test]
    fn interleaved_roots_with_out_of_order_exits_finalize_independently() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let repo_a = temp.path().join("repo-a");
        let repo_b = temp.path().join("repo-b");
        fs::create_dir_all(repo_a.join(".git")).expect("create repo-a git dir");
        fs::create_dir_all(repo_b.join(".git")).expect("create repo-b git dir");

        let start_a = serde_json::json!({
            "event":"start",
            "sid":"s-a",
            "ts":1,
            "argv":["git","commit","-m","a"],
            "worktree":repo_a,
            "git_ai_family_reflog_start": {"HEAD": 100}
        });
        let start_b = serde_json::json!({
            "event":"start",
            "sid":"s-b",
            "ts":2,
            "argv":["git","push","origin","main"],
            "worktree":repo_b,
            "git_ai_family_reflog_start": {"HEAD": 200}
        });
        let exit_b = serde_json::json!({
            "event":"exit",
            "sid":"s-b",
            "ts":3,
            "code":0,
            "git_ai_family_reflog_end": {"HEAD": 201}
        });
        let exit_a = serde_json::json!({
            "event":"exit",
            "sid":"s-a",
            "ts":4,
            "code":0,
            "git_ai_family_reflog_end": {"HEAD": 101}
        });

        assert!(normalizer.ingest_payload(&start_a).unwrap().is_none());
        assert!(normalizer.ingest_payload(&start_b).unwrap().is_none());

        let cmd_b = normalizer.ingest_payload(&exit_b).unwrap().unwrap();
        assert_eq!(cmd_b.root_sid, "s-b");
        assert_eq!(cmd_b.primary_command.as_deref(), Some("push"));
        assert_eq!(cmd_b.worktree.as_deref(), Some(repo_b.as_path()));
        assert!(matches!(cmd_b.scope, CommandScope::Family(_)));

        let cmd_a = normalizer.ingest_payload(&exit_a).unwrap().unwrap();
        assert_eq!(cmd_a.root_sid, "s-a");
        assert_eq!(cmd_a.primary_command.as_deref(), Some("commit"));
        assert_eq!(cmd_a.worktree.as_deref(), Some(repo_a.as_path()));
        assert!(matches!(cmd_a.scope, CommandScope::Family(_)));

        assert!(normalizer.state().pending.is_empty());
    }

    #[test]
    fn start_ignores_repo_gitdir_hint_and_uses_cwd_for_worktree_resolution() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend.clone());
        let temp = tempfile::tempdir().expect("create tempdir");
        let repo_base = temp.path().join("repo-base");
        let common_git_dir = repo_base.join(".git");
        let worker_git_dir = common_git_dir.join("worktrees").join("worker-b");
        let worker_worktree = temp.path().join("repo-worker-b");
        let worker_head = "1111111111111111111111111111111111111111";
        fs::create_dir_all(common_git_dir.join("refs").join("heads"))
            .expect("create common refs/heads");
        fs::create_dir_all(&worker_git_dir).expect("create linked worktree git dir");
        fs::create_dir_all(&worker_worktree).expect("create worker worktree");
        fs::write(
            worker_worktree.join(".git"),
            format!("gitdir: {}\n", worker_git_dir.display()),
        )
        .expect("write linked worktree .git pointer");
        fs::write(worker_git_dir.join("HEAD"), "ref: refs/heads/worker-b\n")
            .expect("write worker HEAD");
        fs::write(
            common_git_dir.join("refs").join("heads").join("worker-b"),
            format!("{worker_head}\n"),
        )
        .expect("write worker branch ref");

        let start = serde_json::json!({
            "event":"start",
            "sid":"s-repo-field",
            "ts":1,
            "argv":["git","commit","-m","msg"],
            "repo":common_git_dir,
            "cwd":worker_worktree,
            "git_ai_pre_repo": {
                "head": worker_head,
                "branch": "worker-b",
                "detached": false
            },
            "git_ai_family_reflog_start": {"HEAD": 300}
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"s-repo-field",
            "ts":2,
            "repo":worker_git_dir
        });
        let cmd_name = serde_json::json!({
            "event":"cmd_name",
            "sid":"s-repo-field",
            "ts":3,
            "name":"commit"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s-repo-field",
            "ts":4,
            "code":0,
            "git_ai_family_reflog_end": {"HEAD": 301}
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        assert!(normalizer.ingest_payload(&cmd_name).unwrap().is_none());

        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(
            cmd.pre_repo.as_ref().and_then(|repo| repo.head.as_deref()),
            Some(worker_head)
        );
        assert!(cmd.post_repo.is_none());
        assert_eq!(cmd.worktree.as_deref(), Some(worker_worktree.as_path()));
    }

    #[test]
    fn stash_target_oid_can_arrive_after_start_on_def_repo() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("create git dir");

        let start = serde_json::json!({
            "event":"start",
            "sid":"stash-late-meta",
            "ts":1,
            "argv":["git","stash","pop"],
            "git_ai_family_reflog_start": {"refs/stash": 9}
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"stash-late-meta",
            "ts":2,
            "worktree":repo,
            "git_ai_stash_target_oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"stash-late-meta",
            "ts":3,
            "code":0,
            "git_ai_family_reflog_start": {"refs/stash": 9},
            "git_ai_family_reflog_end": {"refs/stash": 9}
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();

        assert_eq!(
            cmd.stash_target_oid.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn destructive_stash_can_normalize_without_pre_command_target_oid() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("create git dir");

        let start = serde_json::json!({
            "event":"start",
            "sid":"stash-missing-meta",
            "ts":1,
            "argv":["git","stash","pop"],
            "worktree":repo,
            "git_ai_family_reflog_start": {"refs/stash": 11}
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"stash-missing-meta",
            "ts":2,
            "code":0,
            "git_ai_family_reflog_end": {"refs/stash": 11}
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        let cmd = normalizer
            .ingest_payload(&exit)
            .expect("missing stash metadata should not block normalization")
            .expect("exit payload should emit a normalized command");
        assert!(cmd.stash_target_oid.is_none());
    }

    #[test]
    fn pre_repo_can_arrive_after_start_on_def_repo() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let temp = tempfile::tempdir().expect("create tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git/refs/heads")).expect("create git refs");
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        fs::write(
            repo.join(".git/refs/heads/main"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        )
        .expect("write main ref");

        let start = serde_json::json!({
            "event":"start",
            "sid":"pre-repo-def-repo",
            "ts":1,
            "argv":["git","status"]
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"pre-repo-def-repo",
            "ts":2,
            "worktree":repo,
            "git_ai_pre_repo": {
                "head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "branch":"main",
                "detached":false
            }
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"pre-repo-def-repo",
            "ts":3,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        assert!(normalizer.ingest_payload(&def_repo).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();

        assert_eq!(
            cmd.pre_repo.as_ref().and_then(|repo| repo.head.as_deref()),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            cmd.pre_repo
                .as_ref()
                .and_then(|repo| repo.branch.as_deref()),
            Some("main")
        );
    }
}
