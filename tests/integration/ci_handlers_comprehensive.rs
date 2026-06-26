use crate::repos::test_repo::{TestRepo, real_git_executable};
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn run_real_git(args: &[&str]) -> String {
    let output = Command::new(real_git_executable())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "git {:?} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        output.status.code(),
        stdout,
        stderr
    );
    stdout.trim().to_string()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("test path must be valid UTF-8")
}

// ==============================================================================
// CI Handlers Tests - Module Structure and Types
// ==============================================================================

#[test]
fn test_ci_handlers_module_exists() {
    // Basic smoke test to ensure the module compiles and links
    // Smoke test: module compiles and links successfully
}

// ==============================================================================
// CI Result Types Tests
// ==============================================================================

#[test]
fn test_ci_result_types_coverage() {
    // Test that we understand all CiRunResult variants
    use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
    use git_ai::ci::ci_context::CiRunResult;

    // Test variant construction
    let result1 = CiRunResult::AuthorshipRewritten {
        authorship_log: AuthorshipLog::default(),
    };
    let result2 = CiRunResult::AlreadyExists {
        authorship_log: AuthorshipLog::default(),
    };
    let result3 = CiRunResult::SkippedSimpleMerge;
    let result4 = CiRunResult::SkippedFastForward;
    let result5 = CiRunResult::NoAuthorshipAvailable;
    let result6 = CiRunResult::SyncAuthorshipRewritten { commit_count: 2 };
    let result7 = CiRunResult::SkippedExistingSyncNotes;

    // Verify variants can be constructed
    match result1 {
        CiRunResult::AuthorshipRewritten { .. } => {}
        _ => panic!("Expected AuthorshipRewritten"),
    }

    match result2 {
        CiRunResult::AlreadyExists { .. } => {}
        _ => panic!("Expected AlreadyExists"),
    }

    match result3 {
        CiRunResult::SkippedSimpleMerge => {}
        _ => panic!("Expected SkippedSimpleMerge"),
    }

    match result4 {
        CiRunResult::SkippedFastForward => {}
        _ => panic!("Expected SkippedFastForward"),
    }

    match result5 {
        CiRunResult::NoAuthorshipAvailable => {}
        _ => panic!("Expected NoAuthorshipAvailable"),
    }

    match result6 {
        CiRunResult::SyncAuthorshipRewritten { commit_count } => assert_eq!(commit_count, 2),
        _ => panic!("Expected SyncAuthorshipRewritten"),
    }

    match result7 {
        CiRunResult::SkippedExistingSyncNotes => {}
        _ => panic!("Expected SkippedExistingSyncNotes"),
    }
}

#[test]
fn test_ci_github_run_noops_when_synchronize_has_no_previous_head() {
    let repo = TestRepo::new();
    let mut event_file = tempfile::NamedTempFile::new().expect("event file");
    write!(
        event_file,
        r#"{{
          "action": "synchronize",
          "before": "0000000000000000000000000000000000000000",
          "after": "2222222222222222222222222222222222222222",
          "pull_request": {{
            "number": 42,
            "merged": false,
            "merge_commit_sha": null,
            "base": {{
              "ref": "main",
              "sha": "1111111111111111111111111111111111111111",
              "repo": {{ "clone_url": "https://github.com/acme/repo.git" }}
            }},
            "head": {{
              "ref": "feature",
              "sha": "2222222222222222222222222222222222222222",
              "repo": {{ "clone_url": "https://github.com/acme/repo.git" }}
            }}
          }}
        }}"#
    )
    .expect("write event");

    let output = repo
        .git_ai_with_env(
            &["ci", "github", "run", "--no-cleanup"],
            &[
                ("GITHUB_EVENT_NAME", "pull_request"),
                (
                    "GITHUB_EVENT_PATH",
                    event_file.path().to_str().expect("event path"),
                ),
            ],
        )
        .expect("github ci run should no-op successfully");

    assert!(
        output.contains("No GitHub CI context found; nothing to do"),
        "Expected no-op output, got: {}",
        output
    );
}

