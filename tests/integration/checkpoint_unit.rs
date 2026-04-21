use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::{AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};
use git_ai::commands::checkpoint::{
    BaseOverrideResolutionPolicy, PreparedPathRole, cleanup_failed_captured_checkpoint_prepare,
    compute_file_line_stats, explicit_capture_target_paths, is_ai_author_id,
    run_with_base_commit_override, run_with_base_commit_override_with_policy,
};
use git_ai::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use git_ai::error::GitAiError;
use git_ai::git::repository::find_repository_in_path;
use std::collections::HashMap;
use std::path::PathBuf;

/// Helper function equivalent to TmpRepo::new_with_base_commit()
fn setup_repo_with_base_commit() -> (TestRepo, String, String) {
    let repo = TestRepo::new();

    let lines_content = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n25\n26\n";
    let alphabet_content =
        "A\nB\nC\nD\nE\nF\nG\nH\nI\nJ\nK\nL\nM\nN\nO\nP\nQ\nR\nS\nT\nU\nV\nW\nX\nY\nZ\n";

    std::fs::write(repo.path().join("lines.md"), lines_content).unwrap();
    std::fs::write(repo.path().join("alphabet.md"), alphabet_content).unwrap();
    repo.git(&["add", "lines.md", "alphabet.md"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "lines.md"])
        .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "alphabet.md"])
        .unwrap();
    repo.stage_all_and_commit("initial commit").unwrap();

    (repo, "lines.md".to_string(), "alphabet.md".to_string())
}

fn test_checkpoint_request(
    checkpoint_kind: CheckpointKind,
    edited_filepaths: Option<Vec<&str>>,
    will_edit_filepaths: Option<Vec<&str>>,
    dirty_files: Option<HashMap<&str, &str>>,
) -> CheckpointRequest {
    let (file_paths, path_role) = if let Some(paths) = edited_filepaths {
        (
            paths.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            PreparedPathRole::Edited,
        )
    } else if let Some(paths) = will_edit_filepaths {
        (
            paths.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            PreparedPathRole::WillEdit,
        )
    } else {
        (vec![], PreparedPathRole::Edited)
    };

    CheckpointRequest {
        trace_id: "test-trace".to_string(),
        checkpoint_kind,
        agent_id: Some(AgentId {
            tool: "test-agent".to_string(),
            id: "test-capture".to_string(),
            model: "test-model".to_string(),
        }),
        repo_working_dir: PathBuf::from("."),
        file_paths,
        path_role,
        dirty_files: dirty_files.map(|files| {
            files
                .into_iter()
                .map(|(path, content)| (PathBuf::from(path), content.to_string()))
                .collect()
        }),
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    }
}

#[test]
fn test_explicit_capture_target_paths_accepts_non_empty_edited_filepaths() {
    let agent_run_result = test_checkpoint_request(
        CheckpointKind::AiAgent,
        Some(vec!["src/main.rs"]),
        None,
        None,
    );

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&agent_run_result)),
        Some((PreparedPathRole::Edited, vec!["src/main.rs".to_string()]))
    );
}

#[test]
fn test_explicit_capture_target_paths_accepts_non_empty_will_edit_filepaths() {
    let agent_run_result =
        test_checkpoint_request(CheckpointKind::Human, None, Some(vec!["src/lib.rs"]), None);

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::Human, Some(&agent_run_result)),
        Some((PreparedPathRole::WillEdit, vec!["src/lib.rs".to_string()]))
    );
}

#[test]
fn test_explicit_capture_target_paths_rejects_dirty_files_without_explicit_paths() {
    let agent_run_result = test_checkpoint_request(
        CheckpointKind::AiAgent,
        None,
        None,
        Some(HashMap::from([("src/main.rs", "fn main() {}\n")])),
    );

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&agent_run_result)),
        None
    );
}

#[test]
fn test_explicit_capture_target_paths_known_human_uses_edited_filepaths() {
    // KnownHuman post-save: edit already happened, uses edited_filepaths.
    let agent_run_result = test_checkpoint_request(
        CheckpointKind::KnownHuman,
        Some(vec!["src/foo.rs"]),
        None,
        None,
    );

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::KnownHuman, Some(&agent_run_result)),
        Some((PreparedPathRole::Edited, vec!["src/foo.rs".to_string()]))
    );
}

#[test]
fn test_explicit_capture_target_paths_known_human_uses_will_edit_filepaths() {
    // KnownHuman pre-save: edit hasn't happened yet, uses will_edit_filepaths.
    // Regression: KnownHuman fell into the else branch which only reads edited_filepaths,
    // returning None and silently disabling pathspec scoping for pre-save KnownHuman.
    let agent_run_result = test_checkpoint_request(
        CheckpointKind::KnownHuman,
        None,
        Some(vec!["src/foo.rs"]),
        None,
    );

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::KnownHuman, Some(&agent_run_result)),
        Some((PreparedPathRole::WillEdit, vec!["src/foo.rs".to_string()]))
    );
}

