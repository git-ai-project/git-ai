use crate::repos::test_repo::TestRepo;
use git_ai::authorship::working_log::AgentId;
use git_ai::daemon::checkpoint::build_agent_usage_attrs;
use git_ai::git::repository as GitAiRepository;

/// Verifies that `build_agent_usage_attrs` includes `repo_url` when the repo has a remote
/// with a normalizable URL (SSH or HTTPS format).
/// Regression test: previously, AgentUsage events were emitted before the repo was
/// discovered, so repo_url was never set.
#[test]
fn test_agent_usage_attrs_include_repo_url_when_remote_exists() {
    let repo = TestRepo::new();

    // Set up an origin remote with an SSH-style URL (normalizable to HTTPS)
    repo.git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:test-org/test-repo.git",
    ])
    .unwrap();

    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("should open repo");

    let agent_id = AgentId {
        id: "test-session-123".to_string(),
        tool: "claude-code".to_string(),
        model: "claude-sonnet-4-20250514".to_string(),
    };

    let attrs = build_agent_usage_attrs(&gitai_repo, &agent_id);

    // repo_url should be normalized to HTTPS
    assert_eq!(
        attrs.repo_url,
        Some(Some("https://github.com/test-org/test-repo".to_string())),
        "AgentUsage attrs must include normalized repo_url"
    );

    // Also verify tool, model, session_id are populated
    assert!(
        matches!(&attrs.tool, Some(Some(t)) if t == "claude-code"),
        "attrs.tool should be 'claude-code', got: {:?}",
        attrs.tool
    );
    assert!(
        matches!(&attrs.model, Some(Some(m)) if m == "claude-sonnet-4-20250514"),
        "attrs.model should be set, got: {:?}",
        attrs.model
    );
    assert!(
        matches!(&attrs.session_id, Some(Some(s)) if !s.is_empty()),
        "attrs.session_id should be set, got: {:?}",
        attrs.session_id
    );
    assert!(
        matches!(&attrs.prompt_id, Some(Some(p)) if !p.is_empty()),
        "attrs.prompt_id should be set, got: {:?}",
        attrs.prompt_id
    );
    assert!(
        matches!(&attrs.external_prompt_id, Some(Some(p)) if p == "test-session-123"),
        "attrs.external_prompt_id should match agent_id.id, got: {:?}",
        attrs.external_prompt_id
    );
}

/// Verifies that `build_agent_usage_attrs` normalizes HTTPS remote URLs.
#[test]
fn test_agent_usage_attrs_normalizes_https_remote_url() {
    let repo = TestRepo::new();

    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/my-company/my-project.git",
    ])
    .unwrap();

    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("should open repo");

    let agent_id = AgentId {
        id: "session-456".to_string(),
        tool: "cursor".to_string(),
        model: "gpt-4".to_string(),
    };

    let attrs = build_agent_usage_attrs(&gitai_repo, &agent_id);

    // Should strip .git suffix
    assert_eq!(
        attrs.repo_url,
        Some(Some("https://github.com/my-company/my-project".to_string())),
        "AgentUsage attrs must normalize HTTPS repo_url (strip .git)"
    );
}

/// Verifies that `build_agent_usage_attrs` includes `branch` when on a branch.
#[test]
fn test_agent_usage_attrs_include_branch() {
    use std::fs;

    let repo = TestRepo::new();

    // Need at least one commit so HEAD resolves to a branch
    fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial commit").unwrap();

    repo.git(&["remote", "add", "origin", "git@github.com:org/repo.git"])
        .unwrap();

    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("should open repo");

    let agent_id = AgentId {
        id: "session-789".to_string(),
        tool: "cursor".to_string(),
        model: "gpt-4".to_string(),
    };

    let attrs = build_agent_usage_attrs(&gitai_repo, &agent_id);

    // branch should be "main"
    assert_eq!(
        attrs.branch,
        Some(Some("main".to_string())),
        "AgentUsage attrs must include branch"
    );
}

/// Verifies that `build_agent_usage_attrs` gracefully handles repos without a remote.
#[test]
fn test_agent_usage_attrs_no_remote_still_works() {
    let repo = TestRepo::new();

    let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("should open repo");

    let agent_id = AgentId {
        id: "session-000".to_string(),
        tool: "windsurf".to_string(),
        model: "claude-opus-4-20250514".to_string(),
    };

    let attrs = build_agent_usage_attrs(&gitai_repo, &agent_id);

    // repo_url should NOT be set (no remote configured)
    assert!(
        attrs.repo_url.is_none() || matches!(&attrs.repo_url, Some(None)),
        "AgentUsage attrs should not have repo_url when no remote exists, got: {:?}",
        attrs.repo_url
    );

    // But tool/model/session_id should still be set
    assert!(
        matches!(&attrs.tool, Some(Some(t)) if t == "windsurf"),
        "attrs.tool should be set"
    );
    assert!(
        matches!(&attrs.model, Some(Some(m)) if m == "claude-opus-4-20250514"),
        "attrs.model should be set"
    );
}

/// Verifies that `build_agent_usage_attrs` gracefully handles repos with a local-path remote
/// (which cannot be normalized to HTTPS).
#[test]
fn test_agent_usage_attrs_local_path_remote_no_repo_url() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    let gitai_repo = GitAiRepository::find_repository_in_path(mirror.path().to_str().unwrap())
        .expect("should open repo");

    let agent_id = AgentId {
        id: "session-local".to_string(),
        tool: "claude-code".to_string(),
        model: "claude-sonnet-4-20250514".to_string(),
    };

    let attrs = build_agent_usage_attrs(&gitai_repo, &agent_id);

    // Local path remotes (like /tmp/...) cannot be normalized, so repo_url should be absent
    assert!(
        attrs.repo_url.is_none(),
        "AgentUsage attrs should not have repo_url for local-path remotes, got: {:?}",
        attrs.repo_url
    );

    // But other attrs should still be set correctly
    assert!(
        matches!(&attrs.tool, Some(Some(t)) if t == "claude-code"),
        "attrs.tool should be set"
    );
}
