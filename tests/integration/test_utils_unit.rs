use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::{AgentId, CheckpointKind};
use git_ai::commands::checkpoint_agent::agent_presets::AgentRunResult;
use git_ai::git::find_repository_in_path;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

fn build_scoped_human_agent_run_result(
    repo_path: &str,
    scope_paths: Vec<String>,
) -> AgentRunResult {
    static TEST_HUMAN_SCOPE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let session = TEST_HUMAN_SCOPE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    AgentRunResult {
        agent_id: AgentId {
            tool: "test_harness".to_string(),
            id: format!("test-human-scope-{}", session),
            model: "test_model".to_string(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::Human,
        transcript: None,
        repo_working_dir: Some(repo_path.to_string()),
        edited_filepaths: None,
        will_edit_filepaths: Some(scope_paths),
        dirty_files: None,
        captured_checkpoint_id: None,
    }
}

fn apply_default_checkpoint_scope(
    repo_path: &str,
    scope_paths: Vec<String>,
    agent_run_result: Option<AgentRunResult>,
    checkpoint_kind: CheckpointKind,
) -> Option<AgentRunResult> {
    match agent_run_result {
        Some(mut result) => {
            let has_explicit_scope = if checkpoint_kind == CheckpointKind::Human {
                result
                    .will_edit_filepaths
                    .as_ref()
                    .is_some_and(|paths| !paths.is_empty())
            } else {
                result
                    .edited_filepaths
                    .as_ref()
                    .is_some_and(|paths| !paths.is_empty())
            };

            if !has_explicit_scope {
                result.repo_working_dir = Some(repo_path.to_string());
                if checkpoint_kind == CheckpointKind::Human {
                    result.will_edit_filepaths = Some(scope_paths);
                    result.edited_filepaths = None;
                } else {
                    result.edited_filepaths = Some(scope_paths);
                    result.will_edit_filepaths = None;
                }
            }

            Some(result)
        }
        None => {
            if scope_paths.is_empty() {
                None
            } else {
                Some(build_scoped_human_agent_run_result(repo_path, scope_paths))
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

    let scoped = build_scoped_human_agent_run_result(repo.path().to_str().unwrap(), paths);

    assert_eq!(scoped.checkpoint_kind, CheckpointKind::Human);
    assert_eq!(
        scoped.will_edit_filepaths,
        Some(vec!["tracked.txt".to_string()])
    );
    assert_eq!(
        scoped.repo_working_dir,
        Some(repo.path().to_string_lossy().to_string())
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

    let original = AgentRunResult {
        agent_id: AgentId {
            tool: "test-tool".to_string(),
            id: "test-session".to_string(),
            model: "test-model".to_string(),
        },
        agent_metadata: None,
        checkpoint_kind: CheckpointKind::Human,
        transcript: None,
        repo_working_dir: None,
        edited_filepaths: None,
        will_edit_filepaths: Some(vec!["custom.txt".to_string()]),
        dirty_files: None,
        captured_checkpoint_id: None,
    };

    let applied = apply_default_checkpoint_scope(
        repo.path().to_str().unwrap(),
        scope_paths,
        Some(original.clone()),
        CheckpointKind::Human,
    )
    .expect("explicit scope should be preserved");

    assert_eq!(applied.will_edit_filepaths, original.will_edit_filepaths);
    assert_eq!(applied.repo_working_dir, original.repo_working_dir);
}
