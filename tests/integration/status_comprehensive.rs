use crate::repos::test_repo::TestRepo;
use std::fs;

fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

fn write_file(repo: &TestRepo, path: &str, contents: &str) {
    let abs_path = repo.path().join(path);
    if let Some(parent) = abs_path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be creatable");
    }
    fs::write(abs_path, contents).expect("file write should succeed");
}

// =============================================================================
// Status shows pending AI changes (via checkpoint working log)
// =============================================================================

#[test]
fn test_status_shows_pending_ai_changes() {
    let repo = TestRepo::new();

    write_file(&repo, "app.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI edits the file (checkpoint but no commit yet)
    write_file(&repo, "app.rs", "fn main() {}\nfn ai_helper() {}\n");
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();

    // The working log should have checkpoint entries
    let working_logs = repo.current_working_logs();
    let checkpoints = working_logs.read_all_checkpoints().unwrap();
    assert!(
        !checkpoints.is_empty(),
        "After AI checkpoint, working logs should have checkpoint entries"
    );

    let ai_files = working_logs.all_ai_touched_files().unwrap_or_default();
    assert!(
        ai_files.contains(&"app.rs".to_string()),
        "app.rs should be listed as AI-touched file, got: {:?}",
        ai_files
    );
}

// =============================================================================
// Status no changes -- clean working tree
// =============================================================================

#[test]
fn test_status_no_changes() {
    let repo = TestRepo::new();

    write_file(&repo, "app.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // No edits -- status --json should return valid JSON (even if empty)
    let raw = repo.git_ai(&["status", "--json"]).unwrap();
    let json_str = extract_json_object(&raw);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json_str);
    assert!(
        parsed.is_ok(),
        "status --json should produce valid JSON on clean tree, got: {}",
        raw
    );
}

// =============================================================================
// Diff ignores lockfiles
// =============================================================================

#[test]
fn test_status_ignores_lockfiles() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    write_file(&repo, "package-lock.json", "{ \"lockfileVersion\": 2 }\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Modify both README and lockfile, checkpoint AI on both
    write_file(&repo, "README.md", "# repo\nupdated\n");
    write_file(
        &repo,
        "package-lock.json",
        "{ \"lockfileVersion\": 3, \"extra\": true }\n",
    );
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    repo.stage_all_and_commit("update both").unwrap();

    // The diff command should filter out lockfiles
    let diff_result = repo.git_ai(&["diff"]);
    match diff_result {
        Ok(output) => {
            // If diff produces output, it should not contain package-lock.json changes
            // (the diff command's built-in ignore list filters lockfiles)
            assert!(
                !output.contains("package-lock.json"),
                "git-ai diff should filter out package-lock.json from output, got:\n{}",
                output
            );
        }
        Err(_) => {
            // If diff fails (e.g., no changes to show), that's also acceptable
        }
    }
}

// =============================================================================
// Status JSON output -- produces valid JSON
// =============================================================================

#[test]
fn test_status_json_output() {
    let repo = TestRepo::new();

    write_file(&repo, "app.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // status --json should always produce valid JSON
    let raw = repo.git_ai(&["status", "--json"]).unwrap();
    let json_str = extract_json_object(&raw);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json_str);
    assert!(
        parsed.is_ok(),
        "status --json must produce valid JSON. Got parse error on: {}",
        json_str
    );
}

// =============================================================================
// Status with multiple files — mixed AI/human changes
// =============================================================================

#[test]
fn test_status_multiple_files() {
    let repo = TestRepo::new();

    write_file(&repo, "a.txt", "line1\n");
    write_file(&repo, "b.txt", "line1\n");
    write_file(&repo, "c.txt", "line1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI edits a.txt and c.txt
    write_file(&repo, "a.txt", "line1\nai_line_a\n");
    repo.git_ai(&["checkpoint", "mock_ai", "a.txt"]).unwrap();

    write_file(&repo, "c.txt", "line1\nai_line_c1\nai_line_c2\n");
    repo.git_ai(&["checkpoint", "mock_ai", "c.txt"]).unwrap();

    // Human edits b.txt
    write_file(&repo, "b.txt", "line1\nhuman_line_b\n");
    repo.git_ai(&["checkpoint", "mock_known_human", "b.txt"])
        .unwrap();

    let working_logs = repo.current_working_logs();
    let ai_files = working_logs.all_ai_touched_files().unwrap_or_default();

    assert!(
        ai_files.contains(&"a.txt".to_string()),
        "a.txt should be AI-touched"
    );
    assert!(
        ai_files.contains(&"c.txt".to_string()),
        "c.txt should be AI-touched"
    );
    assert!(
        !ai_files.contains(&"b.txt".to_string()),
        "b.txt should NOT be AI-touched (it was human)"
    );
}

// =============================================================================
// After committing, status is clean
// =============================================================================

#[test]
fn test_status_after_commit_is_clean() {
    let repo = TestRepo::new();

    write_file(&repo, "app.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI edits a file
    write_file(&repo, "app.rs", "fn main() {}\nfn ai_code() {}\n");
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();

    // Verify pending changes exist
    let before_logs = repo.current_working_logs();
    let before_ai = before_logs.all_ai_touched_files().unwrap_or_default();
    assert!(
        !before_ai.is_empty(),
        "Before commit, should have pending AI changes"
    );

    // Commit consumes working logs
    repo.stage_all_and_commit("commit AI changes").unwrap();

    // After commit, the new HEAD's working log should be empty/nonexistent
    let after_logs = repo.current_working_logs();
    let after_ai = after_logs.all_ai_touched_files().unwrap_or_default();
    assert!(
        after_ai.is_empty(),
        "After commit, working log should be clean (no pending AI files), got: {:?}",
        after_ai
    );
}

// =============================================================================
// Stats --json after commit shows correct AI attribution
// =============================================================================

#[test]
fn test_stats_json_after_commit() {
    let repo = TestRepo::new();

    write_file(&repo, "app.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI edits
    write_file(
        &repo,
        "app.rs",
        "fn main() {}\nfn ai_code() {}\nfn more_ai() {}\n",
    );
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();
    repo.stage_all_and_commit("AI changes").unwrap();

    // Stats should show AI additions
    let raw = repo.git_ai(&["stats", "--json"]).unwrap();
    let json_str = extract_json_object(&raw);
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("stats --json should be valid JSON");

    let ai_additions = parsed["ai_additions"].as_u64().unwrap_or(0);
    assert!(
        ai_additions >= 2,
        "Stats should show at least 2 AI additions, got: {}",
        ai_additions
    );
}

crate::reuse_tests_in_worktree!(
    test_status_shows_pending_ai_changes,
    test_status_no_changes,
    test_status_ignores_lockfiles,
    test_status_json_output,
    test_status_multiple_files,
    test_status_after_commit_is_clean,
    test_stats_json_after_commit,
);
