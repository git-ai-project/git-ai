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

    // Query the daemon for an active bash session in this repo.
    // If a bash tool call is currently in flight, we attribute the commit as AI.
    if let Some(session_info) = query_daemon_bash_session(&repo_workdir) {
        tracing::debug!(
            "pre-commit: active bash session found for AI checkpoint (agent: {:?})",
            session_info.agent_id
        );
        let checkpoint_request = CheckpointRequest {
            trace_id: crate::authorship::authorship_log_serialization::generate_trace_id(),
            checkpoint_kind: CheckpointKind::AiAgent,
            agent_id: session_info.agent_id,
            files: vec![],
            path_role: crate::commands::checkpoint::PreparedPathRole::Edited,
            transcript_source: None,
            metadata: session_info.metadata.unwrap_or_default(),
        };
        return (CheckpointKind::AiAgent, Some(checkpoint_request));
    }

    tracing::debug!("pre-commit: no active inflight bash agent context, using human checkpoint");
    (CheckpointKind::Human, None)
}

/// Query the daemon for an active bash session in the given repo.
fn query_daemon_bash_session(
    repo_working_dir: &str,
) -> Option<crate::daemon::control_api::BashSessionQueryResponse> {
    use crate::daemon::control_api::ControlRequest;
    use std::time::Duration;

    let config = crate::daemon::DaemonConfig::from_env_or_default_paths().ok()?;
    if !config.control_socket_path.exists() {
        return None;
    }
    let request = ControlRequest::BashSessionQuery {
        repo_work_dir: repo_working_dir.to_string(),
    };
    let response = crate::daemon::send_control_request_with_timeout(
        &config.control_socket_path,
        &request,
        Duration::from_millis(500),
    )
    .ok()?;

    if !response.ok {
        return None;
    }

    let data = response.data?;
    let session_response: crate::daemon::control_api::BashSessionQueryResponse =
        serde_json::from_value(data).ok()?;

    if session_response.active {
        Some(session_response)
    } else {
        None
    }
}
