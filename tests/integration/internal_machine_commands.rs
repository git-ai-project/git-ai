use crate::repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;

#[test]
fn test_effective_ignore_patterns_internal_command_json() {
    let repo = TestRepo::new();

    fs::write(
        repo.path().join(".gitattributes"),
        "generated/** linguist-generated=true\n",
    )
    .expect("should write .gitattributes");
    fs::write(repo.path().join(".git-ai-ignore"), "custom/**\n")
        .expect("should write .git-ai-ignore");
    fs::write(repo.path().join("README.md"), "# repo\n").expect("should write README");
    repo.stage_all_and_commit("initial")
        .expect("initial commit");

    let request = json!({
        "user_patterns": ["user/**", "generated/**"],
        "extra_patterns": ["extra/**", "custom/**"]
    })
    .to_string();

    let output = repo
        .git_ai(&["effective-ignore-patterns", "--json", &request])
        .expect("internal command should succeed");
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON output");

    let patterns = parsed["patterns"]
        .as_array()
        .expect("patterns should be an array")
        .iter()
        .map(|v| v.as_str().expect("pattern should be a string"))
        .collect::<Vec<_>>();

    assert!(patterns.contains(&"*.lock"));
    assert!(patterns.contains(&"generated/**"));
    assert!(patterns.contains(&"custom/**"));
    assert!(patterns.contains(&"extra/**"));
    assert!(patterns.contains(&"user/**"));

    let generated_count = patterns
        .iter()
        .filter(|pattern| **pattern == "generated/**")
        .count();
    assert_eq!(generated_count, 1);
}

#[test]
fn test_blame_analysis_internal_command_json() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("analysis.txt"), "line1\nline2\nline3\n")
        .expect("should write analysis file");
    repo.stage_all_and_commit("initial")
        .expect("initial commit");

    let request = json!({
        "file_path": "analysis.txt",
        "options": {
            "line_ranges": [[2, 3]],
            "return_human_authors_as_human": true,
            "split_hunks_by_ai_author": false
        }
    })
    .to_string();

    let output = repo
        .git_ai(&["blame-analysis", "--json", &request])
        .expect("internal command should succeed");
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON output");

    let line_authors = parsed["line_authors"]
        .as_object()
        .expect("line_authors should be an object");
    assert_eq!(line_authors.len(), 2);
    assert_eq!(
        line_authors.get("2").and_then(|v| v.as_str()),
        Some("human")
    );
    assert_eq!(
        line_authors.get("3").and_then(|v| v.as_str()),
        Some("human")
    );

    assert!(
        parsed["prompt_records"]
            .as_object()
            .expect("prompt_records should be object")
            .is_empty()
    );
    assert!(
        !parsed["blame_hunks"]
            .as_array()
            .expect("blame_hunks should be array")
            .is_empty()
    );
}

#[test]
fn test_internal_machine_commands_emit_json_errors() {
    let repo = TestRepo::new();

    let err = repo
        .git_ai(&["effective-ignore-patterns"])
        .expect_err("missing --json payload should fail");

    let parsed: serde_json::Value = serde_json::from_str(err.trim()).expect("error should be JSON");
    assert!(parsed["error"].as_str().is_some());
}

#[test]
fn test_fetch_and_push_authorship_notes_internal_commands_json() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("sync.txt"), "sync authorship notes\n")
        .expect("should write sync file");
    mirror
        .stage_all_and_commit("create note source")
        .expect("commit should succeed");

    let request = json!({
        "remote_name": "origin"
    })
    .to_string();

    let fetch_before = mirror
        .git_ai(&["fetch-authorship-notes", "--json", &request])
        .expect("fetch command should succeed");
    let fetch_before_json: serde_json::Value =
        serde_json::from_str(fetch_before.trim()).expect("fetch output should be JSON");
    assert_eq!(fetch_before_json["notes_existence"], "not_found");

    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("push command should succeed");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("push output should be JSON");
    assert_eq!(push_json["ok"], true);

    let fetch_after = mirror
        .git_ai(&["fetch_authorship_notes", "--json", &request])
        .expect("fetch alias command should succeed");
    let fetch_after_json: serde_json::Value =
        serde_json::from_str(fetch_after.trim()).expect("fetch output should be JSON");
    assert_eq!(fetch_after_json["notes_existence"], "found");
}
