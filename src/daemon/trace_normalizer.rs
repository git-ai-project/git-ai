use crate::daemon::domain::{
    CommandScope, Confidence, FamilyKey, NormalizedCommand, RefChange, RepoContext,
};
use crate::daemon::git_backend::{GitBackend, ReflogCut};
use crate::error::GitAiError;
use crate::git::cli_parser::parse_git_cli_args;
use crate::git::repo_state::{git_dir_for_worktree, resolve_squash_source_head_for_worktree};
use crate::observability;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PendingTraceCommand {
    pub root_sid: String,
    pub raw_argv: Vec<String>,
    pub root_cmd_name: Option<String>,
    pub observed_child_commands: Vec<String>,
    pub worktree: Option<PathBuf>,
    pub family_key: Option<FamilyKey>,
    pub started_at_ns: u128,
    pub exit_code: Option<i32>,
    pub finished_at_ns: Option<u128>,
    pub pre_repo: Option<RepoContext>,
    pub post_repo: Option<RepoContext>,
    pub reflog_start_cut: Option<ReflogCut>,
    pub reflog_end_cut: Option<ReflogCut>,
    pub captured_ref_changes: Vec<RefChange>,
    pub worktree_head_start_offset: Option<u64>,
    pub worktree_head_end_offset: Option<u64>,
    pub wrapper_mirror: bool,
    pub saw_def_repo: bool,
    pub rebase_original_head_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TraceNormalizerState {
    pub pending: HashMap<String, PendingTraceCommand>,
    pub deferred_exits: HashMap<String, DeferredRootExit>,
    pub completed_roots: std::collections::HashSet<String>,
    pub sid_to_worktree: HashMap<String, PathBuf>,
    pub sid_to_family: HashMap<String, FamilyKey>,
    pub prestart_root_cmd_names: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DeferredRootExit {
    pub exit_code: i32,
    pub finished_at_ns: u128,
    pub worktree_head_start_offset: Option<u64>,
    pub worktree_head_end_offset: Option<u64>,
    pub reflog_start_cut: Option<ReflogCut>,
    pub reflog_end_cut: Option<ReflogCut>,
    pub captured_ref_changes: Vec<RefChange>,
}

pub struct TraceNormalizer<B: GitBackend> {
    backend: Arc<B>,
    state: TraceNormalizerState,
}

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

    pub fn remove_pending_root(&mut self, root_sid: &str) -> Option<PendingTraceCommand> {
        let removed = self.state.pending.remove(root_sid);
        if removed.is_some() {
            let _ = self.state.sid_to_worktree.remove(root_sid);
            let _ = self.state.sid_to_family.remove(root_sid);
            let _ = self.state.prestart_root_cmd_names.remove(root_sid);
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

            let (Some(worktree), Some(family)) = (pending.worktree.as_deref(), pending.family_key.as_ref()) else {
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
        if self.state.completed_roots.contains(&root_sid) {
            return Ok(None);
        }
        let ts = payload_timestamp_ns(payload)?;
        let (payload_head_start, payload_head_end) = payload_worktree_head_offsets(payload);
        self.merge_pending_worktree_head_offsets(&root_sid, payload_head_start, payload_head_end);
        let (payload_reflog_start, payload_reflog_end) = payload_family_reflog_cuts(payload);
        self.merge_pending_family_reflog_cuts(&root_sid, payload_reflog_start, payload_reflog_end);
        self.merge_pending_ref_changes(&root_sid, payload_reflog_changes(payload));

        match event {
            "start" => self.handle_start(payload, sid, &root_sid, ts),
            "def_repo" => self.handle_def_repo(payload, sid, &root_sid),
            "cmd_name" => self.handle_cmd_name(payload, sid, &root_sid),
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
        if self.state.completed_roots.contains(root_sid) {
            return Ok(None);
        }

        let raw_argv = payload_argv(payload);
        let mut worktree = payload_worktree(payload)
            .or_else(|| worktree_from_argv(&raw_argv))
            .or_else(|| self.state.sid_to_worktree.get(root_sid).cloned());

        if worktree.is_none()
            && let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
        {
            worktree = Some(PathBuf::from(cwd));
        }

        let family_key = if let Some(worktree) = worktree.as_deref() {
            match self.backend.resolve_family(worktree) {
                Ok(family) => {
                    self.state
                        .sid_to_family
                        .insert(root_sid.to_string(), family.clone());
                    Some(family)
                }
                Err(_) => self.state.sid_to_family.get(root_sid).cloned(),
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
            && !args_have_rebase_control_mode(&invoked_args)
        {
            family_key
                .as_ref()
                .and_then(|family| {
                    explicit_rebase_branch_arg(&invoked_args)
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
        let pre_repo = if family_key.is_some()
            && let Some(worktree) = worktree.as_deref()
        {
            Some(self.backend.repo_context(worktree)?)
        } else {
            None
        };

        let wrapper_mirror = payload
            .get("wrapper_mirror")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let pending = PendingTraceCommand {
            root_sid: root_sid.to_string(),
            raw_argv,
            root_cmd_name: None,
            observed_child_commands: Vec::new(),
            worktree,
            family_key,
            started_at_ns,
            exit_code: None,
            finished_at_ns: None,
            pre_repo,
            post_repo: None,
            reflog_start_cut,
            reflog_end_cut: None,
            captured_ref_changes: Vec::new(),
            worktree_head_start_offset,
            worktree_head_end_offset: None,
            wrapper_mirror,
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
            return self.finalize_root_exit(root_sid, deferred.exit_code, deferred.finished_at_ns);
        }

        Ok(None)
    }

    fn handle_def_repo(
        &mut self,
        payload: &Value,
        _sid: &str,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let payload_worktree = payload
            .get("worktree")
            .or_else(|| payload.get("repo_working_dir"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let payload_repo = payload
            .get("repo")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .map(|repo| worktree_from_def_repo_repo(&repo).unwrap_or(repo));

        let pending_worktree = self
            .state
            .pending
            .get(root_sid)
            .and_then(|pending| pending.worktree.clone());

        // Trace2 `def_repo.repo` may point at a common-dir `.git` path for worktrees.
        // Keep the start/cwd-derived worktree when available and only fall back to `repo`
        // when we have no better working-directory signal.
        let repo = payload_worktree
            .or(pending_worktree)
            .or(payload_repo)
            .ok_or_else(|| GitAiError::Generic("def_repo missing repo path".to_string()))?;

        self.state
            .sid_to_worktree
            .insert(root_sid.to_string(), repo.clone());

        let family = self.backend.resolve_family(&repo).ok();
        if let Some(family) = family.as_ref() {
            self.state
                .sid_to_family
                .insert(root_sid.to_string(), family.clone());
        }
        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            pending.saw_def_repo = true;
            pending.worktree = Some(repo);
            if let Some(family) = family.as_ref()
                && pending.family_key.is_none()
            {
                pending.family_key = Some(family.clone());
            }
        }
        let pre_repo_capture_worktree = self.state.pending.get(root_sid).and_then(|pending| {
            if pending.pre_repo.is_none() && pending.family_key.is_some() {
                pending.worktree.clone()
            } else {
                None
            }
        });
        if let Some(worktree) = pre_repo_capture_worktree.as_deref() {
            let pre_repo = self.backend.repo_context(worktree)?;
            if let Some(pending) = self.state.pending.get_mut(root_sid)
                && pending.pre_repo.is_none()
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
        if self.state.completed_roots.contains(root_sid) {
            return Ok(None);
        }

        let exit_code = payload
            .get("code")
            .or_else(|| payload.get("exit_code"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        let (payload_head_start, payload_head_end) = payload_worktree_head_offsets(payload);
        let payload_ref_changes = payload_reflog_changes(payload);

        if !self.state.pending.contains_key(root_sid) {
            let (payload_reflog_start, payload_reflog_end) = payload_family_reflog_cuts(payload);
            let deferred = self
                .state
                .deferred_exits
                .entry(root_sid.to_string())
                .or_insert(DeferredRootExit {
                    exit_code,
                    finished_at_ns,
                    worktree_head_start_offset: payload_head_start,
                    worktree_head_end_offset: payload_head_end,
                    reflog_start_cut: payload_reflog_start.clone(),
                    reflog_end_cut: payload_reflog_end.clone(),
                    captured_ref_changes: payload_ref_changes.clone(),
                });
            deferred.exit_code = exit_code;
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

        self.merge_pending_worktree_head_offsets(root_sid, payload_head_start, payload_head_end);
        self.merge_pending_ref_changes(root_sid, payload_ref_changes);
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
            pending.family_key = self.backend.resolve_family(worktree).ok();
        }
        if pending.pre_repo.is_none()
            && pending.family_key.is_some()
            && let Some(worktree) = pending.worktree.as_deref()
        {
            pending.pre_repo = Some(self.backend.repo_context(worktree)?);
        }
        if pending.post_repo.is_none()
            && pending.family_key.is_some()
            && let Some(worktree) = pending.worktree.as_deref()
        {
            pending.post_repo = Some(self.backend.repo_context(worktree)?);
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
            let cwd_hint = pending.worktree.as_deref();
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
                match self.backend.resolve_family(&candidate) {
                    Ok(resolved_family) => {
                        pending.worktree = Some(candidate);
                        family_key = Some(resolved_family.clone());
                        scope = CommandScope::Family(resolved_family);
                        resolved = true;
                        break;
                    }
                    Err(error) => {
                        last_error = Some((candidate, error));
                    }
                }
            }

            if !resolved {
                // Keep the best available worktree hint even when family resolution fails.
                if let Some(target) = target_from_argv.or(target_from_def_repo) {
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

        let inflight_rebase_original_head = family_key
            .as_ref()
            .and_then(|family| pending_rebase_original_head_from_inflight(&self.state, family))
            .or(pending.rebase_original_head_hint.clone());
        let merge_squash_source_head = if exit_code == 0
            && primary_command.as_deref() == Some("merge")
            && invoked_args.iter().any(|arg| arg == "--squash")
        {
            let worktree = pending.worktree.as_deref().ok_or_else(|| {
                GitAiError::Generic(format!(
                    "merge --squash missing worktree sid={}",
                    pending.root_sid
                ))
            })?;
            Some(resolve_squash_source_head_for_worktree(worktree).ok_or_else(|| {
                GitAiError::Generic(format!(
                    "merge --squash missing source head from MERGE_HEAD/SQUASH_MSG sid={} worktree={}",
                    pending.root_sid,
                    worktree.display()
                ))
            })?)
        } else {
            None
        };

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
            ref_changes,
            confidence,
            wrapper_mirror: pending.wrapper_mirror,
        };

        if self.state.completed_roots.len() > 8_192 {
            self.state.completed_roots.clear();
        }
        trace_debug_lifecycle(&format!(
            "trace normalizer finalized sid={} primary={:?} pending_after_finalize={}",
            root_sid,
            normalized.primary_command,
            self.state.pending.len()
        ));
        self.state.completed_roots.insert(root_sid.to_string());
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

#[derive(Clone, Copy)]
enum MergeCutMode {
    Min,
    Max,
}

fn merge_reflog_cut(target: &mut Option<ReflogCut>, incoming: Option<ReflogCut>, mode: MergeCutMode) {
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
            return Some(PathBuf::from(argv[idx + 1].clone()));
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

fn args_have_rebase_control_mode(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "--continue" | "--abort" | "--skip" | "--quit"))
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
    let (_invoked_command, invoked_args) = canonical_invocation(&pending.raw_argv, primary.as_deref());
    !args_have_rebase_control_mode(&invoked_args)
}

fn pending_rebase_original_head_from_inflight(
    state: &TraceNormalizerState,
    family: &FamilyKey,
) -> Option<String> {
    state
        .pending
        .values()
        .filter(|pending| pending.family_key.as_ref() == Some(family))
        .filter(|pending| pending_is_non_control_rebase(pending))
        .filter_map(|pending| {
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

fn explicit_rebase_branch_arg(args: &[String]) -> Option<&str> {
    let mut positionals = Vec::new();
    let mut has_root = false;
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--root" => {
                has_root = true;
                continue;
            }
            "--onto" | "-s" | "--strategy" | "-X" | "--strategy-option" | "-m" | "--mainline"
            | "-S" | "--gpg-sign" => {
                skip_next = true;
                continue;
            }
            _ => {}
        }
        if arg.starts_with("--onto=")
            || arg.starts_with("--strategy=")
            || arg.starts_with("--strategy-option=")
            || arg.starts_with("--gpg-sign=")
        {
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        positionals.push(arg.as_str());
    }
    if has_root {
        positionals.first().copied()
    } else {
        positionals.get(1).copied()
    }
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
    let common_dir = PathBuf::from(&family.0);
    let loose_ref = common_dir.join(&ref_name);
    if let Ok(contents) = fs::read_to_string(&loose_ref) {
        let oid = contents.trim();
        if is_valid_oid(oid) && !is_zero_oid(oid) {
            return Some(oid.to_string());
        }
    }
    let packed_refs = common_dir.join("packed-refs");
    read_packed_ref_oid(&packed_refs, &ref_name)
}

fn read_packed_ref_oid(path: &Path, ref_name: &str) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let oid = parts.next()?;
        let name = parts.next()?;
        if name == ref_name && is_valid_oid(oid) && !is_zero_oid(oid) {
            return Some(oid.to_string());
        }
    }
    None
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
    fn alias_commit_captures_mutation_state_at_start() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        backend.set_alias("/repo", "ci", "commit");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"alias-commit",
            "ts":1,
            "argv":["git","ci","-m","msg"],
            "worktree":"/repo",
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
        backend.set_family(
            "/outer/nested/relative-clone",
            "/outer/nested/relative-clone/.git",
        );
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"clone-rel",
            "ts":1,
            "argv":["git","clone","ssh://example/repo.git","nested/relative-clone"],
            "worktree":"/outer"
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"clone-rel",
            "ts":2,
            "repo":"/outer/nested/relative-clone/.git"
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
        assert_eq!(
            cmd.worktree.as_ref(),
            Some(&PathBuf::from("/outer/nested/relative-clone"))
        );
        assert!(matches!(cmd.scope, CommandScope::Family(_)));
    }

    #[test]
    fn clone_with_late_family_resolution_does_not_error_without_reflog_start_cut() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend.clone());

        let start = serde_json::json!({
            "event":"start",
            "sid":"clone-late-family",
            "ts":1,
            "argv":["git","clone","ssh://example/repo.git","nested/relative-clone"],
            "worktree":"/outer"
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"clone-late-family",
            "ts":2,
            "worktree":"/outer/nested/relative-clone"
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
        backend.set_family(
            "/outer/nested/relative-clone",
            "/outer/nested/relative-clone/.git",
        );
        backend.set_context("/outer/nested/relative-clone", "head-after-clone");

        let cmd = normalizer
            .ingest_payload(&exit)
            .expect("clone finalize should not error")
            .expect("clone should emit a normalized command");

        assert_eq!(cmd.primary_command.as_deref(), Some("clone"));
        assert!(matches!(cmd.scope, CommandScope::Family(_)));
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
        backend.set_family("/repo-a", "/repo-a/.git");
        backend.set_context("/repo-a", "head-a");
        backend.set_family("/repo-b", "/repo-b/.git");
        backend.set_context("/repo-b", "head-b");
        let mut normalizer = TraceNormalizer::new(backend);

        let start_a = serde_json::json!({
            "event":"start",
            "sid":"s-a",
            "ts":1,
            "argv":["git","commit","-m","a"],
            "worktree":"/repo-a",
            "git_ai_family_reflog_start": {"HEAD": 100}
        });
        let start_b = serde_json::json!({
            "event":"start",
            "sid":"s-b",
            "ts":2,
            "argv":["git","push","origin","main"],
            "worktree":"/repo-b",
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
        assert_eq!(cmd_b.worktree.as_deref(), Some(Path::new("/repo-b")));
        assert!(matches!(cmd_b.scope, CommandScope::Family(_)));

        let cmd_a = normalizer.ingest_payload(&exit_a).unwrap().unwrap();
        assert_eq!(cmd_a.root_sid, "s-a");
        assert_eq!(cmd_a.primary_command.as_deref(), Some("commit"));
        assert_eq!(cmd_a.worktree.as_deref(), Some(Path::new("/repo-a")));
        assert!(matches!(cmd_a.scope, CommandScope::Family(_)));

        assert!(normalizer.state().pending.is_empty());
    }

    #[test]
    fn start_ignores_repo_gitdir_hint_and_uses_cwd_for_worktree_resolution() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo-worker-b", "/family/.git");
        backend.set_context("/repo-worker-b", "worker-head");
        let mut normalizer = TraceNormalizer::new(backend.clone());

        let start = serde_json::json!({
            "event":"start",
            "sid":"s-repo-field",
            "ts":1,
            "argv":["git","commit","-m","msg"],
            "repo":"/repo-base/.git",
            "cwd":"/repo-worker-b",
            "git_ai_family_reflog_start": {"HEAD": 300}
        });
        let def_repo = serde_json::json!({
            "event":"def_repo",
            "sid":"s-repo-field",
            "ts":2,
            "repo":"/repo-base/.git/worktrees/worker-b"
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
            Some("worker-head")
        );
        assert_eq!(
            cmd.post_repo.as_ref().and_then(|repo| repo.head.as_deref()),
            Some("worker-head")
        );
        assert_eq!(cmd.worktree.as_deref(), Some(Path::new("/repo-worker-b")));
    }

}
