use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::error::GitAiError;
use crate::git::repository::Repository;
pub fn pre_commit(repo: &Repository, default_author: String) -> Result<(), GitAiError> {
    let (checkpoint_kind, checkpoint_request) = pre_commit_checkpoint_context(repo);

    let result: Result<(usize, usize, usize), GitAiError> = crate::commands::checkpoint::run(
        repo,
        &default_author,
        checkpoint_kind,
        true,
        checkpoint_request,
        true, // should skip if NO AI CHECKPOINTS
    );
    result.map(|_| ())
}

fn pre_commit_checkpoint_context(repo: &Repository) -> (CheckpointKind, Option<CheckpointRequest>) {
    let Ok(repo_workdir) = repo
        .workdir()
        .map(|path| path.to_string_lossy().to_string())
    else {
        return (CheckpointKind::Human, None);
    };
    let repo_root = std::path::Path::new(&repo_workdir);

    if let Some((checkpoint_kind, agent_run_result)) =
        crate::commands::checkpoint_agent::bash_tool::checkpoint_context_from_active_bash(
            repo_root,
            &repo_workdir,
        )
    {
        tracing::debug!("pre-commit: using active bash context for AI checkpoint");
        return (checkpoint_kind, agent_run_result);
    }

    tracing::debug!("pre-commit: no active inflight bash agent context, using human checkpoint");
    (CheckpointKind::Human, None)
}

