use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::{AgentId, CheckpointKind};
use git_ai::commands::checkpoint::PreparedPathRole;
use git_ai::commands::checkpoint_agent::orchestrator::{CheckpointFileEntry, CheckpointRequest};
use git_ai::git::find_repository_in_path;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn build_scoped_human_checkpoint_request(
    repo_path: &str,
    scope_paths: Vec<String>,
) -> CheckpointRequest {
    static TEST_HUMAN_SCOPE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let session = TEST_HUMAN_SCOPE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let repo_work_dir = PathBuf::from(repo_path);
    let base_commit_sha = {
        let repo = find_repository_in_path(repo_path).unwrap();
        repo.revparse_single("HEAD")
            .map(|o| o.id())
            .unwrap_or_default()
    };
    let files = scope_paths
        .into_iter()
        .map(|p| {
            let abs_path = repo_work_dir.join(&p);
            let content = fs::read_to_string(&abs_path).unwrap_or_default();
            CheckpointFileEntry {
                path: abs_path,
                content,
                repo_work_dir: repo_work_dir.clone(),
                base_commit_sha: base_commit_sha.clone(),
            }
        })
        .collect();
    CheckpointRequest {
        trace_id: format!("test-human-scope-{}", session),
        checkpoint_kind: CheckpointKind::Human,
        agent_id: Some(AgentId {
            tool: "test_harness".to_string(),
            id: format!("test-human-scope-{}", session),
            model: "test_model".to_string(),
        }),
        files,
        path_role: PreparedPathRole::WillEdit,
        transcript_source: None,
        metadata: HashMap::new(),
    }
}

#[test]
fn test_build_scoped_human_agent_run_result_uses_current_changed_paths() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
    repo.git_og(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "base commit"]).unwrap();

    fs::write(repo.path().join("tracked.txt"), "base\nchanged\n").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut paths: Vec<String> = gitai_repo
        .get_staged_and_unstaged_filenames()
        .unwrap()
        .into_iter()
        .collect();
    paths.sort();

    assert!(!paths.is_empty(), "changed file should produce scope paths");

    let scoped = build_scoped_human_checkpoint_request(repo.path().to_str().unwrap(), paths);

    assert_eq!(scoped.checkpoint_kind, CheckpointKind::Human);
    assert_eq!(scoped.path_role, PreparedPathRole::WillEdit);
    assert_eq!(scoped.files.len(), 1);
    assert_eq!(
        scoped.files[0].repo_work_dir,
        PathBuf::from(repo.path().to_string_lossy().to_string())
    );
}
