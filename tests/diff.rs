mod repos;
use git_ai::authorship::transcript::{AiTranscript, Message};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::{NewCommit, TestRepo};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Helper to parse diff output and extract meaningful lines
#[derive(Debug, PartialEq)]
struct DiffLine {
    prefix: String,
    content: String,
    attribution: Option<String>,
}

impl DiffLine {
    fn parse(line: &str) -> Option<Self> {
        // Skip headers and hunk markers
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("---")
            || line.starts_with("+++")
            || line.starts_with("@@")
            || line.is_empty()
        {
            return None;
        }

        let prefix = if line.starts_with('+') {
            "+"
        } else if line.starts_with('-') {
            "-"
        } else if line.starts_with(' ') {
            " "
        } else {
            return None;
        };

        // Extract content and attribution
        let rest = &line[1..];

        // Look for attribution markers at the end
        let attribution = if rest.contains("🤖") {
            // AI attribution: extract tool name after 🤖
            let parts: Vec<&str> = rest.split("🤖").collect();
            if parts.len() > 1 {
                Some(format!("ai:{}", parts[1].trim()))
            } else {
                Some("ai:unknown".to_string())
            }
        } else if rest.contains("👤") {
            // Human attribution: extract username after 👤
            let parts: Vec<&str> = rest.split("👤").collect();
            if parts.len() > 1 {
                Some(format!("human:{}", parts[1].trim()))
            } else {
                Some("human:unknown".to_string())
            }
        } else if rest.contains("[no-data]") {
            Some("no-data".to_string())
        } else {
            None
        };

        // Extract content (everything before attribution markers)
        let content = if attribution.is_some() {
            // Remove attribution from content
            rest.split("🤖")
                .next()
                .or_else(|| rest.split("👤").next())
                .or_else(|| rest.split("[no-data]").next())
                .unwrap_or(rest)
                .trim()
                .to_string()
        } else {
            rest.trim().to_string()
        };

        Some(DiffLine {
            prefix: prefix.to_string(),
            content,
            attribution,
        })
    }
}

/// Parse all meaningful diff lines from output
fn parse_diff_output(output: &str) -> Vec<DiffLine> {
    output.lines().filter_map(DiffLine::parse).collect()
}

/// Helper to assert a line has expected prefix, content, and attribution
fn assert_diff_line(
    line: &DiffLine,
    expected_prefix: &str,
    expected_content: &str,
    expected_attribution: Option<&str>,
) {
    assert_eq!(
        line.prefix, expected_prefix,
        "Line prefix mismatch: expected '{}', got '{}' for content '{}'",
        expected_prefix, line.prefix, line.content
    );

    assert!(
        line.content.contains(expected_content),
        "Line content mismatch: expected '{}' to contain '{}', full line: {:?}",
        line.content,
        expected_content,
        line
    );

    match (expected_attribution, &line.attribution) {
        (Some(expected), Some(actual)) => {
            assert!(
                actual.contains(expected),
                "Attribution mismatch: expected '{}' to contain '{}', full line: {:?}",
                actual,
                expected,
                line
            );
        }
        (Some(expected), None) => {
            panic!(
                "Expected attribution '{}' but found none for line: {:?}",
                expected, line
            );
        }
        (None, _) => {
            // Don't care about attribution
        }
    }
}

/// Assert exact sequence of diff lines with prefix, content, and attribution
fn assert_diff_lines_exact(lines: &[DiffLine], expected: &[(&str, &str, Option<&str>)]) {
    assert_eq!(
        lines.len(),
        expected.len(),
        "Line count mismatch: expected {} lines, got {}\nExpected: {:?}\nActual: {:?}",
        expected.len(),
        lines.len(),
        expected,
        lines
    );

    for (i, (line, (exp_prefix, exp_content, exp_attr))) in
        lines.iter().zip(expected.iter()).enumerate()
    {
        assert_eq!(
            &line.prefix, exp_prefix,
            "Line {} prefix mismatch: expected '{}', got '{}'\nFull line: {:?}",
            i, exp_prefix, line.prefix, line
        );

        assert!(
            line.content.contains(exp_content),
            "Line {} content mismatch: expected to contain '{}', got '{}'\nFull line: {:?}",
            i,
            exp_content,
            line.content,
            line
        );

        match (exp_attr, &line.attribution) {
            (Some(expected_attr), Some(actual_attr)) => {
                assert!(
                    actual_attr.contains(expected_attr),
                    "Line {} attribution mismatch: expected '{}', got '{}'\nFull line: {:?}",
                    i,
                    expected_attr,
                    actual_attr,
                    line
                );
            }
            (Some(expected_attr), None) => {
                panic!(
                    "Line {} expected attribution '{}' but found none\nFull line: {:?}",
                    i, expected_attr, line
                );
            }
            (None, Some(actual_attr)) => {
                // Expected no attribution but got one - this is OK for flexibility
                eprintln!(
                    "Warning: Line {} has unexpected attribution '{}', but not enforcing",
                    i, actual_attr
                );
            }
            (None, None) => {
                // Both None, OK
            }
        }
    }
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn single_prompt_id(commit: &NewCommit) -> String {
    let mut prompt_ids: Vec<String> = commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .cloned()
        .collect();
    prompt_ids.sort();
    assert_eq!(
        prompt_ids.len(),
        1,
        "expected exactly one prompt id for commit {} but got {:?}",
        commit.commit_sha,
        prompt_ids
    );
    prompt_ids[0].clone()
}

fn prompt_id_for_line_in_commit(commit: &NewCommit, file_path: &str, line: u32) -> Option<String> {
    let file_attestation = commit
        .authorship_log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == file_path)?;

    for entry in &file_attestation.entries {
        if entry.line_ranges.iter().any(|range| range.contains(line)) {
            return Some(entry.hash.clone());
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonHunk {
    commit_sha: String,
    content_hash: String,
    hunk_kind: String,
    original_commit_sha: Option<String>,
    start_line: u32,
    end_line: u32,
    file_path: String,
    prompt_id: Option<String>,
}

fn parse_json_hunks(json: &Value, file_path: &str, hunk_kind: &str) -> Vec<JsonHunk> {
    let mut hunks: Vec<JsonHunk> = json["hunks"]
        .as_array()
        .expect("hunks should be an array")
        .iter()
        .filter(|hunk| hunk["file_path"] == file_path && hunk["hunk_kind"] == hunk_kind)
        .map(|hunk| JsonHunk {
            commit_sha: hunk["commit_sha"]
                .as_str()
                .expect("commit_sha should be a string")
                .to_string(),
            content_hash: hunk["content_hash"]
                .as_str()
                .expect("content_hash should be a string")
                .to_string(),
            hunk_kind: hunk["hunk_kind"]
                .as_str()
                .expect("hunk_kind should be a string")
                .to_string(),
            original_commit_sha: hunk["original_commit_sha"]
                .as_str()
                .map(ToString::to_string),
            start_line: hunk["start_line"]
                .as_u64()
                .expect("start_line should be a number") as u32,
            end_line: hunk["end_line"]
                .as_u64()
                .expect("end_line should be a number") as u32,
            file_path: hunk["file_path"]
                .as_str()
                .expect("file_path should be a string")
                .to_string(),
            prompt_id: hunk["prompt_id"].as_str().map(ToString::to_string),
        })
        .collect();

    hunks.sort_by(|a, b| {
        (
            a.file_path.as_str(),
            a.hunk_kind.as_str(),
            a.start_line,
            a.end_line,
            a.content_hash.as_str(),
        )
            .cmp(&(
                b.file_path.as_str(),
                b.hunk_kind.as_str(),
                b.start_line,
                b.end_line,
                b.content_hash.as_str(),
            ))
    });
    hunks
}

fn commit_keys(json: &Value) -> BTreeSet<String> {
    json["commits"]
        .as_object()
        .expect("commits should be an object")
        .keys()
        .cloned()
        .collect()
}

fn write_lines(repo: &TestRepo, file_path: &str, lines: &[&str]) {
    let full_path = repo.path().join(file_path);
    let mut contents = lines.join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    fs::write(full_path, contents).expect("writing test file should succeed");
}

fn checkpoint_agent_v1(
    repo: &TestRepo,
    file_path: &str,
    tool: &str,
    model: &str,
    conversation_id: &str,
    label: &str,
) {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::user(label.to_string(), None));
    transcript.add_message(Message::assistant(
        "Applying requested changes".to_string(),
        None,
    ));

    let hook_input = serde_json::json!({
        "type": "ai_agent",
        "repo_working_dir": repo.path().to_str().unwrap(),
        "edited_filepaths": vec![file_path],
        "transcript": transcript,
        "agent_name": tool,
        "model": model,
        "conversation_id": conversation_id,
    });
    let hook_input_str = serde_json::to_string(&hook_input).expect("hook input should serialize");

    repo.git_ai(&["checkpoint", "agent-v1", "--hook-input", &hook_input_str])
        .expect("agent-v1 checkpoint should succeed");
}

fn checkpoint_human(repo: &TestRepo) {
    repo.git_ai(&["checkpoint"])
        .expect("human checkpoint should succeed");
}

fn commit_after_staging_all(repo: &TestRepo, message: &str) -> NewCommit {
    repo.git(&["add", "-A"]).expect("git add should succeed");
    repo.commit(message).expect("commit should succeed")
}

fn commit_with_git_og_as_author(
    repo: &TestRepo,
    file_path: &str,
    lines: &[&str],
    author_name: &str,
    author_email: &str,
    message: &str,
) -> String {
    write_lines(repo, file_path, lines);
    repo.git_og(&["add", file_path])
        .expect("git add via git_og should succeed");

    let author = format!("{} <{}>", author_name, author_email);
    repo.git_og_with_env(&["commit", "-m", message, "--author", &author], &[])
        .expect("git commit via git_og should succeed");

    repo.git_og(&["rev-parse", "HEAD"])
        .expect("git rev-parse should succeed")
        .trim()
        .to_string()
}

fn diff_json(repo: &TestRepo, args: &[&str]) -> Value {
    let output = repo.git_ai(args).expect("git-ai diff should succeed");
    serde_json::from_str(&output).expect("diff JSON should parse")
}

fn tool_model_stats(
    ai_lines_added: u64,
    ai_lines_generated: u64,
    ai_deletions_generated: u64,
) -> Value {
    serde_json::json!({
        "ai_lines_added": ai_lines_added,
        "ai_lines_generated": ai_lines_generated,
        "ai_deletions_generated": ai_deletions_generated
    })
}

fn assert_stats_exact(
    commit_stats: &Value,
    expected_top_level: &Value,
    expected_breakdown: &BTreeMap<String, Value>,
) {
    assert_eq!(
        commit_stats["ai_lines_added"], expected_top_level["ai_lines_added"],
        "ai_lines_added mismatch"
    );
    assert_eq!(
        commit_stats["ai_lines_generated"], expected_top_level["ai_lines_generated"],
        "ai_lines_generated mismatch"
    );
    assert_eq!(
        commit_stats["ai_deletions_generated"], expected_top_level["ai_deletions_generated"],
        "ai_deletions_generated mismatch"
    );
    assert_eq!(
        commit_stats["human_lines_added"], expected_top_level["human_lines_added"],
        "human_lines_added mismatch"
    );
    assert_eq!(
        commit_stats["unknown_lines_added"], expected_top_level["unknown_lines_added"],
        "unknown_lines_added mismatch"
    );
    assert_eq!(
        commit_stats["git_lines_added"], expected_top_level["git_lines_added"],
        "git_lines_added mismatch"
    );
    assert_eq!(
        commit_stats["git_lines_deleted"], expected_top_level["git_lines_deleted"],
        "git_lines_deleted mismatch"
    );

    let actual_breakdown = commit_stats["tool_model_breakdown"]
        .as_object()
        .expect("tool_model_breakdown should be an object");
    assert_eq!(
        actual_breakdown.len(),
        expected_breakdown.len(),
        "tool_model_breakdown entry count mismatch: actual={:?}, expected={:?}",
        actual_breakdown.keys().collect::<Vec<_>>(),
        expected_breakdown.keys().collect::<Vec<_>>()
    );

    for (key, expected_stats) in expected_breakdown {
        let actual_stats = actual_breakdown
            .get(key)
            .unwrap_or_else(|| panic!("Missing tool_model_breakdown entry for {}", key));
        assert_eq!(
            actual_stats, expected_stats,
            "tool_model_breakdown mismatch for {}",
            key
        );
    }
}

fn configure_repo_external_diff_helper(repo: &TestRepo) -> String {
    let marker = "EXTERNAL_DIFF_MARKER";
    let helper_path = repo.path().join("ext-diff-helper.sh");
    let helper_path_posix = helper_path
        .to_str()
        .expect("helper path must be valid UTF-8")
        .replace('\\', "/");

    fs::write(&helper_path, format!("#!/bin/sh\necho {marker}\nexit 0\n"))
        .expect("should write external diff helper");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&helper_path)
            .expect("helper metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&helper_path, perms).expect("helper should be executable");
    }

    repo.git_og(&["config", "diff.external", &helper_path_posix])
        .expect("configuring diff.external should succeed");

    marker.to_string()
}

fn configure_hostile_diff_settings(repo: &TestRepo) {
    let settings = [
        ("diff.noprefix", "true"),
        ("diff.mnemonicprefix", "true"),
        ("diff.srcPrefix", "SRC/"),
        ("diff.dstPrefix", "DST/"),
        ("diff.renames", "copies"),
        ("diff.relative", "true"),
        ("diff.algorithm", "histogram"),
        ("diff.indentHeuristic", "false"),
        ("diff.interHunkContext", "8"),
        ("color.diff", "always"),
        ("color.ui", "always"),
    ];
    for (key, value) in settings {
        repo.git_og(&["config", key, value])
            .unwrap_or_else(|err| panic!("setting {key}={value} should succeed: {err}"));
    }
}

fn create_external_diff_helper_script(repo: &TestRepo, marker: &str) -> std::path::PathBuf {
    let helper_path = repo.path().join(format!("ext-env-helper-{marker}.sh"));

    fs::write(&helper_path, format!("#!/bin/sh\necho {marker}\nexit 0\n"))
        .expect("should write external diff helper");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&helper_path)
            .expect("helper metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&helper_path, perms).expect("helper should be executable");
    }

    helper_path
}

