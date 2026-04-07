use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::utils::debug_log;

pub fn pre_commit(repo: &Repository, default_author: String) -> Result<(), GitAiError> {
    let (checkpoint_kind, agent_run_result) = pre_commit_checkpoint_context(repo);

    let result: Result<(usize, usize, usize), GitAiError> = crate::commands::checkpoint::run(
        repo,
        &default_author,
        checkpoint_kind,
        true,
        agent_run_result,
        true, // should skip if NO AI CHECKPOINTS
    );
    result.map(|_| ())
}

fn pre_commit_checkpoint_context(repo: &Repository) -> (CheckpointKind, Option<AgentRunResult>) {
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
        debug_log("pre-commit: using active bash context for AI checkpoint");
        return (checkpoint_kind, agent_run_result);
    }

    debug_log("pre-commit: no active inflight bash agent context, using human checkpoint");
    (CheckpointKind::Human, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;
    use std::fs;

    #[test]
    fn test_pre_commit_empty_repo() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Should handle empty repo gracefully
        let result = pre_commit(repo, "test_author".to_string());
        // May succeed or fail depending on repo state, but shouldn't panic
        let _ = result;
    }

    #[test]
    fn test_pre_commit_with_staged_changes() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create and stage a file
        let file_path = test_repo.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("test.txt")).unwrap();
        index.write().unwrap();

        let result = pre_commit(repo, "test_author".to_string());
        // Should not panic
        let _ = result;
    }

    #[test]
    fn test_pre_commit_no_changes() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        // Create initial commit
        let file_path = test_repo.path().join("initial.txt");
        fs::write(&file_path, "initial").unwrap();

        let mut index = test_repo.repo().index().unwrap();
        index.add_path(std::path::Path::new("initial.txt")).unwrap();
        index.write().unwrap();

        let tree_id = index.write_tree().unwrap();
        let tree = test_repo.repo().find_tree(tree_id).unwrap();
        let sig = test_repo.repo().signature().unwrap();

        test_repo
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .unwrap();

        // Run pre_commit with no staged changes
        let result = pre_commit(repo, "test_author".to_string());
        // Should handle gracefully
        let _ = result;
    }

    #[test]
    fn test_pre_commit_result_mapping() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();

        let result = pre_commit(repo, "author".to_string());

        // Result should be either Ok(()) or Err(GitAiError)
        match result {
            Ok(()) => {
                // Success case
            }
            Err(_) => {
                // Error case is also acceptable
            }
        }
    }

    #[test]
    fn test_pre_commit_checkpoint_context_uses_inflight_bash_agent_context() {
        let test_repo = TmpRepo::new().unwrap();
        let repo = test_repo.gitai_repo();
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("codex-session-simple.jsonl");
        let metadata = std::collections::HashMap::from([(
            "transcript_path".to_string(),
            fixture.to_string_lossy().to_string(),
        )]);
        crate::commands::checkpoint_agent::bash_tool::handle_bash_pre_tool_use_with_context(
            test_repo.path(),
            "session-1",
            "tool-1",
            &crate::authorship::working_log::AgentId {
                tool: "codex".to_string(),
                id: "session-1".to_string(),
                model: "gpt-5.4".to_string(),
            },
            Some(&metadata),
        )
        .unwrap();

        let (checkpoint_kind, agent_run_result) = pre_commit_checkpoint_context(repo);
        assert_eq!(checkpoint_kind, CheckpointKind::AiAgent);
        let agent_run_result = agent_run_result.expect("expected codex agent result");
        assert_eq!(agent_run_result.agent_id.tool, "codex");
        assert_eq!(agent_run_result.agent_id.id, "session-1");
        assert_eq!(
            agent_run_result
                .agent_metadata
                .as_ref()
                .and_then(|m| m.get("transcript_path"))
                .map(String::as_str),
            Some(fixture.to_string_lossy().as_ref())
        );
    }
}