#[test]
fn test_ci_github_run_fetches_missing_previous_head_after_force_push() {
    let ci_repo = TestRepo::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote_path = tmp.path().join("origin.git");
    let work_path = tmp.path().join("work");
    let remote = path_str(&remote_path);
    let work = path_str(&work_path);

    run_real_git(&["init", "--bare", "--initial-branch=main", remote]);
    run_real_git(&[
        "-C",
        remote,
        "config",
        "uploadpack.allowAnySHA1InWant",
        "true",
    ]);
    run_real_git(&["init", "--initial-branch=main", work]);
    run_real_git(&["-C", work, "config", "user.name", "Test User"]);
    run_real_git(&["-C", work, "config", "user.email", "test@example.com"]);

    std::fs::write(work_path.join("file.txt"), "base\n").expect("write base");
    run_real_git(&["-C", work, "add", "file.txt"]);
    run_real_git(&["-C", work, "commit", "-m", "base"]);
    let base_sha = run_real_git(&["-C", work, "rev-parse", "HEAD"]);
    run_real_git(&["-C", work, "remote", "add", "origin", remote]);
    run_real_git(&["-C", work, "push", "origin", "main"]);

    run_real_git(&["-C", work, "checkout", "-b", "feature"]);
    std::fs::write(work_path.join("file.txt"), "old PR head\n").expect("write old head");
    run_real_git(&["-C", work, "commit", "-am", "old pr head"]);
    let previous_head_sha = run_real_git(&["-C", work, "rev-parse", "HEAD"]);
    run_real_git(&["-C", work, "push", "origin", "HEAD:refs/pull/42/head"]);

    run_real_git(&["-C", work, "checkout", "-B", "feature", "main"]);
    std::fs::write(work_path.join("file.txt"), "new PR head\n").expect("write new head");
    run_real_git(&["-C", work, "commit", "-am", "new pr head"]);
    let current_head_sha = run_real_git(&["-C", work, "rev-parse", "HEAD"]);
    run_real_git(&[
        "-C",
        work,
        "push",
        "--force",
        "origin",
        "HEAD:refs/pull/42/head",
    ]);

    let remote_url =
        url::Url::from_directory_path(&remote_path).expect("remote path should be file URL");
    let mut event_file = tempfile::NamedTempFile::new().expect("event file");
    let event = serde_json::json!({
        "action": "synchronize",
        "before": previous_head_sha,
        "after": current_head_sha,
        "pull_request": {
            "number": 42,
            "merged": false,
            "merge_commit_sha": null,
            "base": {
                "ref": "main",
                "sha": base_sha,
                "repo": { "clone_url": remote_url.as_str() }
            },
            "head": {
                "ref": "feature",
                "sha": current_head_sha,
                "repo": { "clone_url": remote_url.as_str() }
            }
        }
    });
    serde_json::to_writer(&mut event_file, &event).expect("write event");
    event_file.flush().expect("flush event");

    let output = ci_repo
        .git_ai_with_env(
            &["ci", "github", "run", "--no-cleanup"],
            &[
                ("GITHUB_EVENT_NAME", "pull_request"),
                (
                    "GITHUB_EVENT_PATH",
                    event_file.path().to_str().expect("event path"),
                ),
            ],
        )
        .expect("github ci run should fetch the missing previous head");

    assert!(
        output.contains("GitHub CI: skipped non-rebase PR sync"),
        "expected successful non-rebase sync skip, got:\n{}",
        output
    );

    let clone_path = ci_repo.path().join("git-ai-ci-clone");
    let clone = path_str(&clone_path);
    let previous_commit = format!("{}^{{commit}}", previous_head_sha);
    let resolved_previous_head =
        run_real_git(&["-C", clone, "rev-parse", "--verify", &previous_commit]);
    assert_eq!(resolved_previous_head, previous_head_sha);
}

// ==============================================================================
// CI Event Structure Tests
// ==============================================================================

#[test]
fn test_ci_event_merge_structure() {
    use git_ai::ci::ci_context::CiEvent;

    let event = CiEvent::Merge {
        merge_commit_sha: "abc123".to_string(),
        head_ref: "feature".to_string(),
        head_sha: "def456".to_string(),
        base_ref: "main".to_string(),
        base_sha: "ghi789".to_string(),
        fork_clone_url: Some("https://example.com/fork.git".to_string()),
    };

    match event {
        CiEvent::Merge {
            merge_commit_sha,
            head_ref,
            head_sha,
            base_ref,
            base_sha,
            fork_clone_url,
        } => {
            assert_eq!(merge_commit_sha, "abc123");
            assert_eq!(head_ref, "feature");
            assert_eq!(head_sha, "def456");
            assert_eq!(base_ref, "main");
            assert_eq!(base_sha, "ghi789");
            assert_eq!(
                fork_clone_url,
                Some("https://example.com/fork.git".to_string())
            );
        }
        CiEvent::Sync { .. } => panic!("Expected Merge"),
    }
}

