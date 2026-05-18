use crate::repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_config_show_empty() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["config"])
        .expect("config with no args should succeed");
    let parsed: serde_json::Value =
        serde_json::from_str(output.trim()).expect("output should be valid JSON");
    assert!(parsed.is_object(), "empty config should be an object");
}

#[test]
fn test_config_set_and_get_string() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "api_key", "sk-test-123456789"])
        .expect("config set should succeed");

    let output = repo
        .git_ai(&["config", "api_key"])
        .expect("config get should succeed");
    assert_eq!(output.trim(), "\"sk-test-123456789\"");
}

#[test]
fn test_config_set_and_get_bool() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "quiet", "true"])
        .expect("config set bool should succeed");

    let output = repo
        .git_ai(&["config", "quiet"])
        .expect("config get bool should succeed");
    assert_eq!(output.trim(), "true");
}

#[test]
fn test_config_set_nested_key() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "feature_flags.auth_keyring", "true"])
        .expect("config set nested key should succeed");

    let output = repo
        .git_ai(&["config", "feature_flags.auth_keyring"])
        .expect("config get nested key should succeed");
    assert_eq!(output.trim(), "true");

    let output = repo
        .git_ai(&["config", "feature_flags"])
        .expect("config get parent should succeed");
    let parsed: serde_json::Value =
        serde_json::from_str(output.trim()).expect("output should be valid JSON");
    assert_eq!(parsed["auth_keyring"], true);
}

#[test]
fn test_config_unset() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "quiet", "true"])
        .expect("config set should succeed");
    repo.git_ai(&["config", "unset", "quiet"])
        .expect("config unset should succeed");

    let result = repo.git_ai(&["config", "quiet"]);
    assert!(result.is_err(), "getting unset key should fail");
}

#[test]
fn test_config_add_to_array() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "--add", "exclude_repositories", "private/*"])
        .expect("config --add should succeed");
    repo.git_ai(&["config", "--add", "exclude_repositories", "secret/*"])
        .expect("config --add second should succeed");

    let output = repo
        .git_ai(&["config", "exclude_repositories"])
        .expect("config get array should succeed");
    let parsed: serde_json::Value =
        serde_json::from_str(output.trim()).expect("output should be valid JSON");
    let arr = parsed.as_array().expect("should be an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0], "private/*");
    assert_eq!(arr[1], "secret/*");
}

#[test]
fn test_config_set_integer() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "max_retries", "5"])
        .expect("config set integer should succeed");

    let output = repo
        .git_ai(&["config", "max_retries"])
        .expect("config get integer should succeed");
    assert_eq!(output.trim(), "5");
}

#[test]
fn test_config_api_key_masked_in_output() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["config", "set", "api_key", "sk-test-very-long-secret-key"])
        .expect("config set api_key should succeed");
    assert!(
        !output.contains("sk-test-very-long-secret-key"),
        "api_key should be masked in set output"
    );
    assert!(output.contains("sk-t"), "masked output should show prefix");
}

#[test]
fn test_config_persists_across_invocations() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "update_channel", "next"])
        .expect("config set should succeed");
    repo.git_ai(&["config", "set", "quiet", "true"])
        .expect("config set second key should succeed");

    let output = repo
        .git_ai(&["config"])
        .expect("config show all should succeed");
    let parsed: serde_json::Value =
        serde_json::from_str(output.trim()).expect("output should be valid JSON");
    assert_eq!(parsed["update_channel"], "next");
    assert_eq!(parsed["quiet"], true);
}

#[test]
fn test_config_file_location() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "quiet", "true"])
        .expect("config set should succeed");

    let config_path = repo.test_home_path().join(".git-ai").join("config.json");
    assert!(
        config_path.exists(),
        "config.json should exist in HOME/.git-ai/"
    );

    let content = fs::read_to_string(&config_path).expect("should be able to read config file");
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("config file should be valid JSON");
    assert_eq!(parsed["quiet"], true);
}

#[test]
fn test_config_help() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["config", "--help"])
        .expect("config --help should succeed");
    assert!(
        output.contains("git-ai config"),
        "help should mention git-ai config"
    );
    assert!(output.contains("set"), "help should mention set subcommand");
    assert!(
        output.contains("unset"),
        "help should mention unset subcommand"
    );
}

#[test]
fn test_config_unset_nonexistent_key_fails() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["config", "unset", "nonexistent_key"]);
    assert!(result.is_err(), "unsetting nonexistent key should fail");
}

#[test]
fn test_config_add_to_existing_non_array_fails() {
    let repo = TestRepo::new();
    repo.git_ai(&["config", "set", "quiet", "true"])
        .expect("config set should succeed");

    let result = repo.git_ai(&["config", "--add", "quiet", "false"]);
    assert!(result.is_err(), "--add on non-array key should fail");
}
