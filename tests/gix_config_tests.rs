#[macro_use]
mod repos;
mod test_utils;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};


use git_ai::git::repository as GitAiRepository;
use repos::test_repo::TestRepo;

struct TempGlobalConfig {
    _lock: MutexGuard<'static, ()>,
}

impl TempGlobalConfig {
    fn new() -> Self {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        static CONFIG_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
        let lock = ENV_LOCK.lock().expect("env lock");
        let _path = CONFIG_PATH.get_or_init(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let config_path = dir.path().join("gitconfig");
            let _ = std::fs::write(&config_path, "");
            // Safe in tests: one-time setup for this test binary.
            unsafe {
                std::env::set_var("GIT_CONFIG_GLOBAL", &config_path);
            }
            std::mem::forget(dir);
            config_path
        });
        Self { _lock: lock }
    }
}

/// Helper to get git config via CLI for comparison
fn get_git_config_cli(repo: &TestRepo, _command: &str, key: &str) -> Result<String, String> {
    repo.git_og(&["config", "--get", key])
}

fn git_config_cli_regexp(
    repo: &TestRepo,
    _command: &str,
    key: &str,
) -> Result<HashMap<String, String>, String> {
    let mut result = HashMap::new();
    let output = repo.git_og(&["config", "--get-regexp", key])?;
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

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
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

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
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
    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

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

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
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

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
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
    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = git_ai_repo
        .config_get_regexp(pattern)
        .expect("Failed to match pattern");
    assert!(result.is_empty());
}

#[test]
fn test_config_get_regexp_with_subsections() {
    let repo = TestRepo::new();
    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

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
    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

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

// ============================================================================
// Global config fallback tests
// ============================================================================

#[test]
fn test_config_falls_back_to_global() {
    let _temp_global = TempGlobalConfig::new();
    let repo = TestRepo::new();

    // Use a unique key to avoid conflicts with real config
    let test_key = "gitaici.globalcheck";
    let global_value = "GLOBAL_CI_VALUE_12345";

    // Set a global value for our test key
    repo.git_og(&["config", "--global", test_key, global_value])
        .expect("Failed to set global config");

    // Ensure no local value exists
    let _ = repo.git(&["config", "--local", "--unset", test_key]);

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = git_ai_repo.config_get_str(test_key).unwrap();

    // Clean up global config
    let _ = repo.git_og(&["config", "--global", "--unset", test_key]);

    assert_eq!(result, Some(global_value.to_string()));
}

#[test]
fn test_config_local_overrides_global() {
    let repo = TestRepo::new();

    // Get global value (may or may not exist)
    let global_value = repo
        .git_og(&["config", "--global", "--get", "user.name"])
        .ok()
        .map(|s| s.trim().to_string());

    let local_value = "TEST_LOCAL_USER_12345";

    // Test is invalid if local happens to match global
    if global_value.as_deref() == Some(local_value) {
        panic!("Test invalid: local value matches global");
    }

    repo.git(&["config", "--local", "user.name", local_value])
        .unwrap();

    let git_ai_repo =
        GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let result = git_ai_repo.config_get_str("user.name").unwrap();

    assert_eq!(result, Some(local_value.to_string()));
}

// ============================================================================
// Bare repository tests
// ============================================================================

#[test]
fn test_config_get_str_bare_repo() {
    let repo = TestRepo::new_bare();
    let key = "custom.baretest";

    repo.git(&["config", key, "bare_value"]).unwrap();

    let git_ai_repo = GitAiRepository::from_bare_repository(repo.path()).unwrap();
    let result = git_ai_repo.config_get_str(key).unwrap();

    assert_eq!(result, Some("bare_value".to_string()));
}

#[test]
fn test_config_get_regexp_bare_repo() {
    let repo = TestRepo::new_bare();

    repo.git(&["config", "baretest.key1", "value1"]).unwrap();
    repo.git(&["config", "baretest.key2", "value2"]).unwrap();

    let git_ai_repo = GitAiRepository::from_bare_repository(repo.path()).unwrap();
    let result = git_ai_repo.config_get_regexp(r"^baretest\.").unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(result.get("baretest.key1"), Some(&"value1".to_string()));
    assert_eq!(result.get("baretest.key2"), Some(&"value2".to_string()));
}

worktree_test_wrappers! {
    test_config_get_str_simple_value,
    test_config_get_str_subsection,
    test_config_get_str_missing_key_returns_none,
    test_config_get_str_special_chars,
    test_config_get_regexp_subsection,
    test_config_get_regexp_no_matches,
    test_config_get_regexp_with_subsections,
    test_config_get_regexp_case_insensitive_keys,
    test_config_falls_back_to_global,
    test_config_local_overrides_global,
    test_config_get_str_bare_repo,
    test_config_get_regexp_bare_repo,
}
