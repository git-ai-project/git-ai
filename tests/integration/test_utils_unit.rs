use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::{AgentId, CheckpointKind};
use git_ai::commands::checkpoint::PreparedPathRole;
use git_ai::commands::checkpoint_agent::orchestrator::CheckpointRequest;
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
    CheckpointRequest {
        trace_id: format!("test-human-scope-{}", session),
        checkpoint_kind: CheckpointKind::Human,
        agent_id: Some(AgentId {
            tool: "test_harness".to_string(),
            id: format!("test-human-scope-{}", session),
            model: "test_model".to_string(),
        }),
        repo_working_dir: PathBuf::from(repo_path),
        file_paths: scope_paths.into_iter().map(PathBuf::from).collect(),
        path_role: PreparedPathRole::WillEdit,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    }
}

fn apply_default_checkpoint_scope(
    repo_path: &str,
    scope_paths: Vec<String>,
    checkpoint_request: Option<CheckpointRequest>,
    checkpoint_kind: CheckpointKind,
) -> Option<CheckpointRequest> {
    match checkpoint_request {
        Some(mut result) => {
            let has_explicit_scope = !result.file_paths.is_empty();

            if !has_explicit_scope {
                result.repo_working_dir = PathBuf::from(repo_path);
                result.file_paths = scope_paths.into_iter().map(PathBuf::from).collect();
                if checkpoint_kind == CheckpointKind::Human {
                    result.path_role = PreparedPathRole::WillEdit;
                } else {
                    result.path_role = PreparedPathRole::Edited;
                }
            }

            Some(result)
        }
        None => {
            if scope_paths.is_empty() {
                None
            } else {
                Some(build_scoped_human_checkpoint_request(
                    repo_path,
                    scope_paths,
                ))
            }
        }
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
    assert_eq!(scoped.file_paths, vec![PathBuf::from("tracked.txt")]);
    assert_eq!(
        scoped.repo_working_dir,
        PathBuf::from(repo.path().to_string_lossy().to_string())
    );
}

#[test]
fn test_apply_default_checkpoint_scope_preserves_existing_explicit_scope() {
    let repo = TestRepo::new();
    fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
    repo.git_og(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "base commit"]).unwrap();

    fs::write(repo.path().join("tracked.txt"), "base\nchanged\n").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let mut scope_paths: Vec<String> = gitai_repo
        .get_staged_and_unstaged_filenames()
        .unwrap()
        .into_iter()
        .collect();
    scope_paths.sort();

    let original = CheckpointRequest {
        trace_id: "test-session".to_string(),
        checkpoint_kind: CheckpointKind::Human,
        agent_id: Some(AgentId {
            tool: "test-tool".to_string(),
            id: "test-session".to_string(),
            model: "test-model".to_string(),
        }),
        repo_working_dir: PathBuf::new(),
        file_paths: vec![PathBuf::from("custom.txt")],
        path_role: PreparedPathRole::WillEdit,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    let applied = apply_default_checkpoint_scope(
        repo.path().to_str().unwrap(),
        scope_paths,
        Some(original.clone()),
        CheckpointKind::Human,
    )
    .expect("explicit scope should be preserved");

    assert_eq!(applied.file_paths, original.file_paths);
    assert_eq!(applied.repo_working_dir, original.repo_working_dir);
}