#[test]
fn test_diff_single_commit() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1".human(), "Line 2".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Second commit with AI and human changes
    file.set_contents(lines![
        "Line 1".human(),
        "Line 2 modified".ai(),
        "Line 3 new".ai(),
        "Line 4 human".human()
    ]);
    let second = repo.stage_all_and_commit("Mixed changes").unwrap();

    // Run git-ai diff on the second commit
    let output = repo
        .git_ai(&["diff", &second.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse diff output
    let lines = parse_diff_output(&output);

    // Verify exact lines
    // Should have: -Line 2, +Line 2 modified, +Line 3 new, +Line 4 human
    assert!(
        lines.len() >= 4,
        "Should have at least 4 diff lines, got {}: {:?}",
        lines.len(),
        lines
    );

    // Find the deletion of Line 2
    let line2_deletion = lines
        .iter()
        .find(|l| l.prefix == "-" && l.content.contains("Line 2"));
    assert!(line2_deletion.is_some(), "Should have deletion of Line 2");

    // Find additions
    let line2_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 2 modified"));
    assert!(
        line2_addition.is_some(),
        "Should have addition of 'Line 2 modified'"
    );
    if let Some(line) = line2_addition {
        assert!(
            line.attribution
                .as_ref()
                .map(|a| a.contains("ai"))
                .unwrap_or(false),
            "Line 2 modified should have AI attribution, got: {:?}",
            line.attribution
        );
    }

    let line3_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 3 new"));
    assert!(
        line3_addition.is_some(),
        "Should have addition of 'Line 3 new'"
    );
    if let Some(line) = line3_addition {
        assert!(
            line.attribution
                .as_ref()
                .map(|a| a.contains("ai"))
                .unwrap_or(false),
            "Line 3 new should have AI attribution, got: {:?}",
            line.attribution
        );
    }

    let line4_addition = lines
        .iter()
        .find(|l| l.prefix == "+" && l.content.contains("Line 4 human"));
    assert!(
        line4_addition.is_some(),
        "Should have addition of 'Line 4 human'"
    );
}

#[test]
fn test_diff_commit_range() {
    let repo = TestRepo::new();

    // First commit
    let mut file = repo.filename("range.txt");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First commit").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second commit").unwrap();

    // Third commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    let third = repo.stage_all_and_commit("Third commit").unwrap();

    // Run git-ai diff with range
    let range = format!("{}..{}", first.commit_sha, third.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("git-ai diff range should succeed");

    // Verify output
    assert!(output.contains("diff --git"), "Should contain diff header");
    assert!(output.contains("range.txt"), "Should mention the file");
    assert!(
        output.contains("+Line 2") || output.contains("Line 2"),
        "Should show added line"
    );
    assert!(
        output.contains("+Line 3") || output.contains("Line 3"),
        "Should show added line"
    );
}

#[test]
fn test_diff_two_positional_revisions_uses_git_range_semantics() {
    let repo = TestRepo::new();

    // Ensure the "from" commit has a parent so the regression catches accidental from^..from behavior.
    repo.git(&["commit", "--allow-empty", "-m", "Empty initial"])
        .expect("empty commit should succeed");

    let mut file = repo.filename("range_positional.txt");
    file.set_contents(lines!["BASE".human()]);
    let from = repo.stage_all_and_commit("Base commit").unwrap();

    file.set_contents(lines!["BASE".human(), "AI line 1".ai(), "AI line 2".ai()]);
    let to = repo.stage_all_and_commit("Append lines").unwrap();

    let plain_git_diff = repo
        .git_og(&["--no-pager", "diff", &from.commit_sha, &to.commit_sha])
        .expect("plain git diff should succeed");
    assert!(
        plain_git_diff.contains("+AI line 1") && plain_git_diff.contains("+AI line 2"),
        "plain git diff sanity check failed:\n{}",
        plain_git_diff
    );
    assert!(
        !plain_git_diff.contains("new file mode"),
        "plain git diff should not treat this as a new file:\n{}",
        plain_git_diff
    );

    let git_ai_diff = repo
        .git_ai(&["diff", &from.commit_sha, &to.commit_sha])
        .expect("git-ai diff should support two positional revisions");

    assert!(
        git_ai_diff.contains("+AI line 1") && git_ai_diff.contains("+AI line 2"),
        "git-ai diff should include net additions between from/to commits:\n{}",
        git_ai_diff
    );
    assert!(
        !git_ai_diff.contains("new file mode") && !git_ai_diff.contains("--- /dev/null"),
        "git-ai diff should not fallback to from^..from behavior:\n{}",
        git_ai_diff
    );
}

#[test]
fn test_diff_shows_ai_attribution() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("ai_test.rs");
    file.set_contents(lines!["fn old() {}".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // AI makes changes
    file.set_contents(lines!["fn new() {}".ai(), "fn another() {}".ai()]);
    let commit = repo.stage_all_and_commit("AI changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: deletion, then two additions
    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "fn old()", None),       // Old line deleted (may have no-data or human)
            ("+", "fn new()", Some("ai")), // AI adds fn new()
            ("+", "fn another()", Some("ai")), // AI adds fn another()
        ],
    );
}

