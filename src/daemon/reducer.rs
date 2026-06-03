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
        if change.new.trim().is_empty() || is_zero_oid(&change.new) {
            state.refs.remove(&change.reference);
        } else {
            state
                .refs
                .insert(change.reference.clone(), change.new.clone());
        }
    }
}

fn is_zero_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|ch| ch == '0')
}

fn apply_worktree_state(state: &mut FamilyState, cmd: &NormalizedCommand) {
    let Some(worktree) = cmd.worktree.as_ref() else {
        return;
    };
    let head = cmd
        .ref_changes
        .iter()
        .rfind(|change| change.reference == "HEAD")
        .map(|change| change.new.clone());

    state.worktrees.insert(
        canonicalize_path(worktree),
        WorktreeState {
            head,
            branch: None,
            detached: false,
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
        CommandScope, Confidence, FamilyKey, FamilyState, GlobalState, RefChange, WatermarkState,
    };
    use std::collections::HashMap;

    fn family_state() -> FamilyState {
        FamilyState {
            family_key: FamilyKey::new("family:/tmp/repo"),
            refs: HashMap::new(),
            worktrees: HashMap::new(),
            last_error: None,
            applied_seq: 0,
            watermarks: WatermarkState::default(),
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
            stash_target_oid: None,
            ref_changes: vec![RefChange {
                reference: "refs/heads/main".to_string(),
                old: "".to_string(),
                new: "abc".to_string(),
            }],
            confidence: Confidence::Low,
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
    fn reducer_does_not_update_refs_without_ref_transition_for_head_moving_commands() {
        let mut state = family_state();
        let registry = AnalyzerRegistry::new();
        let mut cmd = normalized();
        cmd.ref_changes.clear();
        cmd.raw_argv = vec!["git".to_string(), "commit".to_string()];
        cmd.primary_command = Some("commit".to_string());
        cmd.invoked_command = Some("commit".to_string());

        let (_applied, _analysis) = reduce_family_command(&mut state, cmd, &registry).unwrap();

        assert_eq!(state.refs.get("refs/heads/main").map(String::as_str), None);
    }

    #[test]
    fn reducer_preserves_refs_for_stash_without_ref_transition() {
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

        let (_applied, _analysis) = reduce_family_command(&mut state, cmd, &registry).unwrap();

        assert_eq!(
            state.refs.get("refs/heads/main").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn reducer_removes_refs_deleted_with_zero_oid() {
        let mut state = family_state();
        state.refs.insert(
            "refs/heads/feature".to_string(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        );
        let registry = AnalyzerRegistry::new();
        let mut cmd = normalized();
        cmd.ref_changes = vec![RefChange {
            reference: "refs/heads/feature".to_string(),
            old: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            new: "0000000000000000000000000000000000000000".to_string(),
        }];

        let (_applied, _analysis) = reduce_family_command(&mut state, cmd, &registry).unwrap();

        assert!(!state.refs.contains_key("refs/heads/feature"));
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
