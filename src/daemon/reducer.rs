use crate::daemon::analyzers::{AnalysisView, AnalyzerRegistry};
use crate::daemon::domain::{
    AnalysisResult, AppliedCommand, CheckpointObserved, CheckpointSummary, EnvOverrideSet,
    FamilyState, GlobalState, NormalizedCommand, ReconcileSnapshot, WorktreeState,
};
use crate::error::GitAiError;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

const RECENT_COMMAND_CAP: usize = 512;

pub fn reduce_family_command(
    state: &mut FamilyState,
    cmd: NormalizedCommand,
    analyzers: &AnalyzerRegistry,
) -> Result<(AppliedCommand, AnalysisResult), GitAiError> {
    // Analyze against pre-command state so history/ref analyzers can infer old->new correctly.
    let analysis = analyzers.analyze(&cmd, AnalysisView { refs: &state.refs })?;
    apply_ref_changes(state, &cmd);
    apply_worktree_state(state, &cmd);

    state.applied_seq = state.applied_seq.saturating_add(1);
    let applied = AppliedCommand {
        seq: state.applied_seq,
        command: cmd,
        analysis: analysis.clone(),
    };
    state.recent_commands.push_back(applied.clone());
    cap_recent_commands(&mut state.recent_commands);
    Ok((applied, analysis))
}

pub fn reduce_global_command(
    state: &mut GlobalState,
    cmd: NormalizedCommand,
    analyzers: &AnalyzerRegistry,
) -> Result<(AppliedCommand, AnalysisResult), GitAiError> {
    let empty_refs = std::collections::HashMap::new();
    let analysis = analyzers.analyze(&cmd, AnalysisView { refs: &empty_refs })?;
    state.applied_seq = state.applied_seq.saturating_add(1);
    let applied = AppliedCommand {
        seq: state.applied_seq,
        command: cmd,
        analysis: analysis.clone(),
    };
    state.recent_commands.push_back(applied.clone());
    cap_recent_commands(&mut state.recent_commands);
    Ok((applied, analysis))
}

pub fn reduce_checkpoint(state: &mut FamilyState, checkpoint: CheckpointObserved) {
    state.applied_seq = state.applied_seq.saturating_add(1);
    state.checkpoints.insert(
        checkpoint.id.clone(),
        CheckpointSummary {
            id: checkpoint.id,
            author: checkpoint.author,
            timestamp_ns: checkpoint.timestamp_ns,
            file_count: checkpoint.file_count,
        },
    );
}

pub fn reduce_env_override(state: &mut FamilyState, env: EnvOverrideSet) {
    state.applied_seq = state.applied_seq.saturating_add(1);
    let key = env
        .repo_working_dir
        .canonicalize()
        .unwrap_or(env.repo_working_dir);
    state.env_overrides.insert(key, env.overrides);
}

pub fn reduce_reconcile(state: &mut FamilyState, reconcile: ReconcileSnapshot) {
    state.applied_seq = state.applied_seq.saturating_add(1);
    state.refs = reconcile.refs;
    state.last_reconcile_ns = Some(reconcile.timestamp_ns);
}

fn apply_ref_changes(state: &mut FamilyState, cmd: &NormalizedCommand) {
    for change in &cmd.ref_changes {
        if change.new.trim().is_empty() {
            state.refs.remove(&change.reference);
        } else {
            state
                .refs
                .insert(change.reference.clone(), change.new.clone());
        }
    }
}

fn apply_worktree_state(state: &mut FamilyState, cmd: &NormalizedCommand) {
    let Some(worktree) = cmd.worktree.as_ref() else {
        return;
    };
    let Some(post_repo) = cmd.post_repo.as_ref() else {
        return;
    };

    state.worktrees.insert(
        canonicalize_path(worktree),
        WorktreeState {
            head: post_repo.head.clone(),
            branch: post_repo.branch.clone(),
            detached: post_repo.detached,
            last_updated_ns: cmd.finished_at_ns,
        },
    );
}

fn cap_recent_commands(commands: &mut VecDeque<AppliedCommand>) {
    while commands.len() > RECENT_COMMAND_CAP {
        let _ = commands.pop_front();
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::analyzers::AnalyzerRegistry;
    use crate::daemon::domain::{
        CommandScope, Confidence, FamilyKey, FamilyState, GlobalState, RefChange,
    };
    use std::collections::{HashMap, VecDeque};

    fn family_state() -> FamilyState {
        FamilyState {
            family_key: FamilyKey::new("family:/tmp/repo"),
            refs: HashMap::new(),
            worktrees: HashMap::new(),
            recent_commands: VecDeque::new(),
            checkpoints: HashMap::new(),
            env_overrides: HashMap::new(),
            last_error: None,
            last_reconcile_ns: None,
            applied_seq: 0,
        }
    }

    fn normalized() -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new("family:/tmp/repo")),
            family_key: Some(FamilyKey::new("family:/tmp/repo")),
            worktree: Some(PathBuf::from("/tmp/repo")),
            root_sid: "sid".to_string(),
            raw_argv: vec!["git".to_string(), "update-ref".to_string()],
            primary_command: Some("update-ref".to_string()),
            invoked_command: Some("update-ref".to_string()),
            invoked_args: Vec::new(),
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            ref_changes: vec![RefChange {
                reference: "refs/heads/main".to_string(),
                old: "".to_string(),
                new: "abc".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[test]
    fn reducer_applies_ref_changes_and_produces_applied_command() {
        let mut state = family_state();
        let registry = AnalyzerRegistry::new();
        let (applied, analysis) =
            reduce_family_command(&mut state, normalized(), &registry).unwrap();
        assert_eq!(applied.seq, 1);
        assert!(matches!(
            analysis.class,
            crate::daemon::domain::CommandClass::RefMutation
        ));
        assert_eq!(
            state.refs.get("refs/heads/main").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn global_reducer_never_drops_commands() {
        let mut state = GlobalState {
            recent_commands: VecDeque::new(),
            applied_seq: 0,
        };
        let registry = AnalyzerRegistry::new();
        let (applied, _analysis) =
            reduce_global_command(&mut state, normalized(), &registry).unwrap();
        assert_eq!(applied.seq, 1);
        assert_eq!(state.recent_commands.len(), 1);
    }
}