#[test]
fn test_explicit_capture_target_paths_rejects_empty_explicit_lists() {
    let human_result =
        test_checkpoint_request(CheckpointKind::Human, None, Some(vec!["", "   "]), None);
    let ai_result =
        test_checkpoint_request(CheckpointKind::AiAgent, Some(vec!["", "   "]), None, None);

    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::Human, Some(&human_result)),
        None
    );
    assert_eq!(
        explicit_capture_target_paths(CheckpointKind::AiAgent, Some(&ai_result)),
        None
    );
}

#[test]
fn test_cleanup_failed_captured_checkpoint_prepare_removes_partial_capture_dir() {
    let temp = tempfile::tempdir().expect("temp dir should be creatable");
    let capture_dir = temp.path().join("capture-fixture");
    std::fs::create_dir_all(capture_dir.join("blobs"))
        .expect("partial capture directory should be creatable");
    std::fs::write(capture_dir.join("blobs").join("partial-blob"), "partial")
        .expect("partial blob should be creatable");

    cleanup_failed_captured_checkpoint_prepare(
        &capture_dir,
        "capture-fixture",
        &GitAiError::Generic("synthetic prepare failure".to_string()),
    );

    assert!(
        !capture_dir.exists(),
        "cleanup helper should remove partial capture directories"
    );
}

#[test]
fn test_checkpoint_with_staged_changes() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Make changes to the file
    let file_path = repo.path().join(&lines_file);
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("New line added by user\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Run checkpoint - it should track the changes even though they're staged
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Verify the checkpoint was created with correct entries
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    // The bug: when changes are staged, entries_len is 0 instead of 1
    assert_eq!(
        latest.entries.len(),
        1,
        "Should have 1 file entry in checkpoint (staged changes should be tracked)"
    );
}

#[test]
fn test_checkpoint_with_staged_changes_after_previous_checkpoint() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Make first changes and checkpoint
    let file_path = repo.path().join(&lines_file);
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("First change\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Make second changes - these are staged
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Second change\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Run checkpoint again - it should track the staged changes even after a previous checkpoint
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Verify the checkpoint was created with correct entries
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    assert_eq!(
        latest.entries.len(),
        1,
        "Second checkpoint: should have 1 file entry in checkpoint (staged changes should be tracked)"
    );
}

#[test]
fn test_checkpoint_with_only_staged_no_unstaged_changes() {
    use std::fs;

    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Get the file path
    let file_path = repo.path().join(&lines_file);

    // Manually modify the file (bypassing TmpFile's automatic staging)
    let mut content = fs::read_to_string(&file_path).unwrap();
    content.push_str("New line for staging test\n");
    fs::write(&file_path, &content).unwrap();

    // Now manually stage it using git (this is what "git add" does)
    repo.git(&["add", &lines_file]).unwrap();

    // At this point: HEAD has old content, index has new content, workdir has new content
    // And unstaged should be "Unmodified" because workdir == index

    // Now run checkpoint
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Verify the checkpoint was created with correct entries
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    // This should work: we should see 1 file with 1 entry
    assert_eq!(
        latest.entries.len(),
        1,
        "Should track the staged changes in checkpoint"
    );
}

#[test]
fn test_checkpoint_with_only_unstaged_changes_for_ai_without_pathspec() {
    use std::fs;

    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Manually modify the file without staging it
    let file_path = repo.path().join(&lines_file);
    let mut content = fs::read_to_string(&file_path).unwrap();
    content.push_str("New unstaged AI line\n");
    fs::write(&file_path, &content).unwrap();

    // Trigger AI checkpoint without edited_filepaths (pathspec-less flow used by some agents)
    repo.git_ai(&["checkpoint", "mock_ai", &lines_file])
        .unwrap();

    // Verify the checkpoint was created
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    assert_eq!(
        latest.entries.len(),
        1,
        "Should create an AI checkpoint entry for unstaged changes without pathspecs"
    );
}