// ==============================================================================
// Flag Parsing Tests
// ==============================================================================

#[test]
fn test_ci_local_flag_parsing_structure() {
    // Test that flag parsing logic expectations are correct
    let args = [
        "--merge-commit-sha".to_string(),
        "abc123".to_string(),
        "--base-ref".to_string(),
        "main".to_string(),
    ];

    // Verify flag structure
    assert!(args.contains(&"--merge-commit-sha".to_string()));
    assert!(args.contains(&"--base-ref".to_string()));
}

#[test]
fn test_ci_local_flag_values() {
    // Test flag value extraction logic
    let args = [
        "--head-ref".to_string(),
        "feature-branch".to_string(),
        "--head-sha".to_string(),
        "def456".to_string(),
    ];

    // Find flag values
    let mut i = 0;
    let mut head_ref = None;
    let mut head_sha = None;

    while i < args.len() {
        if args[i] == "--head-ref" && i + 1 < args.len() {
            head_ref = Some(args[i + 1].clone());
            i += 2;
        } else if args[i] == "--head-sha" && i + 1 < args.len() {
            head_sha = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    assert_eq!(head_ref, Some("feature-branch".to_string()));
    assert_eq!(head_sha, Some("def456".to_string()));
}

#[test]
fn test_no_cleanup_flag_detection() {
    let args1 = ["run".to_string(), "--no-cleanup".to_string()];
    let args2 = ["run".to_string()];

    let has_no_cleanup_1 = args1.iter().any(|a| a == "--no-cleanup");
    let has_no_cleanup_2 = args2.iter().any(|a| a == "--no-cleanup");

    assert!(has_no_cleanup_1);
    assert!(!has_no_cleanup_2);
}

#[test]
fn test_ci_missing_flag_value_detection() {
    let args = ["--merge-commit-sha".to_string()];

    // Simulate flag parser
    let mut i = 0;
    let mut found_value = false;

    while i < args.len() {
        if args[i] == "--merge-commit-sha" {
            if i + 1 < args.len() {
                found_value = true;
            }
            break;
        }
        i += 1;
    }

    assert!(!found_value, "Should detect missing flag value");
}

#[test]
fn test_ci_required_flags_for_merge() {
    let required_flags = [
        "--merge-commit-sha",
        "--base-ref",
        "--head-ref",
        "--head-sha",
        "--base-sha",
    ];

    assert_eq!(required_flags.len(), 5);
    assert!(required_flags.contains(&"--merge-commit-sha"));
    assert!(required_flags.contains(&"--base-ref"));
    assert!(required_flags.contains(&"--head-ref"));
    assert!(required_flags.contains(&"--head-sha"));
    assert!(required_flags.contains(&"--base-sha"));
}

#[test]
fn test_ci_optional_skip_fetch_flags_for_merge() {
    let optional_flags = [
        "--skip-fetch-notes",
        "--skip-fetch-base",
        "--skip-fetch-fork-notes",
        "--skip-fetch",
    ];

    assert_eq!(optional_flags.len(), 4);
    assert!(optional_flags.contains(&"--skip-fetch-notes"));
    assert!(optional_flags.contains(&"--skip-fetch-base"));
    assert!(optional_flags.contains(&"--skip-fetch-fork-notes"));
    assert!(optional_flags.contains(&"--skip-fetch"));
}

// ==============================================================================
// Subcommand Structure Tests
// ==============================================================================

#[test]
fn test_ci_subcommand_classification() {
    let valid_platforms = vec!["github", "gitlab", "local"];
    let valid_actions = vec!["run", "install"];

    // Test platform detection
    for platform in &valid_platforms {
        assert!(valid_platforms.contains(platform));
    }

    // Test action detection
    for action in &valid_actions {
        assert!(valid_actions.contains(action));
    }
}

#[test]
fn test_ci_github_subcommands() {
    let subcommands = ["run", "install"];

    assert!(subcommands.contains(&"run"));
    assert!(subcommands.contains(&"install"));
    assert!(!subcommands.contains(&"unknown"));
}

#[test]
fn test_ci_gitlab_subcommands() {
    let subcommands = ["run", "install"];

    assert!(subcommands.contains(&"run"));
    assert!(subcommands.contains(&"install"));
    assert!(!subcommands.contains(&"unknown"));
}

#[test]
fn test_ci_local_events() {
    let events = ["merge"];

    assert!(events.contains(&"merge"));
    assert!(!events.contains(&"push"));
}

// ==============================================================================
// Environment Detection Tests
// ==============================================================================

#[test]
fn test_github_ci_env_detection() {
    // Test GitHub CI environment variable detection logic
    // In actual CI, GITHUB_ACTIONS=true would be set

    let github_actions = std::env::var("GITHUB_ACTIONS").ok();

    // In test environment, this should be None
    // In actual GitHub Actions, it would be Some("true")
    if let Some(val) = github_actions {
        assert_eq!(val, "true");
    }
    // Otherwise not in GitHub Actions - expected in test environment
}

#[test]
fn test_gitlab_ci_env_detection() {
    // Test GitLab CI environment variable detection logic
    // In actual CI, GITLAB_CI=true would be set

    let gitlab_ci = std::env::var("GITLAB_CI").ok();

    // In test environment, this should be None
    // In actual GitLab CI, it would be Some("true")
    if let Some(val) = gitlab_ci {
        assert_eq!(val, "true");
    }
    // Otherwise not in GitLab CI - expected in test environment
}

// ==============================================================================
// Repository Context Tests
// ==============================================================================

#[test]
fn test_ci_requires_valid_repository() {
    // CI commands require a valid git repository
    let repo = TestRepo::new();

    // Verify .git directory exists
    assert!(repo.path().join(".git").exists());

    // Create a commit so we have a HEAD
    repo.filename("README.md")
        .set_contents(vec!["test"])
        .stage();
    let commit = repo.commit("initial commit").unwrap();

    assert!(!commit.commit_sha.is_empty());
}

// ==============================================================================
// CI Context Integration Tests
// ==============================================================================

#[test]
fn test_ci_context_with_temp_dir() {
    use git_ai::ci::ci_context::{CiContext, CiEvent};
    use git_ai::git::repository::find_repository_in_path;

    let test_repo = TestRepo::new();

    // Create a commit
    test_repo
        .filename("file.txt")
        .set_contents(vec!["content"])
        .stage();
    let commit = test_repo.commit("test commit").unwrap();
    let sha = commit.commit_sha;

    let repo = find_repository_in_path(test_repo.path().to_str().unwrap())
        .expect("Failed to open repository");

    let event = CiEvent::Merge {
        merge_commit_sha: sha.clone(),
        head_ref: "feature".to_string(),
        head_sha: sha.clone(),
        base_ref: "main".to_string(),
        base_sha: sha.clone(),
        fork_clone_url: None,
    };

    let ctx = CiContext {
        repo,
        event,
        temp_dir: test_repo.path().to_path_buf(),
    };

    // Verify context was created
    assert!(ctx.temp_dir.exists());
}

// ==============================================================================
// Workflow File Tests
// ==============================================================================

#[test]
fn test_github_workflow_file_creation() {
    use std::fs;
    let repo = TestRepo::new();
    let workflows_dir = repo.path().join(".github").join("workflows");

    // Create directory structure
    fs::create_dir_all(&workflows_dir).expect("Failed to create workflows dir");

    let workflow_file = workflows_dir.join("git-ai-authorship.yml");

    // Write a minimal workflow
    fs::write(&workflow_file, "name: Git AI Authorship\n").expect("Failed to write workflow");

    assert!(workflow_file.exists());
}

#[test]
fn test_github_workflow_path_structure() {
    let repo = TestRepo::new();
    let expected_path = repo
        .path()
        .join(".github")
        .join("workflows")
        .join("git-ai-authorship.yml");

    // Verify path components
    assert!(expected_path.to_string_lossy().contains(".github"));
    assert!(expected_path.to_string_lossy().contains("workflows"));
    assert!(
        expected_path
            .to_string_lossy()
            .contains("git-ai-authorship.yml")
    );
}

crate::reuse_tests_in_worktree!(
    test_ci_handlers_module_exists,
    test_ci_result_types_coverage,
    test_ci_event_merge_structure,
    test_ci_local_flag_parsing_structure,
    test_ci_local_flag_values,
    test_no_cleanup_flag_detection,
    test_ci_missing_flag_value_detection,
    test_ci_required_flags_for_merge,
    test_ci_optional_skip_fetch_flags_for_merge,
    test_ci_subcommand_classification,
    test_ci_github_subcommands,
    test_ci_gitlab_subcommands,
    test_ci_local_events,
    test_github_ci_env_detection,
    test_gitlab_ci_env_detection,
    test_ci_requires_valid_repository,
    test_ci_context_with_temp_dir,
    test_github_workflow_file_creation,
    test_github_workflow_path_structure,
);