#[test]
fn test_diff_shows_human_attribution() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("human_test.rs");
    file.set_contents(lines!["fn old() {}".ai()]);
    repo.stage_all_and_commit("Initial AI").unwrap();

    // Human makes changes
    file.set_contents(lines!["fn new() {}".human(), "fn another() {}".human()]);
    let commit = repo.stage_all_and_commit("Human changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: deletion, then two additions
    assert_eq!(lines.len(), 3, "Should have exactly 3 lines");

    // First line: deletion (no attribution on deletions)
    assert_diff_line(&lines[0], "-", "fn old()", None);

    // Next two lines: additions (will have no-data or human attribution)
    assert_diff_line(&lines[1], "+", "fn new()", None);
    assert_diff_line(&lines[2], "+", "fn another()", None);

    // Verify both additions have some attribution
    assert!(
        lines[1].attribution.is_some(),
        "First addition should have attribution"
    );
    assert!(
        lines[2].attribution.is_some(),
        "Second addition should have attribution"
    );
}

#[test]
fn test_diff_multiple_files() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");
    file1.set_contents(lines!["File 1 line 1".human()]);
    file2.set_contents(lines!["File 2 line 1".human()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Modify both files
    file1.set_contents(lines!["File 1 line 1".human(), "File 1 line 2".ai()]);
    file2.set_contents(lines!["File 2 line 1".human(), "File 2 line 2".human()]);
    let commit = repo.stage_all_and_commit("Modify both files").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should show both files
    assert!(output.contains("file1.txt"), "Should mention file1");
    assert!(output.contains("file2.txt"), "Should mention file2");

    // Should have multiple diff sections
    let diff_count = output.matches("diff --git").count();
    assert_eq!(diff_count, 2, "Should have 2 diff sections");
}

#[test]
fn test_diff_initial_commit() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("initial.txt");
    file.set_contents(lines!["Initial line".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Run diff on initial commit (should compare to empty tree)
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff on initial commit should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Should have exactly 1 addition, no deletions
    assert_diff_lines_exact(
        &lines,
        &[
            ("+", "Initial line", Some("ai")), // Only addition with AI attribution
        ],
    );
}

#[test]
fn test_diff_pure_additions() {
    let repo = TestRepo::new();

    // Initial commit with one line
    let mut file = repo.filename("additions.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Add more lines at the end (pure additions)
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".ai()]);
    let commit = repo.stage_all_and_commit("Add lines").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should have additions
    assert!(
        output.contains("+Line 2") || output.contains("Line 2"),
        "Should show Line 2 addition"
    );
    assert!(
        output.contains("+Line 3") || output.contains("Line 3"),
        "Should show Line 3 addition"
    );

    // Should show AI attribution on added lines
    assert!(
        output.contains("🤖") || output.contains("mock_ai"),
        "Should show AI attribution on additions"
    );
}

#[test]
fn test_diff_pure_deletions() {
    let repo = TestRepo::new();

    // Initial commit with multiple lines
    let mut file = repo.filename("deletions.txt");
    file.set_contents(lines![
        "Line 1".ai(),
        "Line 2".ai(),
        "Line 3".human(),
        "Line 4".ai()
    ]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Delete all lines
    file.set_contents(lines![]);
    let commit = repo.stage_all_and_commit("Delete all").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify exact sequence
    let lines = parse_diff_output(&output);

    // Verify exact order: 4 deletions in sequence, no additions
    assert_eq!(
        lines.len(),
        4,
        "Should have exactly 4 lines (all deletions)"
    );

    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "Line 1", None), // No attribution on deletions
            ("-", "Line 2", None), // No attribution on deletions
            ("-", "Line 3", None), // No attribution on deletions
            ("-", "Line 4", None), // No attribution on deletions
        ],
    );
}

#[test]
fn test_diff_mixed_ai_and_human() {
    let repo = TestRepo::new();

    // Initial commit with AI content
    let mut file = repo.filename("mixed.txt");
    file.set_contents(lines!["Line 1".ai(), "Line 2".ai()]);
    repo.stage_all_and_commit("Initial AI").unwrap();

    // Modify with AI changes
    file.set_contents(lines![
        "Line 1".ai(),
        "Line 2 modified".ai(),
        "Line 3 new".ai()
    ]);
    let commit = repo.stage_all_and_commit("AI modifies").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should have both additions and deletions
    assert!(output.contains("-"), "Should have deletion lines");
    assert!(output.contains("+"), "Should have addition lines");

    // Should show AI attribution
    let has_ai = output.contains("🤖") || output.contains("mock_ai");
    assert!(has_ai, "Should show AI attribution, output:\n{}", output);
}

#[test]
fn test_diff_with_head_ref() {
    let repo = TestRepo::new();

    // Initial commit
    let mut file = repo.filename("head_test.txt");
    file.set_contents(lines!["Line 1".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Add line").unwrap();

    // Run diff using HEAD
    let output = repo
        .git_ai(&["diff", "HEAD"])
        .expect("git-ai diff HEAD should succeed");

    // Should work with HEAD reference
    assert!(output.contains("diff --git"), "Should contain diff header");
    assert!(output.contains("head_test.txt"), "Should mention the file");
}

#[test]
fn test_diff_output_format() {
    let repo = TestRepo::new();

    // Create a simple diff
    let mut file = repo.filename("format.txt");
    file.set_contents(lines!["old".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["new".ai()]);
    let commit = repo.stage_all_and_commit("Change").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Verify standard git diff format elements
    assert!(output.contains("diff --git"), "Should have diff header");
    assert!(output.contains("---"), "Should have old file marker");
    assert!(output.contains("+++"), "Should have new file marker");
    assert!(output.contains("@@"), "Should have hunk header");

    // Parse and verify exact sequence of diff lines
    let lines = parse_diff_output(&output);

    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "old", None),       // Deletion (may have no-data or human)
            ("+", "new", Some("ai")), // Addition with AI attribution
        ],
    );
}

#[test]
fn test_diff_error_on_no_args() {
    let repo = TestRepo::new();

    // Try to run diff without arguments
    let result = repo.git_ai(&["diff"]);

    // Should fail with error
    assert!(result.is_err(), "git-ai diff without arguments should fail");
}

#[test]
fn test_diff_json_output_with_escaped_newlines() {
    let repo = TestRepo::new();

    // Initial commit with text.split("\n")
    let mut file = repo.filename("utils.ts");
    file.set_contents(lines![r#"const lines = text.split("\n")"#.human()]);
    repo.stage_all_and_commit("Initial split implementation")
        .unwrap();

    // Modify to other_text.split("\n\n")
    file.set_contents(lines![r#"const lines = other_text.split("\n\n")"#.ai()]);
    let commit = repo
        .stage_all_and_commit("Update split to use double newline")
        .unwrap();

    // Run git-ai diff with --json flag
    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("git-ai diff --json should succeed");

    // Parse JSON to verify it's valid
    let json: serde_json::Value =
        serde_json::from_str(&output).expect("Output should be valid JSON");

    // Verify newlines are properly escaped in the base_content
    let files = json.get("files").unwrap().as_object().unwrap();
    let utils_file = files.get("utils.ts").unwrap();
    let base_content = utils_file.get("base_content").unwrap().as_str().unwrap();
    assert!(
        base_content.contains(r#"text.split("\n")"#),
        "Base content should contain properly escaped newlines: text.split(\"\\n\"), got: {}",
        base_content
    );

    // Verify newlines are properly escaped in the diff content
    let diff = utils_file.get("diff").unwrap().as_str().unwrap();
    assert!(
        diff.contains(r#"text.split("\n")"#),
        "Diff should contain properly escaped newlines in old line: text.split(\"\\n\")"
    );
    assert!(
        diff.contains(r#"other_text.split("\n\n")"#),
        "Diff should contain properly escaped newlines in new line: other_text.split(\"\\n\\n\")"
    );

    // Print the JSON output for inspection
    println!("JSON output:\n{}", serde_json::to_string(&json).unwrap());
}

#[test]
fn test_diff_json_omits_commit_stats_without_include_stats_flag() {
    let repo = TestRepo::new();

    let mut file = repo.filename("stats_omitted.txt");
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["base".human(), "ai line".ai()]);
    let commit = repo.stage_all_and_commit("Add AI line").unwrap();

    let output = repo
        .git_ai(&["diff", &commit.commit_sha, "--json"])
        .expect("git-ai diff --json should succeed");
    let json: Value = serde_json::from_str(&output).expect("diff JSON should parse");

    assert!(
        json.get("commit_stats").is_none(),
        "commit_stats should be omitted unless --include-stats is provided"
    );
}

#[test]
fn test_diff_json_all_prompts_includes_non_landing_prompts() {
    let repo = TestRepo::new();

    write_lines(&repo, "all_prompts.txt", &["base"]);
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    // Landed AI prompt: cursor::gpt-4o
    write_lines(&repo, "all_prompts.txt", &["base", "cursor landed"]);
    checkpoint_agent_v1(
        &repo,
        "all_prompts.txt",
        "cursor",
        "gpt-4o",
        "cursor-conv",
        "cursor landed edit",
    );

    // Landed AI prompt: codex::o3
    write_lines(
        &repo,
        "all_prompts.txt",
        &["base", "cursor landed", "codex landed"],
    );
    checkpoint_agent_v1(
        &repo,
        "all_prompts.txt",
        "codex",
        "o3",
        "codex-conv",
        "codex landed edit",
    );

    // Non-landing AI prompt: claude::sonnet (added then removed before commit)
    write_lines(
        &repo,
        "all_prompts.txt",
        &["base", "cursor landed", "codex landed", "claude temp"],
    );
    checkpoint_agent_v1(
        &repo,
        "all_prompts.txt",
        "claude",
        "sonnet",
        "claude-conv",
        "temporary claude edit",
    );
    write_lines(
        &repo,
        "all_prompts.txt",
        &["base", "cursor landed", "codex landed"],
    );
    checkpoint_human(&repo);

    let commit = commit_after_staging_all(&repo, "all-prompts target");

    let all_prompt_ids: BTreeSet<String> = commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        all_prompt_ids.len(),
        3,
        "expected three prompts in authorship note"
    );

    let claude_prompt_id = commit
        .authorship_log
        .metadata
        .prompts
        .iter()
        .find_map(|(prompt_id, prompt)| {
            (prompt.agent_id.tool == "claude" && prompt.agent_id.model == "sonnet")
                .then_some(prompt_id.clone())
        })
        .expect("expected non-landing claude prompt in authorship note");

    let without_all_prompts = diff_json(&repo, &["diff", &commit.commit_sha, "--json"]);
    let without_ids: BTreeSet<String> = without_all_prompts["prompts"]
        .as_object()
        .expect("prompts should be an object")
        .keys()
        .cloned()
        .collect();
    assert!(
        !without_ids.contains(&claude_prompt_id),
        "non-landing prompt should be omitted without --all-prompts"
    );

    let with_all_prompts = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--all-prompts"],
    );
    let with_ids: BTreeSet<String> = with_all_prompts["prompts"]
        .as_object()
        .expect("prompts should be an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        with_ids, all_prompt_ids,
        "--all-prompts should return exactly all prompts from authorship note"
    );
    assert!(
        with_ids.contains(&claude_prompt_id),
        "--all-prompts should include non-landing prompt"
    );
}

#[test]
fn test_diff_json_include_stats_exact_single_model_counts() {
    let repo = TestRepo::new();

    write_lines(
        &repo,
        "single_model_stats.txt",
        &["base-1", "base-2", "base-3", "base-4"],
    );
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    // Math:
    // - Landed diff: +2 AI lines, -2 lines
    // - Prompt generated totals: +2, -2
    write_lines(
        &repo,
        "single_model_stats.txt",
        &["base-1", "cursor-ai-1", "cursor-ai-2", "base-4"],
    );
    checkpoint_agent_v1(
        &repo,
        "single_model_stats.txt",
        "cursor",
        "gpt-4o",
        "single-model-conv",
        "replace two lines",
    );

    let commit = commit_after_staging_all(&repo, "single model stats target");
    let diff = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--include-stats"],
    );
    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present with --include-stats");

    let expected_top_level = serde_json::json!({
        "ai_lines_added": 2,
        "ai_lines_generated": 2,
        "ai_deletions_generated": 2,
        "human_lines_added": 0,
        "unknown_lines_added": 0,
        "git_lines_added": 2,
        "git_lines_deleted": 2
    });
    let expected_breakdown =
        BTreeMap::from([("cursor::gpt-4o".to_string(), tool_model_stats(2, 2, 2))]);
    assert_stats_exact(commit_stats, &expected_top_level, &expected_breakdown);
}

#[test]
fn test_diff_json_include_stats_exact_multi_model_with_non_landing_prompt() {
    let repo = TestRepo::new();

    write_lines(&repo, "multi_model_stats.txt", &["base"]);
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    // cursor::gpt-4o adds 3 lines
    write_lines(
        &repo,
        "multi_model_stats.txt",
        &["base", "cursor-1", "cursor-2", "cursor-3"],
    );
    checkpoint_agent_v1(
        &repo,
        "multi_model_stats.txt",
        "cursor",
        "gpt-4o",
        "cursor-main-conv",
        "cursor adds three lines",
    );

    // codex::o3 adds 2 lines
    write_lines(
        &repo,
        "multi_model_stats.txt",
        &[
            "base", "cursor-1", "cursor-2", "cursor-3", "codex-a", "codex-b",
        ],
    );
    checkpoint_agent_v1(
        &repo,
        "multi_model_stats.txt",
        "codex",
        "o3",
        "codex-main-conv",
        "codex adds two lines",
    );

    // Same codex prompt does delete 1 + add 1 (net 0)
    write_lines(
        &repo,
        "multi_model_stats.txt",
        &[
            "base", "cursor-1", "cursor-2", "cursor-3", "codex-a", "codex-b2",
        ],
    );
    checkpoint_agent_v1(
        &repo,
        "multi_model_stats.txt",
        "codex",
        "o3",
        "codex-main-conv",
        "codex replaces one line",
    );

    // Non-landing claude::sonnet prompt (+1 generated, 0 landed)
    write_lines(
        &repo,
        "multi_model_stats.txt",
        &[
            "base",
            "cursor-1",
            "cursor-2",
            "cursor-3",
            "codex-a",
            "codex-b2",
            "claude-temp",
        ],
    );
    checkpoint_agent_v1(
        &repo,
        "multi_model_stats.txt",
        "claude",
        "sonnet",
        "claude-temp-conv",
        "temporary claude line",
    );

    // Human override of one cursor line and remove claude temp line
    write_lines(
        &repo,
        "multi_model_stats.txt",
        &[
            "base",
            "cursor-1",
            "human-override",
            "cursor-3",
            "codex-a",
            "codex-b2",
        ],
    );
    checkpoint_human(&repo);

    let commit = commit_after_staging_all(&repo, "multi model stats target");
    let diff = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--include-stats"],
    );
    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present with --include-stats");

    // Math ledger:
    // - Landed additions in final diff: 5 total
    //   - AI landed: 4 (cursor-1, cursor-3, codex-a, codex-b2)
    //   - Human landed: 1 (human-override)
    // - Landed deletions in final diff: 0
    // - Generated by prompts:
    //   - cursor::gpt-4o => +3, -0
    //   - codex::o3 => +3, -1
    //   - claude::sonnet => +1, -0 (non-landing)
    // => totals: ai_lines_generated=7, ai_deletions_generated=1
    let expected_top_level = serde_json::json!({
        "ai_lines_added": 4,
        "ai_lines_generated": 7,
        "ai_deletions_generated": 1,
        "human_lines_added": 1,
        "unknown_lines_added": 0,
        "git_lines_added": 5,
        "git_lines_deleted": 0
    });
    let expected_breakdown = BTreeMap::from([
        ("claude::sonnet".to_string(), tool_model_stats(0, 1, 0)),
        ("codex::o3".to_string(), tool_model_stats(2, 3, 1)),
        ("cursor::gpt-4o".to_string(), tool_model_stats(2, 3, 0)),
    ]);
    assert_stats_exact(commit_stats, &expected_top_level, &expected_breakdown);

    let prompts_without_all = diff["prompts"]
        .as_object()
        .expect("prompts should be object");
    assert_eq!(
        prompts_without_all.len(),
        2,
        "without --all-prompts, only landed prompt records should be returned"
    );
    assert!(
        !prompts_without_all.values().any(|prompt| {
            prompt["agent_id"]["tool"] == "claude" && prompt["agent_id"]["model"] == "sonnet"
        }),
        "non-landing prompt should not be present in prompts map without --all-prompts"
    );
}

#[test]
fn test_diff_json_include_stats_exact_human_landed_with_ai_generated() {
    let repo = TestRepo::new();

    write_lines(&repo, "human_landed_stats.txt", &["base"]);
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    // AI generates two lines, human rewrites both before commit.
    write_lines(
        &repo,
        "human_landed_stats.txt",
        &["base", "ai-temp-1", "ai-temp-2"],
    );
    checkpoint_agent_v1(
        &repo,
        "human_landed_stats.txt",
        "cursor",
        "gpt-4o",
        "human-landed-conv",
        "temporary ai lines",
    );

    write_lines(
        &repo,
        "human_landed_stats.txt",
        &["base", "human-final-1", "human-final-2"],
    );
    checkpoint_human(&repo);

    let commit = commit_after_staging_all(&repo, "human landed target");
    let diff = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--include-stats"],
    );
    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present with --include-stats");

    let expected_top_level = serde_json::json!({
        "ai_lines_added": 0,
        "ai_lines_generated": 2,
        "ai_deletions_generated": 0,
        "human_lines_added": 2,
        "unknown_lines_added": 0,
        "git_lines_added": 2,
        "git_lines_deleted": 0
    });
    let expected_breakdown =
        BTreeMap::from([("cursor::gpt-4o".to_string(), tool_model_stats(0, 2, 0))]);
    assert_stats_exact(commit_stats, &expected_top_level, &expected_breakdown);
}

#[test]
fn test_diff_json_include_stats_blame_deletions_devin_added_prompts_only() {
    let repo = TestRepo::new();

    write_lines(
        &repo,
        "blame_deletion_stats.txt",
        &["base-1", "base-2", "base-3"],
    );
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    // Commit A: codex prompt that will appear only in deleted hunks of commit B.
    write_lines(
        &repo,
        "blame_deletion_stats.txt",
        &["base-1", "ai-temp-2", "ai-temp-3"],
    );
    checkpoint_agent_v1(
        &repo,
        "blame_deletion_stats.txt",
        "codex",
        "o3",
        "blame-deletion-source",
        "ai replacement",
    );
    let _source_commit = commit_after_staging_all(&repo, "codex source");

    // Commit B: background/synthetic agent commit authored as Devin bot (no authorship note).
    // It deletes codex lines and adds new Devin lines.
    let devin_commit_sha = commit_with_git_og_as_author(
        &repo,
        "blame_deletion_stats.txt",
        &["base-1", "devin-final-4", "devin-final-5"],
        "devin-ai-integration[bot]",
        "158243242+devin-ai-integration[bot]@users.noreply.github.com",
        "devin cleanup",
    );

    let diff = diff_json(
        &repo,
        &[
            "diff",
            &devin_commit_sha,
            "--json",
            "--include-stats",
            "--blame-deletions",
        ],
    );

    let prompts = diff["prompts"]
        .as_object()
        .expect("prompts should be object");
    let prompt_tools: BTreeSet<String> = prompts
        .values()
        .map(|prompt| {
            prompt["agent_id"]["tool"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(
        prompt_tools,
        BTreeSet::from(["codex".to_string(), "devin".to_string()]),
        "prompt payload should include deleted-origin codex plus added-hunk Devin prompt records"
    );

    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present with --include-stats");
    let breakdown = commit_stats["tool_model_breakdown"]
        .as_object()
        .expect("tool_model_breakdown should be an object");
    assert!(
        breakdown.keys().any(|key| key.starts_with("devin::")),
        "expected Devin in tool_model_breakdown, got: {:?}",
        breakdown.keys().collect::<Vec<_>>()
    );
    assert!(
        !breakdown.keys().any(|key| key.starts_with("codex::")),
        "deleted-origin codex prompt should not contribute to commit_stats breakdown"
    );

    assert_eq!(commit_stats["ai_lines_added"], serde_json::json!(2));
    assert_eq!(commit_stats["git_lines_added"], serde_json::json!(2));
    assert_eq!(commit_stats["git_lines_deleted"], serde_json::json!(2));
    assert_eq!(commit_stats["human_lines_added"], serde_json::json!(0));
    assert_eq!(commit_stats["unknown_lines_added"], serde_json::json!(0));
    assert!(
        commit_stats["ai_lines_generated"].as_u64().unwrap_or(0) > 0,
        "Devin synthetic prompt should still contribute generated stats"
    );
}

#[test]
fn test_diff_json_rename_only_has_no_hunks_and_zero_stats() {
    let repo = TestRepo::new();

    write_lines(&repo, "rename_old.txt", &["line-1", "line-2"]);
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    repo.git(&["mv", "rename_old.txt", "rename_new.txt"])
        .expect("git mv should succeed");
    checkpoint_human(&repo);
    let commit = commit_after_staging_all(&repo, "rename only");

    let diff = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--include-stats"],
    );

    let hunks = diff["hunks"].as_array().expect("hunks should be an array");
    assert!(
        hunks.is_empty(),
        "rename-only commit should not produce add/delete hunks"
    );

    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present");
    let expected_top_level = serde_json::json!({
        "ai_lines_added": 0,
        "ai_lines_generated": 0,
        "ai_deletions_generated": 0,
        "human_lines_added": 0,
        "unknown_lines_added": 0,
        "git_lines_added": 0,
        "git_lines_deleted": 0
    });
    let expected_breakdown: BTreeMap<String, Value> = BTreeMap::new();
    assert_stats_exact(commit_stats, &expected_top_level, &expected_breakdown);

    let files = diff["files"].as_object().expect("files should be object");
    assert!(
        !files.is_empty(),
        "rename-only commit should still include a file diff section"
    );
    assert!(
        files.values().any(|file| {
            let text = file["diff"].as_str().unwrap_or("");
            text.contains("rename from rename_old.txt") && text.contains("rename to rename_new.txt")
        }),
        "rename-only diff should include rename metadata"
    );
}

#[test]
fn test_diff_json_rename_with_ai_edit_exact_stats() {
    let repo = TestRepo::new();

    write_lines(&repo, "rename_edit_old.txt", &["base-1", "base-2"]);
    checkpoint_human(&repo);
    let _base = commit_after_staging_all(&repo, "base");

    repo.git(&["mv", "rename_edit_old.txt", "rename_edit_new.txt"])
        .expect("git mv should succeed");
    write_lines(
        &repo,
        "rename_edit_new.txt",
        &["base-1", "ai-line-2", "ai-line-3"],
    );
    checkpoint_agent_v1(
        &repo,
        "rename_edit_new.txt",
        "cursor",
        "gpt-4o",
        "rename-edit-conv",
        "rename and edit",
    );
    let commit = commit_after_staging_all(&repo, "rename with ai edit");

    let diff = diff_json(
        &repo,
        &["diff", &commit.commit_sha, "--json", "--include-stats"],
    );
    let commit_stats = diff
        .get("commit_stats")
        .expect("commit_stats should be present");

    // Math ledger:
    // old: [base-1, base-2]
    // new: [base-1, ai-line-2, ai-line-3]
    // => landed +2, -1 (all AI-attributed additions)
    // => prompt totals +3, -0 (checkpoint rewrites full new file)
    let expected_top_level = serde_json::json!({
        "ai_lines_added": 2,
        "ai_lines_generated": 3,
        "ai_deletions_generated": 0,
        "human_lines_added": 0,
        "unknown_lines_added": 0,
        "git_lines_added": 2,
        "git_lines_deleted": 1
    });
    let expected_breakdown =
        BTreeMap::from([("cursor::gpt-4o".to_string(), tool_model_stats(2, 3, 0))]);
    assert_stats_exact(commit_stats, &expected_top_level, &expected_breakdown);

    let files = diff["files"].as_object().expect("files should be object");
    assert!(
        files.values().any(|file| {
            let text = file["diff"].as_str().unwrap_or("");
            text.contains("rename from rename_edit_old.txt")
                && text.contains("rename to rename_edit_new.txt")
        }),
        "rename+edit diff should include rename metadata"
    );
}

#[test]
fn test_diff_json_blame_deletions_rename_with_edit_uses_old_path() {
    let repo = TestRepo::new();

    let mut old_file = repo.filename("rename_blame_old.txt");
    old_file.set_contents(lines!["keep".human(), "drop-ai".ai(), "tail".human()]);
    let base_commit = repo.stage_all_and_commit("base with ai line").unwrap();
    let old_line_prompt = prompt_id_for_line_in_commit(&base_commit, "rename_blame_old.txt", 2)
        .expect("line 2 in base commit should be AI-attributed");

    repo.git(&["mv", "rename_blame_old.txt", "rename_blame_new.txt"])
        .expect("git mv should succeed");
    let mut new_file = repo.filename("rename_blame_new.txt");
    new_file.set_contents(lines!["keep".human(), "tail".human()]);
    let rename_commit = repo
        .stage_all_and_commit("rename and edit removing ai line")
        .unwrap();

    let output = repo
        .git_ai(&[
            "diff",
            &rename_commit.commit_sha,
            "--json",
            "--blame-deletions",
        ])
        .expect("diff --json --blame-deletions should succeed");
    let json: Value = serde_json::from_str(&output).expect("diff JSON should parse");

    let deletion_hunks = parse_json_hunks(&json, "rename_blame_new.txt", "deletion");
    assert_eq!(
        deletion_hunks,
        vec![JsonHunk {
            commit_sha: rename_commit.commit_sha.clone(),
            content_hash: sha256_hex("drop-ai"),
            hunk_kind: "deletion".to_string(),
            original_commit_sha: Some(base_commit.commit_sha.clone()),
            start_line: 2,
            end_line: 2,
            file_path: "rename_blame_new.txt".to_string(),
            prompt_id: Some(old_line_prompt),
        }],
        "deletion blame should resolve against the old path after rename+edit"
    );

    let expected_commit_keys = BTreeSet::from([
        base_commit.commit_sha.clone(),
        rename_commit.commit_sha.clone(),
    ]);
    assert_eq!(commit_keys(&json), expected_commit_keys);
}

#[test]
fn test_diff_json_include_stats_rejects_commit_ranges() {
    let repo = TestRepo::new();

    let mut file = repo.filename("range_stats.txt");
    file.set_contents(lines!["line 1".human()]);
    let first = repo.stage_all_and_commit("Commit 1").unwrap();

    file.set_contents(lines!["line 1".human(), "line 2".ai()]);
    let second = repo.stage_all_and_commit("Commit 2").unwrap();

    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let result = repo.git_ai(&["diff", &range, "--json", "--include-stats"]);
    assert!(
        result.is_err(),
        "--include-stats should be rejected for commit ranges"
    );
}

#[test]
fn test_diff_preserves_context_lines() {
    let repo = TestRepo::new();

    // Create file with multiple lines
    let mut file = repo.filename("context.txt");
    file.set_contents(lines![
        "Context 1".human(),
        "Context 2".human(),
        "Context 3".human(),
        "Old line".human(),
        "Context 4".human(),
        "Context 5".human(),
        "Context 6".human()
    ]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Change one line in the middle
    file.set_contents(lines![
        "Context 1".human(),
        "Context 2".human(),
        "Context 3".human(),
        "New line".ai(),
        "Context 4".human(),
        "Context 5".human(),
        "Context 6".human()
    ]);
    let commit = repo.stage_all_and_commit("Change middle").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Should show context lines (lines starting with space)
    let context_count = output
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert!(
        context_count >= 3,
        "Should show at least 3 context lines (default -U3)"
    );
}

#[test]
fn test_diff_exact_sequence_verification() {
    let repo = TestRepo::new();

    // Initial commit with 2 lines
    let mut file = repo.filename("sequence.rs");
    file.set_contents(lines!["fn first() {}".human(), "fn second() {}".ai()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Modify: delete first, modify second, add third
    file.set_contents(lines!["fn second_modified() {}".ai(), "fn third() {}".ai()]);
    let commit = repo.stage_all_and_commit("Complex changes").unwrap();

    // Run diff
    let output = repo
        .git_ai(&["diff", &commit.commit_sha])
        .expect("git-ai diff should succeed");

    // Parse and verify EXACT order of every line
    let lines = parse_diff_output(&output);

    // Verify exact sequence with specific order and attribution
    // Git will show: delete both old lines, add both new lines
    assert_diff_lines_exact(
        &lines,
        &[
            ("-", "fn first()", None),                 // Delete human line
            ("-", "fn second()", None), // Delete AI line (no attribution on deletions)
            ("+", "fn second_modified()", Some("ai")), // Add AI line
            ("+", "fn third()", Some("ai")), // Add AI line
        ],
    );
}

#[test]
fn test_diff_range_multiple_commits() {
    let repo = TestRepo::new();

    // First commit
    let mut file = repo.filename("multi.txt");
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("First").unwrap();

    // Second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
    repo.stage_all_and_commit("Second").unwrap();

    // Third commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai(), "Line 3".human()]);
    repo.stage_all_and_commit("Third").unwrap();

    // Fourth commit
    file.set_contents(lines![
        "Line 1".human(),
        "Line 2".ai(),
        "Line 3".human(),
        "Line 4".ai()
    ]);
    let fourth = repo.stage_all_and_commit("Fourth").unwrap();

    // Run diff across multiple commits
    let range = format!("{}..{}", first.commit_sha, fourth.commit_sha);
    let output = repo
        .git_ai(&["diff", &range])
        .expect("git-ai diff multi-commit range should succeed");

    // Should show cumulative changes
    assert!(output.contains("+Line 2"), "Should show Line 2 addition");
    assert!(output.contains("+Line 3"), "Should show Line 3 addition");
    assert!(output.contains("+Line 4"), "Should show Line 4 addition");

    // Should have attribution markers
    assert!(
        output.contains("🤖") || output.contains("👤"),
        "Should have attribution markers"
    );
}

#[test]
fn test_diff_ignores_repo_external_diff_helper_but_proxy_uses_it() {
    let repo = TestRepo::new();

    let mut file = repo.filename("README.md");
    file.set_contents(lines!["line one".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["line one".human(), "line two".ai()]);
    repo.stage_all_and_commit("second").unwrap();

    let marker = configure_repo_external_diff_helper(&repo);

    let proxied_diff = repo
        .git(&["diff", "HEAD^", "HEAD"])
        .expect("proxied git diff should succeed");
    assert!(
        proxied_diff.contains(&marker),
        "proxied git diff should honor diff.external helper output, got:\n{}",
        proxied_diff
    );

    let git_ai_diff = repo
        .git_ai(&["diff", "HEAD"])
        .expect("git-ai diff should succeed");
    assert!(
        !git_ai_diff.contains(&marker),
        "git-ai diff should not use external diff helper output, got:\n{}",
        git_ai_diff
    );
    assert!(
        git_ai_diff.contains("diff --git"),
        "git-ai diff should emit standard unified diff output, got:\n{}",
        git_ai_diff
    );
    assert!(
        git_ai_diff.contains("@@"),
        "git-ai diff should include hunk headers, got:\n{}",
        git_ai_diff
    );
}

#[test]
fn test_diff_parsing_is_stable_under_hostile_diff_config() {
    let repo = TestRepo::new();

    let mut file = repo.filename("README.md");
    file.set_contents(lines!["line one".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines![
        "line one".human(),
        "line two".ai(),
        "line three".ai()
    ]);
    repo.stage_all_and_commit("second").unwrap();

    configure_hostile_diff_settings(&repo);

    let git_ai_diff = repo
        .git_ai(&["diff", "HEAD"])
        .expect("git-ai diff should succeed");
    assert!(git_ai_diff.contains("diff --git"));
    assert!(git_ai_diff.contains("@@"));
    assert!(git_ai_diff.contains("+line two"));
    assert!(git_ai_diff.contains("+line three"));
}

#[test]
fn test_checkpoint_and_commit_ignore_repo_external_diff_helper() {
    let repo = TestRepo::new();

    let mut file = repo.filename("tracked.txt");
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["base".human(), "added by ai".ai()]);
    let marker = configure_repo_external_diff_helper(&repo);
    let proxied_diff = repo
        .git(&["diff", "HEAD"])
        .expect("proxied git diff should succeed");
    assert!(
        proxied_diff.contains(&marker),
        "sanity check: external diff helper should be active for proxied git diff"
    );

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed with external diff configured");
    repo.stage_all_and_commit("ai commit").unwrap();

    file.assert_lines_and_blame(lines!["base".human(), "added by ai".ai()]);
}

#[test]
fn test_diff_ignores_git_external_diff_env_but_proxy_uses_it() {
    let repo = TestRepo::new();

    let mut file = repo.filename("env-diff.txt");
    file.set_contents(lines!["before".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines!["before".human(), "after".ai()]);
    repo.stage_all_and_commit("second").unwrap();

    let marker = "ENV_EXTERNAL_DIFF_MARKER";
    let helper_path = create_external_diff_helper_script(&repo, marker);
    let helper_path_str = helper_path
        .to_str()
        .expect("helper path must be valid UTF-8")
        .replace('\\', "/")
        .to_string();

    let proxied = repo
        .git_with_env(
            &["diff", "HEAD^", "HEAD"],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
            None,
        )
        .expect("proxied git diff should succeed");
    assert!(
        proxied.contains(marker),
        "proxied git diff should honor GIT_EXTERNAL_DIFF, got:\n{}",
        proxied
    );

    let ai_diff = repo
        .git_ai_with_env(
            &["diff", "HEAD"],
            &[("GIT_EXTERNAL_DIFF", helper_path_str.as_str())],
        )
        .expect("git-ai diff should succeed with GIT_EXTERNAL_DIFF set");
    assert!(
        !ai_diff.contains(marker),
        "git-ai diff should ignore GIT_EXTERNAL_DIFF for internal diff calls, got:\n{}",
        ai_diff
    );
    assert!(
        ai_diff.contains("diff --git"),
        "git-ai diff should still emit normal unified diff output, got:\n{}",
        ai_diff
    );
}

#[test]
fn test_diff_ignores_git_diff_opts_env_for_internal_diff() {
    let repo = TestRepo::new();

    let mut file = repo.filename("env-diff-opts.txt");
    file.set_contents(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "line 4".human(),
        "line 5".human()
    ]);
    repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(lines![
        "line 1".human(),
        "line 2".human(),
        "line 3 changed".ai(),
        "line 4".human(),
        "line 5".human()
    ]);
    let commit = repo.stage_all_and_commit("change middle").unwrap();

    // Proxied git should honor this env var and output 0 context lines.
    let proxied = repo
        .git_with_env(
            &[
                "diff",
                &format!("{}^", commit.commit_sha),
                &commit.commit_sha,
            ],
            &[("GIT_DIFF_OPTS", "--unified=0")],
            None,
        )
        .expect("proxied git diff should succeed");
    let proxied_context_count = proxied
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert_eq!(
        proxied_context_count, 0,
        "proxied git diff should honor GIT_DIFF_OPTS=--unified=0, got:\n{}",
        proxied
    );

    // git-ai diff should ignore GIT_DIFF_OPTS and keep normal context behavior.
    let ai_diff = repo
        .git_ai_with_env(
            &["diff", &commit.commit_sha],
            &[("GIT_DIFF_OPTS", "--unified=0")],
        )
        .expect("git-ai diff should succeed with GIT_DIFF_OPTS set");
    let ai_context_count = ai_diff
        .lines()
        .filter(|l| l.starts_with(' ') && !l.starts_with("  "))
        .count();
    assert!(
        ai_context_count >= 2,
        "git-ai diff should ignore GIT_DIFF_OPTS and preserve context lines, got:\n{}",
        ai_diff
    );
}

#[test]
fn test_diff_respects_effective_ignore_patterns() {
    let repo = TestRepo::new();
    let ignore_file_path = repo.path().join(".git-ai-ignore");
    fs::write(&ignore_file_path, "ignored/**\n").expect("should write .git-ai-ignore");

    let mut visible = repo.filename("src/visible.txt");
    let mut ignored = repo.filename("ignored/secret.txt");
    visible.set_contents(lines!["base visible".human()]);
    ignored.set_contents(lines!["base secret".ai()]);
    repo.stage_all_and_commit("Initial with ignored file")
        .unwrap();

    visible.set_contents(lines!["base visible".human(), "new visible".ai()]);
    ignored.set_contents(lines!["base secret".ai(), "new secret".ai()]);
    let change_commit = repo
        .stage_all_and_commit("Change visible and ignored")
        .unwrap();

    let terminal_output = repo
        .git_ai(&["diff", &change_commit.commit_sha])
        .expect("git-ai diff should succeed");
    assert!(
        terminal_output.contains("src/visible.txt"),
        "visible file should be present in diff output"
    );
    assert!(
        !terminal_output.contains("ignored/secret.txt"),
        "ignored file should be filtered from diff output"
    );

    let json_output = repo
        .git_ai(&["diff", &change_commit.commit_sha, "--json"])
        .expect("git-ai diff --json should succeed");
    let json: Value = serde_json::from_str(&json_output).expect("diff JSON should parse");
    assert!(json["files"].get("src/visible.txt").is_some());
    assert!(json["files"].get("ignored/secret.txt").is_none());

    let hunks = json["hunks"].as_array().expect("hunks should be an array");
    assert!(hunks.iter().all(|hunk| {
        hunk.get("file_path")
            .and_then(|value| value.as_str())
            .map(|file| file == "src/visible.txt")
            .unwrap_or(false)
    }));
}

#[test]
fn test_diff_blame_deletions_terminal_annotations() {
    let repo = TestRepo::new();

    let mut file = repo.filename("deletion_terminal.txt");
    file.set_contents(lines!["keep".human(), "delete ai".ai(), "tail".human()]);
    repo.stage_all_and_commit("Seed AI deletion line").unwrap();

    file.set_contents(lines!["keep".human(), "tail".human()]);
    let deletion_commit = repo.stage_all_and_commit("Delete AI line").unwrap();

    let without_flag = repo
        .git_ai(&["diff", &deletion_commit.commit_sha])
        .expect("diff without --blame-deletions should succeed");
    let without_line = parse_diff_output(&without_flag)
        .into_iter()
        .find(|line| line.prefix == "-" && line.content.contains("delete ai"))
        .expect("expected deleted line in diff output");
    let without_has_ai = without_line
        .attribution
        .as_ref()
        .map(|value| value.contains("ai"))
        .unwrap_or(false);
    assert!(
        !without_has_ai,
        "deleted line should not have AI attribution without --blame-deletions"
    );

    let with_flag = repo
        .git_ai(&["diff", &deletion_commit.commit_sha, "--blame-deletions"])
        .expect("diff with --blame-deletions should succeed");
    let with_line = parse_diff_output(&with_flag)
        .into_iter()
        .find(|line| line.prefix == "-" && line.content.contains("delete ai"))
        .expect("expected deleted line in diff output");
    let with_has_ai = with_line
        .attribution
        .as_ref()
        .map(|value| value.contains("ai"))
        .unwrap_or(false);
    assert!(
        with_has_ai,
        "deleted line should include AI attribution with --blame-deletions, got: {:?}",
        with_line
    );
}

#[test]
fn test_diff_blame_deletions_since_accepts_git_date_specs() {
    let repo = TestRepo::new();

    let mut file = repo.filename("deletion_since.txt");
    file.set_contents(lines!["keep".human(), "remove me".ai(), "tail".human()]);
    repo.stage_all_and_commit("Seed AI line").unwrap();

    file.set_contents(lines!["keep".human(), "tail".human()]);
    let deletion_commit = repo.stage_all_and_commit("Delete AI line").unwrap();

    let json_output = repo
        .git_ai(&[
            "diff",
            &deletion_commit.commit_sha,
            "--json",
            "--blame-deletions",
            "--blame-deletions-since",
            "2999-01-01",
        ])
        .expect("diff --json with blame-deletions-since should succeed");
    let json: Value = serde_json::from_str(&json_output).expect("diff JSON should parse");

    let deletion_hunks: Vec<&Value> = json["hunks"]
        .as_array()
        .expect("hunks should be array")
        .iter()
        .filter(|hunk| hunk["file_path"] == "deletion_since.txt" && hunk["hunk_kind"] == "deletion")
        .collect();
    assert!(!deletion_hunks.is_empty(), "expected deletion hunks");
    let relative_date_output = repo
        .git_ai(&[
            "diff",
            &deletion_commit.commit_sha,
            "--json",
            "--blame-deletions",
            "--blame-deletions-since",
            "2 weeks ago",
        ])
        .expect("diff with relative blame-deletions-since date should succeed");
    let relative_json: Value =
        serde_json::from_str(&relative_date_output).expect("relative date JSON should parse");
    let relative_deletion_hunks = relative_json["hunks"]
        .as_array()
        .expect("hunks should be array")
        .iter()
        .filter(|hunk| hunk["file_path"] == "deletion_since.txt" && hunk["hunk_kind"] == "deletion")
        .count();
    assert!(
        relative_deletion_hunks > 0,
        "relative date should still produce deletion hunks"
    );
}

#[test]
fn test_diff_json_deleted_hunks_line_level_exact_mapping() {
    let repo = TestRepo::new();

    let mut file = repo.filename("deletion_exact.txt");
    file.set_contents(lines![
        "keep head".human(),
        "AI drop one".ai(),
        "human drop".human(),
        "AI drop two".ai(),
        "keep tail".human()
    ]);
    let source_commit = repo
        .stage_all_and_commit("Seed exact deletion lines")
        .unwrap();
    let source_prompt_id = single_prompt_id(&source_commit);

    file.set_contents(lines!["keep head".human(), "keep tail".human()]);
    let deletion_commit = repo
        .stage_all_and_commit("Delete exact target lines")
        .unwrap();

    let json_output = repo
        .git_ai(&[
            "diff",
            &deletion_commit.commit_sha,
            "--json",
            "--blame-deletions",
        ])
        .expect("diff --json --blame-deletions should succeed");
    let json: Value = serde_json::from_str(&json_output).expect("diff JSON should parse");

    let deletion_hunks = parse_json_hunks(&json, "deletion_exact.txt", "deletion");
    let expected = vec![
        JsonHunk {
            commit_sha: deletion_commit.commit_sha.clone(),
            content_hash: sha256_hex("AI drop one"),
            hunk_kind: "deletion".to_string(),
            original_commit_sha: Some(source_commit.commit_sha.clone()),
            start_line: 2,
            end_line: 2,
            file_path: "deletion_exact.txt".to_string(),
            prompt_id: Some(source_prompt_id.clone()),
        },
        JsonHunk {
            commit_sha: deletion_commit.commit_sha.clone(),
            content_hash: sha256_hex("human drop"),
            hunk_kind: "deletion".to_string(),
            original_commit_sha: Some(source_commit.commit_sha.clone()),
            start_line: 3,
            end_line: 3,
            file_path: "deletion_exact.txt".to_string(),
            prompt_id: None,
        },
        JsonHunk {
            commit_sha: deletion_commit.commit_sha.clone(),
            content_hash: sha256_hex("AI drop two"),
            hunk_kind: "deletion".to_string(),
            original_commit_sha: Some(source_commit.commit_sha.clone()),
            start_line: 4,
            end_line: 4,
            file_path: "deletion_exact.txt".to_string(),
            prompt_id: Some(source_prompt_id),
        },
    ];
    assert_eq!(deletion_hunks, expected);

    let expected_commit_keys = BTreeSet::from([
        source_commit.commit_sha.clone(),
        deletion_commit.commit_sha.clone(),
    ]);
    assert_eq!(commit_keys(&json), expected_commit_keys);

    let commits = json["commits"]
        .as_object()
        .expect("commits should be object");
    assert_eq!(
        commits[&source_commit.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "Seed exact deletion lines"
    );
    assert_eq!(
        commits[&deletion_commit.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "Delete exact target lines"
    );
}

#[test]
fn test_diff_json_deleted_hunks_exact_replacement_from_known_origin_commit() {
    let repo = TestRepo::new();
    let mut file = repo.filename("replacement_exact.txt");

    file.set_contents(lines!["a".ai(), "b".ai(), "c".ai()]);
    let commit_a = repo.stage_all_and_commit("A writes abc").unwrap();
    let prompt_a = single_prompt_id(&commit_a);

    file.replace_at(0, "b".ai());
    let commit_b = repo.stage_all_and_commit("B replaces first line").unwrap();
    let prompt_b = single_prompt_id(&commit_b);

    let output = repo
        .git_ai(&["diff", &commit_b.commit_sha, "--json", "--blame-deletions"])
        .expect("diff --json --blame-deletions should succeed");
    let json: Value = serde_json::from_str(&output).expect("diff JSON should parse");

    let deletion_hunks = parse_json_hunks(&json, "replacement_exact.txt", "deletion");
    let addition_hunks = parse_json_hunks(&json, "replacement_exact.txt", "addition");

    assert_eq!(
        deletion_hunks,
        vec![JsonHunk {
            commit_sha: commit_b.commit_sha.clone(),
            content_hash: sha256_hex("a"),
            hunk_kind: "deletion".to_string(),
            original_commit_sha: Some(commit_a.commit_sha.clone()),
            start_line: 1,
            end_line: 1,
            file_path: "replacement_exact.txt".to_string(),
            prompt_id: Some(prompt_a),
        }]
    );
    assert_eq!(
        addition_hunks,
        vec![JsonHunk {
            commit_sha: commit_b.commit_sha.clone(),
            content_hash: sha256_hex("b"),
            hunk_kind: "addition".to_string(),
            original_commit_sha: None,
            start_line: 1,
            end_line: 1,
            file_path: "replacement_exact.txt".to_string(),
            prompt_id: Some(prompt_b),
        }]
    );

    let expected_commit_keys =
        BTreeSet::from([commit_a.commit_sha.clone(), commit_b.commit_sha.clone()]);
    assert_eq!(commit_keys(&json), expected_commit_keys);
    let commits = json["commits"]
        .as_object()
        .expect("commits should be object");
    assert_eq!(
        commits[&commit_a.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "A writes abc"
    );
    assert_eq!(
        commits[&commit_b.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "B replaces first line"
    );
}

#[test]
fn test_diff_json_deleted_hunks_strict_mixed_origins_and_contiguous_segments() {
    let repo = TestRepo::new();
    let mut file = repo.filename("mixed_origin_exact.txt");

    file.set_contents(lines![
        "A1-ai".ai(),
        "A2-human".human(),
        "A3-ai".ai(),
        "A4-human".human(),
        "A5-ai".ai()
    ]);
    let commit_a = repo.stage_all_and_commit("A baseline mixed lines").unwrap();
    let prompt_a_line_1 = prompt_id_for_line_in_commit(&commit_a, "mixed_origin_exact.txt", 1)
        .expect("line 1 in commit A should be AI-attributed");
    let prompt_a_line_5 = prompt_id_for_line_in_commit(&commit_a, "mixed_origin_exact.txt", 5)
        .expect("line 5 in commit A should be AI-attributed");

    file.delete_range(2, 4);
    file.insert_at(2, vec!["B3-ai".ai(), "B4-ai".ai()]);
    let commit_b = repo
        .stage_all_and_commit("B rewrites middle lines")
        .unwrap();
    let prompt_b = prompt_id_for_line_in_commit(&commit_b, "mixed_origin_exact.txt", 3)
        .expect("line 3 in commit B should be AI-attributed");

    file.delete_range(2, 5);
    file.delete_at(0);
    let commit_c = repo
        .stage_all_and_commit("C deletes mixed-origin ranges")
        .unwrap();

    let output = repo
        .git_ai(&["diff", &commit_c.commit_sha, "--json", "--blame-deletions"])
        .expect("diff --json --blame-deletions should succeed");
    let json: Value = serde_json::from_str(&output).expect("diff JSON should parse");

    let deletion_hunks = parse_json_hunks(&json, "mixed_origin_exact.txt", "deletion");
    let addition_hunks = parse_json_hunks(&json, "mixed_origin_exact.txt", "addition");

    assert_eq!(
        addition_hunks,
        vec![JsonHunk {
            commit_sha: commit_c.commit_sha.clone(),
            content_hash: sha256_hex("A2-human"),
            hunk_kind: "addition".to_string(),
            original_commit_sha: None,
            start_line: 1,
            end_line: 1,
            file_path: "mixed_origin_exact.txt".to_string(),
            prompt_id: None,
        }]
    );
    assert_eq!(
        deletion_hunks,
        vec![
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("A1-ai"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_a.commit_sha.clone()),
                start_line: 1,
                end_line: 1,
                file_path: "mixed_origin_exact.txt".to_string(),
                prompt_id: Some(prompt_a_line_1),
            },
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("A2-human"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_a.commit_sha.clone()),
                start_line: 2,
                end_line: 2,
                file_path: "mixed_origin_exact.txt".to_string(),
                prompt_id: None,
            },
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("B3-ai\nB4-ai"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_b.commit_sha.clone()),
                start_line: 3,
                end_line: 4,
                file_path: "mixed_origin_exact.txt".to_string(),
                prompt_id: Some(prompt_b),
            },
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("A5-ai"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_a.commit_sha.clone()),
                start_line: 5,
                end_line: 5,
                file_path: "mixed_origin_exact.txt".to_string(),
                prompt_id: Some(prompt_a_line_5),
            },
        ]
    );

    let expected_commit_keys = BTreeSet::from([
        commit_a.commit_sha.clone(),
        commit_b.commit_sha.clone(),
        commit_c.commit_sha.clone(),
    ]);
    assert_eq!(commit_keys(&json), expected_commit_keys);
    let commits = json["commits"]
        .as_object()
        .expect("commits should be object");
    assert_eq!(
        commits[&commit_a.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "A baseline mixed lines"
    );
    assert_eq!(
        commits[&commit_b.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "B rewrites middle lines"
    );
    assert_eq!(
        commits[&commit_c.commit_sha]["msg"]
            .as_str()
            .expect("msg should be string"),
        "C deletes mixed-origin ranges"
    );
}

#[test]
fn test_diff_json_deleted_hunks_same_content_but_different_origins() {
    let repo = TestRepo::new();
    let mut file = repo.filename("duplicate_content_exact.txt");

    file.set_contents(lines![
        "top".human(),
        "dup".ai(),
        "middle".human(),
        "tail".human()
    ]);
    let commit_a = repo.stage_all_and_commit("A creates first dup").unwrap();
    let prompt_a = prompt_id_for_line_in_commit(&commit_a, "duplicate_content_exact.txt", 2)
        .expect("line 2 in commit A should be AI-attributed");

    file.insert_at(3, vec!["dup".ai()]);
    let commit_b = repo.stage_all_and_commit("B adds second dup").unwrap();
    let prompt_b = prompt_id_for_line_in_commit(&commit_b, "duplicate_content_exact.txt", 4)
        .expect("line 4 in commit B should be AI-attributed");

    file.delete_at(3);
    file.delete_at(1);
    let commit_c = repo
        .stage_all_and_commit("C deletes both dup lines")
        .unwrap();

    let output = repo
        .git_ai(&["diff", &commit_c.commit_sha, "--json", "--blame-deletions"])
        .expect("diff --json --blame-deletions should succeed");
    let json: Value = serde_json::from_str(&output).expect("diff JSON should parse");

    let deletion_hunks = parse_json_hunks(&json, "duplicate_content_exact.txt", "deletion");
    assert_eq!(
        deletion_hunks,
        vec![
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("dup"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_a.commit_sha.clone()),
                start_line: 2,
                end_line: 2,
                file_path: "duplicate_content_exact.txt".to_string(),
                prompt_id: Some(prompt_a),
            },
            JsonHunk {
                commit_sha: commit_c.commit_sha.clone(),
                content_hash: sha256_hex("dup"),
                hunk_kind: "deletion".to_string(),
                original_commit_sha: Some(commit_b.commit_sha.clone()),
                start_line: 4,
                end_line: 4,
                file_path: "duplicate_content_exact.txt".to_string(),
                prompt_id: Some(prompt_b),
            },
        ]
    );

    let expected_commit_keys = BTreeSet::from([
        commit_a.commit_sha.clone(),
        commit_b.commit_sha.clone(),
        commit_c.commit_sha.clone(),
    ]);
    assert_eq!(commit_keys(&json), expected_commit_keys);
}

#[test]
fn test_diff_json_commit_author_is_full_ident() {
    let repo = TestRepo::new();
    let mut file = repo.filename("author_ident.txt");
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    file.set_contents(lines!["base".human(), "AI line".ai()]);
    let commit = repo.stage_all_and_commit("Add AI line").unwrap();

    let json = diff_json(&repo, &["diff", &commit.commit_sha, "--json"]);
    let author = json["commits"][&commit.commit_sha]["author"]
        .as_str()
        .expect("commit author should be a string");
    assert_eq!(author, "Test User <test@example.com>");
}

reuse_tests_in_worktree!(
    test_diff_single_commit,
    test_diff_commit_range,
    test_diff_shows_ai_attribution,
    test_diff_shows_human_attribution,
    test_diff_multiple_files,
    test_diff_initial_commit,
    test_diff_pure_additions,
    test_diff_pure_deletions,
    test_diff_mixed_ai_and_human,
    test_diff_with_head_ref,
    test_diff_output_format,
    test_diff_error_on_no_args,
    test_diff_json_output_with_escaped_newlines,
    test_diff_json_omits_commit_stats_without_include_stats_flag,
    test_diff_json_all_prompts_includes_non_landing_prompts,
    test_diff_json_include_stats_exact_single_model_counts,
    test_diff_json_include_stats_exact_multi_model_with_non_landing_prompt,
    test_diff_json_include_stats_exact_human_landed_with_ai_generated,
    test_diff_json_include_stats_blame_deletions_devin_added_prompts_only,
    test_diff_json_rename_only_has_no_hunks_and_zero_stats,
    test_diff_json_rename_with_ai_edit_exact_stats,
    test_diff_json_blame_deletions_rename_with_edit_uses_old_path,
    test_diff_json_include_stats_rejects_commit_ranges,
    test_diff_preserves_context_lines,
    test_diff_exact_sequence_verification,
    test_diff_range_multiple_commits,
    test_diff_ignores_repo_external_diff_helper_but_proxy_uses_it,
    test_diff_parsing_is_stable_under_hostile_diff_config,
    test_checkpoint_and_commit_ignore_repo_external_diff_helper,
    test_diff_ignores_git_external_diff_env_but_proxy_uses_it,
    test_diff_ignores_git_diff_opts_env_for_internal_diff,
    test_diff_respects_effective_ignore_patterns,
    test_diff_blame_deletions_terminal_annotations,
    test_diff_blame_deletions_since_accepts_git_date_specs,
    test_diff_json_deleted_hunks_line_level_exact_mapping,
    test_diff_json_deleted_hunks_exact_replacement_from_known_origin_commit,
    test_diff_json_deleted_hunks_strict_mixed_origins_and_contiguous_segments,
    test_diff_json_deleted_hunks_same_content_but_different_origins,
    test_diff_json_commit_author_is_full_ident,
);