#[test]
fn test_checkpoint_base_override_controls_head_context_for_entry_generation() {
    use std::fs;

    let (repo, lines_file, _) = setup_repo_with_base_commit();
    let file_path = repo.path().join(&lines_file);

    fs::write(&file_path, "line from commit A\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit A").unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(&file_path, "line from commit B\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit B").unwrap();

    // Keep the worktree dirty so git status returns this file, but inject deterministic
    // content from commit B via dirty_files.
    fs::write(&file_path, "line from uncommitted edit\n").unwrap();

    let mut dirty_files = HashMap::new();
    dirty_files.insert(
        PathBuf::from(&lines_file),
        "line from commit B\n".to_string(),
    );
    let checkpoint_request = CheckpointRequest {
        trace_id: "base-override-regression".to_string(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(AgentId {
            tool: "mock_ai".to_string(),
            id: "base-override-regression".to_string(),
            model: "test".to_string(),
        }),
        repo_working_dir: repo.path().to_path_buf(),
        file_paths: vec![PathBuf::from(&lines_file)],
        path_role: PreparedPathRole::Edited,
        dirty_files: Some(dirty_files),
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let (entries_len, files_len, _) = run_with_base_commit_override_with_policy(
        &gitai_repo,
        "mock-ai",
        CheckpointKind::AiAgent,
        true,
        Some(checkpoint_request),
        false,
        Some(base_commit.as_str()),
        BaseOverrideResolutionPolicy::RequireExplicitSnapshot,
    )
    .unwrap();

    assert_eq!(
        files_len, 1,
        "Expected one tracked file for the checkpoint run"
    );
    assert_eq!(
        entries_len, 1,
        "When base override points to commit A, current content from commit B must produce an entry"
    );
}

#[test]
fn test_checkpoint_base_override_strict_rejects_missing_dirty_snapshot() {
    use std::fs;

    let (repo, lines_file, _) = setup_repo_with_base_commit();
    let file_path = repo.path().join(&lines_file);

    fs::write(&file_path, "line from commit A\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit A").unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(&file_path, "line from commit B\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit B").unwrap();

    // Keep the worktree dirty so the legacy fallback would succeed if it were used.
    fs::write(&file_path, "line from uncommitted edit\n").unwrap();

    let checkpoint_request = CheckpointRequest {
        trace_id: "base-override-strict-missing-snapshot".to_string(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(AgentId {
            tool: "mock_ai".to_string(),
            id: "base-override-strict-missing-snapshot".to_string(),
            model: "test".to_string(),
        }),
        repo_working_dir: repo.path().to_path_buf(),
        file_paths: vec![PathBuf::from(&lines_file)],
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let error = run_with_base_commit_override_with_policy(
        &gitai_repo,
        "mock-ai",
        CheckpointKind::AiAgent,
        true,
        Some(checkpoint_request),
        false,
        Some(base_commit.as_str()),
        BaseOverrideResolutionPolicy::RequireExplicitSnapshot,
    )
    .expect_err("strict base override should reject missing dirty snapshots");

    assert!(
        error
            .to_string()
            .contains("requires explicit in-repository target paths and a matching dirty snapshot"),
        "expected strict snapshot error, got: {}",
        error
    );
}

#[test]
fn test_checkpoint_base_override_allow_fallback_scans_when_snapshot_missing() {
    use std::fs;

    let (repo, lines_file, _) = setup_repo_with_base_commit();
    let file_path = repo.path().join(&lines_file);

    fs::write(&file_path, "line from commit A\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit A").unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(&file_path, "line from commit B\n").unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.stage_all_and_commit("commit B").unwrap();

    // Without a dirty snapshot the fallback path must rediscover the dirty file from the repo.
    fs::write(&file_path, "line from uncommitted edit\n").unwrap();

    let checkpoint_request = CheckpointRequest {
        trace_id: "base-override-allow-fallback".to_string(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(AgentId {
            tool: "mock_ai".to_string(),
            id: "base-override-allow-fallback".to_string(),
            model: "test".to_string(),
        }),
        repo_working_dir: repo.path().to_path_buf(),
        file_paths: vec![PathBuf::from(&lines_file)],
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let (entries_len, files_len, _) = run_with_base_commit_override(
        &gitai_repo,
        "mock-ai",
        CheckpointKind::AiAgent,
        true,
        Some(checkpoint_request),
        false,
        Some(base_commit.as_str()),
    )
    .expect("allow-fallback base override should still scan the repo");

    assert_eq!(
        files_len, 1,
        "fallback path should rediscover the changed file"
    );
    assert_eq!(
        entries_len, 1,
        "fallback path should still produce checkpoint entries from the worktree scan"
    );
}

#[test]
fn test_checkpoint_skips_conflicted_files() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Get the current branch name (whatever the default is)
    let base_branch = repo.current_branch();

    // Create a branch and make different changes on each branch to create a conflict
    repo.git(&["checkout", "-b", "feature-branch"]).unwrap();

    // On feature branch, modify the file
    let file_path = repo.path().join(&lines_file);
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Feature branch change\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();
    repo.stage_all_and_commit("Feature commit").unwrap();

    // Switch back to base branch and make conflicting changes
    repo.git(&["checkout", &base_branch]).unwrap();
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Main branch change\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();
    repo.stage_all_and_commit("Main commit").unwrap();

    // Attempt to merge feature-branch into base branch - this should create a conflict
    let output = repo.git_og(&["merge", "feature-branch"]);
    let has_conflicts = output.is_err();
    assert!(has_conflicts, "Should have merge conflicts");

    // Try to checkpoint while there are conflicts
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints_before = working_log.read_all_checkpoints().unwrap();
    let count_before = checkpoints_before.len();

    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Checkpoint should skip conflicted files - check that either no new checkpoint was created
    // or the new checkpoint has 0 entries
    let checkpoints_after = working_log.read_all_checkpoints().unwrap();
    if checkpoints_after.len() > count_before {
        let latest = checkpoints_after.last().unwrap();
        assert_eq!(
            latest.entries.len(),
            0,
            "Should have 0 entries (conflicted file should be skipped)"
        );
    }
}

#[test]
fn test_checkpoint_with_paths_outside_repo() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Make changes to the file
    let file_path = repo.path().join(&lines_file);
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("New line added\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Create checkpoint request with paths outside the repo
    let checkpoint_request = CheckpointRequest {
        trace_id: "test-outside-paths".to_string(),
        checkpoint_kind: CheckpointKind::AiAgent,
        agent_id: Some(AgentId {
            tool: "test_tool".to_string(),
            id: "test_session".to_string(),
            model: "test_model".to_string(),
        }),
        repo_working_dir: repo.path().to_path_buf(),
        file_paths: vec![
            PathBuf::from("/tmp/outside_file.txt"),
            PathBuf::from("../outside_parent.txt"),
            PathBuf::from(&lines_file), // This one is valid
        ],
        path_role: PreparedPathRole::Edited,
        dirty_files: None,
        transcript_source: None,
        metadata: HashMap::new(),
        captured_checkpoint_id: None,
    };

    // Run checkpoint - should not crash even with paths outside repo
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = run_with_base_commit_override(
        &gitai_repo,
        "test_user",
        CheckpointKind::AiAgent,
        true,
        Some(checkpoint_request),
        false,
        None,
    );

    // Should succeed without crashing
    assert!(
        result.is_ok(),
        "Checkpoint should succeed even with paths outside repo: {:?}",
        result.err()
    );

    let (entries_len, files_len, _) = result.unwrap();
    // Should only process the valid file
    assert_eq!(files_len, 1, "Should process 1 valid file");
    assert_eq!(entries_len, 1, "Should create 1 entry");
}

#[test]
fn test_checkpoint_filters_external_paths_from_stored_checkpoints() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Get access to the working log storage
    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());

    // Manually inject a checkpoint with an external file path (simulating the bug)
    // This is what happens when a file outside the repo was tracked before the fix
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();

    let external_entry = WorkingLogEntry::new(
        "/external/path/outside/repo.txt".to_string(),
        "fake_sha_for_external".to_string(),
        vec![],
        vec![],
    );

    let fake_checkpoint = Checkpoint::new(
        CheckpointKind::Human,
        "fake_diff".to_string(),
        "test_author".to_string(),
        vec![external_entry],
    );

    // Store the checkpoint with external path
    working_log
        .append_checkpoint(&fake_checkpoint)
        .expect("Should be able to append checkpoint");

    // Now make actual changes to a file in the repo
    let file_path = repo.path().join(&lines_file);
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("New line for testing\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Run checkpoint - this should NOT crash even though there's an external path stored
    // Previously this would fail with: "fatal: /external/path/outside/repo.txt is outside repository"
    let result = repo.git_ai(&["checkpoint", "mock_known_human", &lines_file]);

    assert!(
        result.is_ok(),
        "Checkpoint should succeed even with external paths stored in previous checkpoints: {:?}",
        result.err()
    );

    // Verify the new checkpoint only processed the valid file
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    // Should only process the valid file in the repo
    assert_eq!(
        latest.entries.len(),
        1,
        "Should process 1 valid file (external path should be filtered)"
    );
}

#[test]
fn test_checkpoint_works_after_conflict_resolution_maintains_authorship() {
    // Create a repo with an initial commit
    let (repo, lines_file, _) = setup_repo_with_base_commit();

    // Get the current branch name (whatever the default is)
    let base_branch = repo.current_branch();

    // Checkpoint initial state to track the base authorship
    let file_path = repo.path().join(&lines_file);
    let initial_content = std::fs::read_to_string(&file_path).unwrap();
    println!("Initial content:\n{}", initial_content);

    // Create a branch and make changes
    repo.git(&["checkout", "-b", "feature-branch"]).unwrap();
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Feature line 1\n");
    content.push_str("Feature line 2\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", &lines_file])
        .unwrap();
    repo.stage_all_and_commit("Feature commit").unwrap();

    // Switch back to base branch and make conflicting changes
    repo.git(&["checkout", &base_branch]).unwrap();
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Main line 1\n");
    content.push_str("Main line 2\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();
    repo.stage_all_and_commit("Main commit").unwrap();

    // Attempt to merge feature-branch into base branch - this should create a conflict
    let output = repo.git_og(&["merge", "feature-branch"]);
    let has_conflicts = output.is_err();
    assert!(has_conflicts, "Should have merge conflicts");

    // While there are conflicts, checkpoint should skip the file
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints_before_conflict_checkpoint = working_log.read_all_checkpoints().unwrap();
    let count_before = checkpoints_before_conflict_checkpoint.len();

    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    // Checkpoint should skip conflicted files - check that either no new checkpoint was created
    // or the new checkpoint has 0 entries
    let checkpoints_after_conflict_checkpoint = working_log.read_all_checkpoints().unwrap();
    if checkpoints_after_conflict_checkpoint.len() > count_before {
        let checkpoint_during_conflict = checkpoints_after_conflict_checkpoint.last().unwrap();
        assert_eq!(
            checkpoint_during_conflict.entries.len(),
            0,
            "Should skip conflicted files during conflict"
        );
    }

    // Resolve the conflict by choosing "ours" (base branch)
    repo.git_og(&["checkout", "--ours", &lines_file]).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Verify content to ensure the resolution was applied correctly
    let resolved_content = std::fs::read_to_string(&file_path).unwrap();
    println!("Resolved content after resolution:\n{}", resolved_content);
    assert!(
        resolved_content.contains("Main line 1"),
        "Should contain base branch content (we chose 'ours')"
    );
    assert!(
        resolved_content.contains("Main line 2"),
        "Should contain base branch content (we chose 'ours')"
    );
    assert!(
        !resolved_content.contains("Feature line 1"),
        "Should not contain feature branch content (we chose 'ours')"
    );

    // After resolution, make additional changes to test that checkpointing works again
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("Post-resolution line 1\n");
    content.push_str("Post-resolution line 2\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    // Now checkpoint should work and track the new changes
    repo.git_ai(&["checkpoint", "mock_known_human", &lines_file])
        .unwrap();

    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    println!(
        "After resolution and new changes: entries_len={}",
        latest.entries.len()
    );

    // The file should be tracked with the new changes
    assert_eq!(
        latest.entries.len(),
        1,
        "Should create 1 entry for new changes after conflict resolution"
    );
}

#[test]
fn test_known_human_checkpoint_without_ai_history_records_h_hash_attributions() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("simple.txt"), "one\n").unwrap();
    repo.git(&["add", "simple.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "simple.txt"])
        .unwrap();
    repo.stage_all_and_commit("seed commit").unwrap();

    let file_path = repo.path().join("simple.txt");
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("two\n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", "simple.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "simple.txt"])
        .unwrap();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();
    let entry = latest
        .entries
        .iter()
        .find(|entry| entry.file == "simple.txt")
        .unwrap();

    // KnownHuman checkpoints always record h_<hash> line attributions, even with no AI history.
    // This allows downstream stats to count these lines as human_additions.
    assert!(
        !entry.line_attributions.is_empty(),
        "KnownHuman checkpoint should record line-level h_<hash> attributions"
    );
    assert!(
        entry
            .line_attributions
            .iter()
            .all(|la| la.author_id.starts_with("h_")),
        "All line attributions should be h_<hash> IDs"
    );
    assert!(
        latest.line_stats.additions > 0,
        "KnownHuman checkpoint should record line stats"
    );
}

#[test]
fn test_human_checkpoint_keeps_attributions_for_ai_touched_file() {
    let (repo, lines_file, alphabet_file) = setup_repo_with_base_commit();

    let lines_path = repo.path().join(&lines_file);
    let alphabet_path = repo.path().join(&alphabet_file);

    let mut content = std::fs::read_to_string(&lines_path).unwrap();
    content.push_str("ai change\n");
    std::fs::write(&lines_path, &content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", &lines_file])
        .unwrap();

    let mut lines_content = std::fs::read_to_string(&lines_path).unwrap();
    lines_content.push_str("human after ai\n");
    std::fs::write(&lines_path, &lines_content).unwrap();
    repo.git(&["add", &lines_file]).unwrap();

    let mut alphabet_content = std::fs::read_to_string(&alphabet_path).unwrap();
    alphabet_content.push_str("human only\n");
    std::fs::write(&alphabet_path, &alphabet_content).unwrap();
    repo.git(&["add", &alphabet_file]).unwrap();

    repo.git_ai(&[
        "checkpoint",
        "mock_known_human",
        &lines_file,
        &alphabet_file,
    ])
    .unwrap();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints.last().unwrap();

    let ai_touched_entry = latest
        .entries
        .iter()
        .find(|entry| entry.file == "lines.md")
        .unwrap();
    assert!(
        !ai_touched_entry.attributions.is_empty() || !ai_touched_entry.line_attributions.is_empty(),
        "AI-touched file should keep attribution tracking"
    );

    let human_only_entry = latest
        .entries
        .iter()
        .find(|entry| entry.file == "alphabet.md")
        .unwrap();
    // KnownHuman checkpoints record h_<hash> attributions for all files, including
    // files with no AI history. This ensures human lines are counted correctly in stats.
    assert!(
        !human_only_entry.line_attributions.is_empty(),
        "KnownHuman checkpoint should record line attributions for human-only files"
    );
    assert!(
        human_only_entry
            .line_attributions
            .iter()
            .all(|la| la.author_id.starts_with("h_")),
        "Human-only file attributions should all be h_<hash> IDs"
    );
}

#[test]
fn test_checkpoint_skips_default_ignored_files() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("README.md"), "# repo\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    std::fs::write(repo.path().join("README.md"), "# repo\n\nupdated\n").unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# lock\n# lock2\n").unwrap();

    // Checkpoint both files explicitly (CLI doesn't support "." the same way)
    repo.git(&["add", "README.md"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // Should have at least one checkpoint
    assert!(
        !checkpoints.is_empty(),
        "Should have at least one checkpoint"
    );
    let latest = checkpoints.last().unwrap();

    assert!(
        latest.entries.iter().any(|entry| entry.file == "README.md"),
        "Expected non-ignored source file to be checkpointed"
    );
    assert!(
        latest
            .entries
            .iter()
            .all(|entry| entry.file != "Cargo.lock"),
        "Expected Cargo.lock to be filtered by default ignore patterns"
    );
}

#[test]
fn test_checkpoint_skips_linguist_generated_files_from_root_gitattributes() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("README.md"), "# repo\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    std::fs::write(
        repo.path().join(".gitattributes"),
        "generated/** linguist-generated\n",
    )
    .unwrap();
    repo.git(&["add", ".gitattributes"]).unwrap();
    repo.stage_all_and_commit("attrs").unwrap();

    std::fs::create_dir_all(repo.path().join("generated")).unwrap();
    std::fs::write(
        repo.path().join("generated").join("api.generated.ts"),
        "// generated\n// generated 2\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.git(&["add", "main.rs"]).unwrap();

    // Checkpoint the non-generated file
    repo.git_ai(&["checkpoint", "mock_known_human", "main.rs"])
        .unwrap();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // Should have at least one checkpoint
    assert!(
        !checkpoints.is_empty(),
        "Should have at least one checkpoint"
    );
    let latest = checkpoints.last().unwrap();

    assert!(
        latest.entries.iter().any(|entry| entry.file == "main.rs"),
        "Expected non-generated file to be checkpointed"
    );
    assert!(
        latest
            .entries
            .iter()
            .all(|entry| entry.file != "generated/api.generated.ts"),
        "Expected linguist-generated file to be filtered via .gitattributes"
    );
}

#[test]
fn test_compute_line_stats_ignores_whitespace_only_lines() {
    let (repo, _lines_file, _alphabet_file) = setup_repo_with_base_commit();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");

    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();

    std::fs::write(repo.path().join("whitespace.txt"), "Seed line\n").unwrap();
    repo.git(&["add", "whitespace.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "whitespace.txt"])
        .expect("Setup checkpoint should succeed");

    let file_path = repo.path().join("whitespace.txt");
    let mut content = std::fs::read_to_string(&file_path).unwrap();
    content.push_str("\n\n   \nVisible line one\n\n\t\nVisible line two\n  \n");
    std::fs::write(&file_path, &content).unwrap();
    repo.git(&["add", "whitespace.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "whitespace.txt"])
        .expect("First checkpoint should succeed");

    let after_add_stats = working_log
        .read_all_checkpoints()
        .expect("Should read checkpoints after addition");
    let after_add_last = after_add_stats
        .last()
        .expect("At least one checkpoint expected")
        .line_stats
        .clone();

    assert_eq!(
        after_add_last.additions, 8,
        "Additions includes empty lines"
    );
    assert_eq!(after_add_last.deletions, 0, "No deletions expected yet");
    assert_eq!(
        after_add_last.additions_sloc, 2,
        "Only visible lines counted"
    );
    assert_eq!(
        after_add_last.deletions_sloc, 0,
        "No deletions expected yet"
    );

    let cleaned_content = std::fs::read_to_string(&file_path).unwrap();
    let cleaned_lines: Vec<&str> = cleaned_content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let cleaned_body = format!("{}\n", cleaned_lines.join("\n"));
    std::fs::write(&file_path, &cleaned_body).unwrap();
    repo.git(&["add", "whitespace.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "whitespace.txt"])
        .expect("Second checkpoint should succeed");

    let after_delete_stats = working_log
        .read_all_checkpoints()
        .expect("Should read checkpoints after deletion");
    let latest_stats = after_delete_stats
        .last()
        .expect("At least one checkpoint expected")
        .line_stats
        .clone();

    assert_eq!(
        latest_stats.additions, 0,
        "No additions in cleanup checkpoint"
    );
    assert_eq!(latest_stats.deletions, 6, "Deletions includes empty lines");
    assert_eq!(
        latest_stats.additions_sloc, 0,
        "No additions in cleanup checkpoint"
    );
    assert_eq!(
        latest_stats.deletions_sloc, 0,
        "Whitespace deletions ignored"
    );
}

// ====================================================================
// CRLF / LF normalization tests for compute_file_line_stats
// ====================================================================

#[test]
fn test_compute_file_line_stats_crlf_to_lf_no_changes() {
    // Same content, only line endings differ (CRLF → LF).
    // Stats should show 0 additions and 0 deletions.
    let old = "line1\r\nline2\r\nline3\r\n";
    let new = "line1\nline2\nline3\n";

    let stats = compute_file_line_stats(old, new);

    assert_eq!(
        stats.additions, 0,
        "CRLF→LF with identical content should show 0 additions"
    );
    assert_eq!(
        stats.deletions, 0,
        "CRLF→LF with identical content should show 0 deletions"
    );
}

#[test]
fn test_compute_file_line_stats_lf_to_crlf_no_changes() {
    let old = "line1\nline2\nline3\n";
    let new = "line1\r\nline2\r\nline3\r\n";

    let stats = compute_file_line_stats(old, new);

    assert_eq!(
        stats.additions, 0,
        "LF→CRLF with identical content should show 0 additions"
    );
    assert_eq!(
        stats.deletions, 0,
        "LF→CRLF with identical content should show 0 deletions"
    );
}

#[test]
fn test_compute_file_line_stats_crlf_to_lf_with_additions() {
    // Reproduces the user-reported bug: file with CRLF, AI adds lines with LF.
    // Old: 3 CRLF lines. New: same 3 lines (LF) + 2 new lines.
    // Should show exactly 2 additions and 0 deletions.
    let old = "line1\r\nline2\r\nline3\r\n";
    let new = "line1\nline2\nline3\nnew_a\nnew_b\n";

    let stats = compute_file_line_stats(old, new);

    assert_eq!(
        stats.additions, 2,
        "Should have exactly 2 additions (the new lines)"
    );
    assert_eq!(
        stats.deletions, 0,
        "Should have 0 deletions (no lines removed)"
    );
}

#[test]
fn test_compute_file_line_stats_crlf_large_file_user_reported_bug() {
    // Exact scenario from user report:
    // 100-line CRLF file, AI adds 5 lines (with LF).
    // Should show +5 -0, NOT +105 -100.
    let mut old = String::new();
    for i in 1..=100 {
        old.push_str(&format!("line number {}\r\n", i));
    }

    let mut new = String::new();
    for i in 1..=100 {
        new.push_str(&format!("line number {}\n", i));
    }
    for i in 1..=5 {
        new.push_str(&format!("new ai line {}\n", i));
    }

    let stats = compute_file_line_stats(&old, &new);

    assert_eq!(
        stats.additions, 5,
        "Should have exactly 5 additions (AI-added lines), not {}",
        stats.additions
    );
    assert_eq!(
        stats.deletions, 0,
        "Should have 0 deletions, not {}",
        stats.deletions
    );
}

// ====================================================================
// End-to-end CRLF test: blob has CRLF, working tree has LF
// Simulates the real-world scenario where git stores CRLF (or autocrlf
// converts on checkout) and an AI tool writes LF.
// ====================================================================

#[test]
fn test_checkpoint_crlf_blob_vs_lf_working_tree_stats_not_inflated() {
    // Step 1: Create a repo and commit a file with CRLF line endings.
    // On Linux without autocrlf, the blob stores CRLF verbatim.
    let repo = TestRepo::new();
    let crlf_content = "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\n";
    std::fs::write(repo.path().join("test.txt"), crlf_content).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("initial commit with CRLF")
        .unwrap();

    // Step 2: Overwrite the file with LF endings + one new line,
    // simulating an AI tool that writes LF on a Windows repo.
    let lf_content_with_addition = "line1\nline2\nline3\nline4\nline5\nnew_ai_line\n";
    std::fs::write(repo.path().join("test.txt"), lf_content_with_addition).unwrap();

    // Step 3: Run a checkpoint
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Step 4: Read back checkpoint stats
    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();
    let latest = checkpoints
        .last()
        .expect("Should have at least one checkpoint");

    // The key assertion: stats should reflect only the actual addition,
    // NOT inflate every line because of CRLF→LF conversion.
    assert_eq!(
        latest.line_stats.additions, 1,
        "Should have 1 addition (the new AI line), not {} (which would mean CRLF→LF inflated the count)",
        latest.line_stats.additions
    );
    assert_eq!(
        latest.line_stats.deletions, 0,
        "Should have 0 deletions, not {} (which would mean CRLF→LF caused all old lines to appear deleted)",
        latest.line_stats.deletions
    );
}

#[test]
fn test_checkpoint_crlf_blob_vs_lf_working_tree_no_changes_skipped() {
    // When the only difference is CRLF→LF (no actual content change),
    // the checkpoint should skip the file entirely — content_eq_normalized
    // detects they're equal and returns None.
    let repo = TestRepo::new();
    let crlf_content = "line1\r\nline2\r\nline3\r\n";
    std::fs::write(repo.path().join("test.txt"), crlf_content).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("initial commit with CRLF")
        .unwrap();

    // Overwrite with LF-only — same text content, different line endings
    let lf_content = "line1\nline2\nline3\n";
    std::fs::write(repo.path().join("test.txt"), lf_content).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // The checkpoint may be empty (no entries) or absent entirely,
    // because content_eq_normalized correctly detected no real change.
    if let Some(latest) = checkpoints.last() {
        let test_entry = latest.entries.iter().find(|e| e.file == "test.txt");
        assert!(
            test_entry.is_none(),
            "test.txt should be skipped when only line endings differ"
        );
    }
    // If no checkpoints at all, that's also correct — nothing changed.
}

#[test]
fn test_checkpoint_stale_crlf_blob_causes_ai_reattribution() {
    // Regression test for Devin review finding: when a CRLF-only change is
    // skipped (preserving a stale CRLF blob), the NEXT AI checkpoint compares
    // the stale CRLF blob against the LF working tree. Because
    // capture_diff_slices sees "line\r\n" ≠ "line\n", ALL lines appear changed.
    // With force_split=true in AI checkpoints, every "changed" line gets
    // re-attributed to AI — even human-written lines.
    //
    // The fix: when content differs only in line endings, update the blob
    // to LF (preserving attributions) so future diffs are LF-vs-LF.
    let repo = TestRepo::new();
    let crlf_initial = "human_line1\r\nhuman_line2\r\nhuman_line3\r\n";
    std::fs::write(repo.path().join("test.txt"), crlf_initial).unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("initial commit with CRLF")
        .unwrap();

    // Step 1: Human checkpoint on CRLF file → creates entry with CRLF blob
    // (need to add a line so the checkpoint creates an entry)
    let crlf_with_edit = "human_line1\r\nhuman_line2\r\nhuman_line3\r\nhuman_line4\r\n";
    std::fs::write(repo.path().join("test.txt"), crlf_with_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Step 2: Convert file to LF (same content, only line endings change)
    let lf_with_edit = "human_line1\nhuman_line2\nhuman_line3\nhuman_line4\n";
    std::fs::write(repo.path().join("test.txt"), lf_with_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Step 3: AI adds one line (LF) → AI checkpoint
    let lf_with_ai = "human_line1\nhuman_line2\nhuman_line3\nhuman_line4\nai_new_line\n";
    std::fs::write(repo.path().join("test.txt"), lf_with_ai).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    // Read the AI checkpoint
    let gitai_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("Repository should exist");
    let base_commit = gitai_repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| "initial".to_string());
    let working_log = gitai_repo
        .storage
        .working_log_for_base_commit(&base_commit)
        .unwrap();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // Find the AI checkpoint entry for test.txt
    let ai_checkpoint = checkpoints
        .iter()
        .rev()
        .find(|cp| cp.kind.is_ai() && cp.entries.iter().any(|e| e.file == "test.txt"))
        .expect("Should have an AI checkpoint with test.txt");
    let test_entry = ai_checkpoint
        .entries
        .iter()
        .find(|e| e.file == "test.txt")
        .unwrap();

    // The key assertion: the AI checkpoint should NOT attribute all lines to AI.
    // Only the actually-added line should be AI-attributed.
    let ai_line_attrs: Vec<_> = test_entry
        .line_attributions
        .iter()
        .filter(|la| is_ai_author_id(&la.author_id))
        .collect();

    // Count total lines covered by AI attributions
    let ai_line_count: u32 = ai_line_attrs
        .iter()
        .map(|la| la.end_line - la.start_line + 1)
        .sum();

    // AI should only attribute 1 line (the new ai_new_line), not all 5 lines.
    // If the stale CRLF blob caused full re-attribution, ai_line_count would be 5.
    assert!(
        ai_line_count <= 2,
        "AI should attribute at most 1-2 lines (the actual addition), \
         but attributed {} lines — stale CRLF blob caused full re-attribution. \
         AI attributions: {:?}, all attributions: {:?}",
        ai_line_count,
        ai_line_attrs,
        test_entry.line_attributions
    );
}
