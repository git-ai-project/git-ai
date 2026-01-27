#[macro_use]
mod repos;
mod test_utils;

use std::collections::HashMap;

use git_ai::{
    error::GitAiError,
    git::{find_repository, repository::exec_git},
};
use repos::test_repo::TestRepo;

/// Helper to get the git-ai Repository from a TestRepo
fn get_git_ai_repo(repo: &TestRepo) -> git_ai::git::repository::Repository {
    let args = vec!["-C".to_string(), repo.path().to_str().unwrap().to_string()];
    find_repository(&args).expect("Failed to find repo")
}
/// Helper to get git config via CLI for comparison
fn get_git_config_cli(repo: &TestRepo, command: &str, key: &str) -> Result<String, String> {
    repo.git_og(&["config", command, key])
}

fn git_config_cli_regexp(
    repo: &TestRepo,
    command: &str,
    key: &str,
) -> Result<HashMap<String, String>, String> {
    let mut result = HashMap::new();
    let output = get_git_config_cli(repo, "--get-regexp", key)?;
    for line in output.lines() {
        // Format: "key value" (space-separated)
        if let Some((key, value)) = line.split_once(' ') {
            result.insert(key.to_string(), value.to_string());
        }
    }
    Ok(result)
}

// ============================================================================
// config_get_str tests
// ============================================================================

#[test]
fn test_config_get_str_simple_value() {
    let repo = TestRepo::new();
    let key = "custom.key";

    repo.git(&["config", key, "custom_value"]).unwrap();

    let git_ai_repo = get_git_ai_repo(&repo);
    let result = git_ai_repo
        .config_get_str(key)
        .expect("Failed to get custom.key value")
        .unwrap();
    let git_config_result = get_git_config_cli(&repo, "--get", key).unwrap();

    // compare with trimmed git config --get output
    assert_eq!(result, git_config_result.trim());

    assert_eq!(result, "custom_value".to_string());
}

#[test]
fn test_config_get_str_subsection() {
    let repo = TestRepo::new();
    let key = "custom.sub.key";

    repo.git(&["config", key, "custom_value"]).unwrap();

    let git_ai_repo = get_git_ai_repo(&repo);
    let result = git_ai_repo
        .config_get_str(key)
        .expect("Failed to get custom.key value")
        .unwrap();

    let git_config_result = get_git_config_cli(&repo, "--get", key).unwrap();

    // compare with trimmed git config --get output
    assert_eq!(result, git_config_result.trim());
}

#[test]
fn test_config_get_str_missing_key_returns_none() {
    let repo = TestRepo::new();
    let git_ai_repo = get_git_ai_repo(&repo);

    // Non-existent key should return None (same as git config --get exit code 1)
    let result = git_ai_repo.config_get_str("nonexistent.key").unwrap();
    assert_eq!(result, None);
}

#[test]
fn test_config_get_str_special_chars() {
    let repo = TestRepo::new();
    let name_key = "user.name";
    let alias_key = "alias.lg";

    repo.git(&["config", name_key, "Test User <test@example.com>"])
        .unwrap();
    repo.git(&["config", alias_key, "log --oneline --graph"])
        .unwrap();

    let git_ai_repo = get_git_ai_repo(&repo);
    let name_result = git_ai_repo
        .config_get_str(name_key)
        .expect("Failed to get custom.key value")
        .unwrap();

    // compare with trimmed git config --get output
    assert_eq!(
        name_result,
        get_git_config_cli(&repo, "--get", name_key).unwrap().trim()
    );
    let alias_result = git_ai_repo
        .config_get_str(alias_key)
        .expect("Failed to get custom.key value")
        .unwrap();

    // compare with trimmed git config --get output
    assert_eq!(
        alias_result,
        get_git_config_cli(&repo, "--get", alias_key)
            .unwrap()
            .trim()
    );
}

// ============================================================================
// config_get_regexp tests
// ============================================================================

#[test]
fn test_config_get_regexp_subsection() {
    let repo = TestRepo::new();
    let key = "custom.sub.testkey";
    let pattern = "test";

    repo.git(&["config", key, "custom_value"]).unwrap();

    let git_ai_repo = get_git_ai_repo(&repo);
    let result = git_ai_repo
        .config_get_regexp(pattern)
        .expect("Failed to match pattern");

    let git_config_result = git_config_cli_regexp(&repo, "--get-regexp", pattern).unwrap();

    // compare with trimmed git config --get-regexp output
    assert_eq!(result, git_config_result);
}

#[test]
fn test_config_get_regexp_no_matches() {
    let repo = TestRepo::new();
    let pattern = "nonexistant";
    let git_ai_repo = get_git_ai_repo(&repo);
    let result = git_ai_repo
        .config_get_regexp(pattern)
        .expect("Failed to match pattern");
    assert!(result.is_empty());
}

#[test]
fn test_config_get_regexp_with_subsections() {
    let repo = TestRepo::new();
    let git_ai_repo = get_git_ai_repo(&repo);

    // Set up remotes using TestRepo's git method
    repo.git(&[
        "config",
        "remote.origin.url",
        "https://github.com/test/repo.git",
    ])
    .unwrap();
    repo.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ])
    .unwrap();
    repo.git(&[
        "config",
        "remote.upstream.url",
        "https://github.com/upstream/repo.git",
    ])
    .unwrap();

    // Match all remote.*.url keys
    let result = git_ai_repo.config_get_regexp(r"^remote\..*\.url$").unwrap();

    assert_eq!(result.len(), 2);
    assert!(result.contains_key("remote.origin.url"));
    assert!(result.contains_key("remote.upstream.url"));
}

#[test]
fn test_config_get_regexp_case_insensitive_keys() {
    let repo = TestRepo::new();
    let key = "Core.AutoCRLF";
    let value = "true";

    repo.git(&["config", key, value]).unwrap();
    let git_ai_repo = get_git_ai_repo(&repo);

    // Our implementation normalizes to lowercase
    let result = git_ai_repo.config_get_regexp(r"^core\.autocrlf$").unwrap();
    assert!(
        result.contains_key("core.autocrlf"),
        "Expected core.autocrlf in lowercase, got: {:?}",
        result.keys()
    );

    // Also compare to actual git config command output
    let git_config_result =
        git_config_cli_regexp(&repo, "--get-regexp", r"^core\.autocrlf$").unwrap();

    assert_eq!(result, git_config_result);
}
