use crate::daemon::analyzers::{AnalysisView, AnalyzerRegistry};
use crate::daemon::domain::{
    AnalysisResult, AppliedCommand, FamilyState, GlobalState, NormalizedCommand, WorktreeState,
};
use crate::error::GitAiError;
use std::path::{Path, PathBuf};

pub fn reduce_family_command(
    state: &mut FamilyState,
    cmd: NormalizedCommand,
    analyzers: &AnalyzerRegistry,
) -> Result<(AppliedCommand, AnalysisResult), GitAiError> {
    // Analyze against pre-command state so history/ref analyzers can infer old->new correctly.
    let analysis = analyzers.analyze(&cmd, AnalysisView { refs: &state.refs })?;
    apply_ref_changes(state, &cmd);
    apply_post_repo_refs(state, &cmd, &analysis);
    apply_analysis_ref_updates(state, &analysis);
    apply_worktree_state(state, &cmd);

    state.applied_seq = state.applied_seq.saturating_add(1);
    let applied = AppliedCommand {
        seq: state.applied_seq,
        command: cmd,
        analysis: analysis.clone(),
    };
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
    Ok((applied, analysis))
}

pub fn reduce_checkpoint(state: &mut FamilyState) {
    state.applied_seq = state.applied_seq.saturating_add(1);
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

fn apply_post_repo_refs(
    state: &mut FamilyState,
    cmd: &NormalizedCommand,
    analysis: &AnalysisResult,
) {
    if !should_apply_post_repo_refs(cmd, analysis) {
        return;
    }
    let Some(post_repo) = cmd.post_repo.as_ref() else {
        return;
    };
    let Some(head) = post_repo
        .head
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let head = head.to_string();
    if let Some(branch) = post_repo
        .branch
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        state.refs.insert(format!("refs/heads/{}", branch), head);
    }
}

fn should_apply_post_repo_refs(cmd: &NormalizedCommand, analysis: &AnalysisResult) -> bool {
    if cmd.post_repo.is_none() {
        return false;
    }

    if cmd
        .ref_changes
        .iter()
        .any(|change| change.reference == "HEAD" || change.reference.starts_with("refs/heads/"))
    {
        return false;
    }

    analysis.events.iter().any(|event| {
        matches!(
            event,
            crate::daemon::domain::SemanticEvent::CommitCreated { .. }
                | crate::daemon::domain::SemanticEvent::CommitAmended { .. }
                | crate::daemon::domain::SemanticEvent::Reset { .. }
                | crate::daemon::domain::SemanticEvent::RebaseComplete { .. }
                | crate::daemon::domain::SemanticEvent::RebaseAbort { .. }
                | crate::daemon::domain::SemanticEvent::CherryPickComplete { .. }
                | crate::daemon::domain::SemanticEvent::CherryPickAbort { .. }
                | crate::daemon::domain::SemanticEvent::PullCompleted { .. }
                | crate::daemon::domain::SemanticEvent::RefUpdated { .. }
                | crate::daemon::domain::SemanticEvent::BranchCreated { .. }
                | crate::daemon::domain::SemanticEvent::BranchDeleted { .. }
                | crate::daemon::domain::SemanticEvent::BranchRenamed { .. }
                | crate::daemon::domain::SemanticEvent::SymbolicRefUpdated { .. }
        )
    })
}

/// Update tracked refs from `RefUpdated` analysis events.  This covers
/// plumbing commands like `update-ref` where trace2 does not emit
/// `reference:` events and no `post_repo` snapshot is available.
fn apply_analysis_ref_updates(state: &mut FamilyState, analysis: &AnalysisResult) {
    for event in &analysis.events {
        if let crate::daemon::domain::SemanticEvent::RefUpdated { reference, new, .. } = event
            && !new.is_empty()
        {
            state.refs.insert(reference.clone(), new.clone());
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
    use std::collections::HashMap;

    fn family_state() -> FamilyState {
        FamilyState {
            family_key: FamilyKey::new("family:/tmp/repo"),
            refs: HashMap::new(),
            worktrees: HashMap::new(),
            last_error: None,
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
            inflight_rebase_original_head: None,
            merge_squash_source_head: None,
            carryover_snapshot_id: None,
            stash_target_oid: None,
            ref_changes: vec![RefChange {
                reference: "refs/heads/main".to_string(),
                old: "".to_string(),
                new: "abc".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_invocation_id: None,
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
            crate::daemon::domain::CommandClass::HistoryRewrite
        ));
        assert_eq!(
            state.refs.get("refs/heads/main").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn reducer_tracks_head_from_post_repo_snapshot_for_head_moving_commands() {
        let mut state = family_state();
        let registry = AnalyzerRegistry::new();
        let mut cmd = normalized();
        cmd.ref_changes.clear();
        cmd.raw_argv = vec!["git".to_string(), "commit".to_string()];
        cmd.primary_command = Some("commit".to_string());
        cmd.invoked_command = Some("commit".to_string());
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("def".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });

        let (_applied, _analysis) = reduce_family_command(&mut state, cmd, &registry).unwrap();

        assert_eq!(
            state.refs.get("refs/heads/main").map(String::as_str),
            Some("def")
        );
    }

    #[test]
    fn reducer_ignores_post_repo_snapshot_for_stash_commands() {
        let mut state = family_state();
        state
            .refs
            .insert("refs/heads/main".to_string(), "abc".to_string());
        let registry = AnalyzerRegistry::new();
        let mut cmd = normalized();
        cmd.ref_changes.clear();
        cmd.raw_argv = vec!["git".to_string(), "stash".to_string(), "push".to_string()];
        cmd.primary_command = Some("stash".to_string());
        cmd.invoked_command = Some("stash".to_string());
        cmd.invoked_args = vec!["push".to_string()];
        cmd.post_repo = Some(crate::daemon::domain::RepoContext {
            head: Some("def".to_string()),
            branch: Some("main".to_string()),
            detached: false,
        });

        let (_applied, _analysis) = reduce_family_command(&mut state, cmd, &registry).unwrap();

        assert_eq!(
            state.refs.get("refs/heads/main").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn global_reducer_never_drops_commands() {
        let mut state = GlobalState { applied_seq: 0 };
        let registry = AnalyzerRegistry::new();
        let (applied, _analysis) =
            reduce_global_command(&mut state, normalized(), &registry).unwrap();
        assert_eq!(applied.seq, 1);
        assert_eq!(state.applied_seq, 1);
    }
}
