use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::stats::CommitStats;
use insta::assert_debug_snapshot;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Extract the first complete JSON object from mixed stdout/stderr output.
fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

fn stats_from_args(repo: &TestRepo, args: &[&str]) -> CommitStats {
    let raw = repo.git_ai(args).expect("git-ai stats should succeed");
    let json = extract_json_object(&raw);
    serde_json::from_str(&json).expect("valid stats json")
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn configure_repo_external_diff_helper(repo: &TestRepo) -> String {
    let marker = "STATS_EXTERNAL_DIFF_MARKER";
    let helper_path = repo.path().join("stats-ext-diff-helper.sh");
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

#[test]
fn test_authorship_log_stats() {
    let repo = TestRepo::new();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a brand new file with planets
    let mut file = repo.filename("planets.txt");
    file.set_contents(crate::lines![
        "Mercury".human(),
        "Venus".human(),
        "Earth".ai(),
        "Mars".ai(),
        "Jupiter".human(),
        "Saturn".ai(),
        "Uranus".ai(),
        "Neptune".ai(),
        "Pluto (dwarf)".ai(),
    ]);

    file.set_contents(crate::lines![
        "Mercury".human(),
        "Venus".human(),
        "Earth".ai(),
        "Mars".ai(),
        "Jupiter".human(),
        "Saturn".ai(),
        "Uranus".ai(),
        "Neptune (override)".human(),
        "Pluto (dwarf)".ai(),
    ]);

    // First commit should have all the planets
    let first_commit = repo.stage_all_and_commit("Add planets").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Mercury".human(),
        "Venus".human(),
        "Earth".ai(),
        "Mars".ai(),
        "Jupiter".human(),
        "Saturn".ai(),
        "Uranus".ai(),
        "Neptune (override)".human(),
        "Pluto (dwarf)".ai(),
    ]);

    assert_eq!(first_commit.authorship_log.attestations.len(), 1);

    let raw = repo.git_ai(&["stats", "--json"]).unwrap();
    let json = extract_json_object(&raw);
    let stats: CommitStats = serde_json::from_str(&json).unwrap();
    assert_eq!(stats.human_additions, 4);
    assert_eq!(stats.mixed_additions, 1);
    assert_eq!(stats.ai_additions, 6); // Includes the one mixed line (Neptune (override))
    assert_eq!(stats.ai_accepted, 5);
    assert_eq!(stats.total_ai_additions, 11);
    assert_eq!(stats.total_ai_deletions, 11);
    assert_eq!(stats.git_diff_deleted_lines, 0);
    assert_eq!(stats.git_diff_added_lines, 9);

    assert_eq!(stats.tool_model_breakdown.len(), 1);
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .ai_additions,
        6
    );
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .ai_accepted,
        5
    );
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .total_ai_additions,
        11
    );
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .total_ai_deletions,
        11
    );
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .mixed_additions,
        1
    );
    assert_eq!(
        stats
            .tool_model_breakdown
            .get("mock_ai::unknown")
            .unwrap()
            .time_waiting_for_ai,
        0
    );
}

#[test]
fn test_stats_cli_range() {
    let repo = TestRepo::new();

    // Initial human commit
    let mut file = repo.filename("range.txt");
    file.set_contents(crate::lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("Initial human").unwrap();

    // AI adds a line in a second commit
    file.set_contents(crate::lines!["Line 1".human(), "Line 2".ai()]);
    let second = repo.stage_all_and_commit("AI adds line").unwrap();

    // Sanity check individual commit stats
    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let raw = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("git-ai stats range should succeed");

    let output = extract_json_object(&raw);
    let stats: git_ai::authorship::range_authorship::RangeAuthorshipStats =
        serde_json::from_str(&output).unwrap();

    // Range should only include the AI commit's diff and report at least one AI-added line
    assert_eq!(stats.authorship_stats.total_commits, 1);
    assert!(
        stats.range_stats.ai_additions >= 1,
        "expected at least one AI addition in range, got {}",
        stats.range_stats.ai_additions
    );
    assert!(
        stats.range_stats.git_diff_added_lines >= stats.range_stats.ai_additions,
        "git diff added lines ({}) should be >= ai_additions ({})",
        stats.range_stats.git_diff_added_lines,
        stats.range_stats.ai_additions
    );
}

#[test]
fn test_stats_cli_range_ignores_repo_external_diff_helper() {
    let repo = TestRepo::new();

    let mut file = repo.filename("stats-range-ext.txt");
    file.set_contents(crate::lines!["base".human()]);
    let first = repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(crate::lines!["base".human(), "ai line".ai()]);
    let second = repo.stage_all_and_commit("ai second").unwrap();

    let marker = configure_repo_external_diff_helper(&repo);
    let proxied_diff = repo
        .git(&["diff", &first.commit_sha, &second.commit_sha])
        .expect("proxied git diff should succeed");
    assert!(
        proxied_diff.contains(&marker),
        "sanity check: proxied git diff should use configured external helper"
    );

    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let raw = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("git-ai stats range should succeed with external diff configured");
    assert!(
        !raw.contains(&marker),
        "git-ai stats output should not include external helper output, got:\n{}",
        raw
    );

    let output = extract_json_object(&raw);
    let stats: git_ai::authorship::range_authorship::RangeAuthorshipStats =
        serde_json::from_str(&output).unwrap();
    assert_eq!(stats.authorship_stats.total_commits, 1);
    assert!(
        stats.range_stats.git_diff_added_lines >= 1,
        "expected at least one added line in range, got {}",
        stats.range_stats.git_diff_added_lines
    );
    assert!(stats.range_stats.ai_additions >= 1);
}

#[test]
fn test_stats_cli_range_with_hostile_diff_config() {
    let repo = TestRepo::new();

    let mut file = repo.filename("stats-range-hostile.txt");
    file.set_contents(crate::lines!["base".human()]);
    let first = repo.stage_all_and_commit("initial").unwrap();

    file.set_contents(crate::lines!["base".human(), "ai line".ai()]);
    let second = repo.stage_all_and_commit("ai second").unwrap();

    configure_hostile_diff_settings(&repo);

    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let raw = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("git-ai stats range should succeed with hostile diff config");
    let output = extract_json_object(&raw);
    let stats: git_ai::authorship::range_authorship::RangeAuthorshipStats =
        serde_json::from_str(&output).unwrap();

    assert_eq!(stats.authorship_stats.total_commits, 1);
    assert!(stats.range_stats.git_diff_added_lines >= 1);
    assert!(stats.range_stats.ai_additions >= 1);
}

#[test]
fn test_stats_cli_empty_tree_range() {
    let repo = TestRepo::new();

    // First commit: AI line
    let mut file = repo.filename("history.txt");
    file.set_contents(crate::lines!["AI Line 1".ai()]);
    let _first = repo.stage_all_and_commit("Initial AI").unwrap();

    // Second commit: human line
    file.set_contents(crate::lines!["AI Line 1".ai(), "Human Line 2".human()]);
    repo.stage_all_and_commit("Human adds line").unwrap();

    // Git's empty tree OID
    let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let head = repo
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse HEAD should succeed")
        .trim()
        .to_string();
    let range = format!("{}..{}", empty_tree, head);

    let raw = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("git-ai stats empty-tree range should succeed");

    let output = extract_json_object(&raw);
    let stats: git_ai::authorship::range_authorship::RangeAuthorshipStats =
        serde_json::from_str(&output).unwrap();

    // Entire history from empty tree to HEAD:
    // - 2 commits in range
    // - 1 AI-added line, 1 human-added line in final diff
    assert_eq!(stats.authorship_stats.total_commits, 2);
    assert_eq!(stats.range_stats.git_diff_added_lines, 2);
    assert_eq!(stats.range_stats.ai_additions, 1);
    // human_additions is computed as git_diff_added_lines - ai_accepted
    assert_eq!(stats.range_stats.human_additions, 1);
}

#[test]
fn test_markdown_stats_deletion_only() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    let stats = CommitStats {
        human_additions: 0,
        mixed_additions: 0,
        ai_additions: 0,
        ai_accepted: 0,
        total_ai_additions: 0,
        total_ai_deletions: 5,
        time_waiting_for_ai: 0,
        git_diff_deleted_lines: 5,
        git_diff_added_lines: 0,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_all_human() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    let stats = CommitStats {
        human_additions: 10,
        mixed_additions: 0,
        ai_additions: 0,
        ai_accepted: 0,
        total_ai_additions: 0,
        total_ai_deletions: 0,
        time_waiting_for_ai: 0,
        git_diff_deleted_lines: 0,
        git_diff_added_lines: 10,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_all_ai() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    let stats = CommitStats {
        human_additions: 0,
        mixed_additions: 0,
        ai_additions: 15,
        ai_accepted: 15,
        total_ai_additions: 15,
        total_ai_deletions: 0,
        time_waiting_for_ai: 30,
        git_diff_deleted_lines: 0,
        git_diff_added_lines: 15,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_mixed() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    let stats = CommitStats {
        human_additions: 10,
        mixed_additions: 5,
        ai_additions: 20,
        ai_accepted: 15,
        total_ai_additions: 25,
        total_ai_deletions: 10,
        time_waiting_for_ai: 45,
        git_diff_deleted_lines: 5,
        git_diff_added_lines: 30,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_no_mixed() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    let stats = CommitStats {
        human_additions: 8,
        mixed_additions: 0,
        ai_additions: 12,
        ai_accepted: 12,
        total_ai_additions: 12,
        total_ai_deletions: 0,
        time_waiting_for_ai: 15,
        git_diff_deleted_lines: 0,
        git_diff_added_lines: 20,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_minimal_human() {
    use git_ai::authorship::stats::write_stats_to_markdown;
    use std::collections::BTreeMap;

    // Test that humans get at least 2 visible blocks if they have more than 1 line
    let stats = CommitStats {
        human_additions: 2,
        mixed_additions: 0,
        ai_additions: 98,
        ai_accepted: 98,
        total_ai_additions: 98,
        total_ai_deletions: 0,
        time_waiting_for_ai: 10,
        git_diff_deleted_lines: 0,
        git_diff_added_lines: 100,
        tool_model_breakdown: BTreeMap::new(),
    };

    let markdown = write_stats_to_markdown(&stats);

    assert_debug_snapshot!(markdown);
}

#[test]
fn test_markdown_stats_formatting() {
    use git_ai::authorship::stats::{ToolModelHeadlineStats, write_stats_to_markdown};
    use std::collections::BTreeMap;

    let mut tool_model_breakdown = BTreeMap::new();
    tool_model_breakdown.insert(
        "cursor::claude-3.5-sonnet".to_string(),
        ToolModelHeadlineStats {
            ai_additions: 8,
            mixed_additions: 2,
            ai_accepted: 6,
            total_ai_additions: 10,
            total_ai_deletions: 3,
            time_waiting_for_ai: 25,
        },
    );

    let stats = CommitStats {
        human_additions: 5,
        mixed_additions: 2,
        ai_additions: 8,
        ai_accepted: 6,
        total_ai_additions: 10,
        total_ai_deletions: 3,
        time_waiting_for_ai: 25,
        git_diff_deleted_lines: 2,
        git_diff_added_lines: 13,
        tool_model_breakdown,
    };

    let markdown = write_stats_to_markdown(&stats);
    println!("{}", markdown);
    assert_debug_snapshot!(markdown);
}

#[test]
fn test_stats_default_ignores_snapshot_files() {
    let repo = TestRepo::new();
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    repo.filename("src/main.rs")
        .set_contents(crate::lines!["fn main() {}".ai()]);
    repo.filename("__snapshots__/main.snap")
        .set_contents(crate::lines![
            "snapshot line 1",
            "snapshot line 2",
            "snapshot line 3"
        ]);
    repo.stage_all_and_commit("Add source and snapshot")
        .unwrap();

    let stats = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
}

#[test]
fn test_stats_default_ignores_lockfiles_and_generated_files() {
    let repo = TestRepo::new();
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    repo.filename("src/lib.rs")
        .set_contents(crate::lines!["pub fn answer() -> u32 { 42 }".ai()]);
    repo.filename("Cargo.lock")
        .set_contents(vec!["lock".to_string().repeat(5); 650]);
    repo.filename("api.generated.ts")
        .set_contents(vec!["export type X = string;".to_string(); 500]);
    repo.stage_all_and_commit("Add source and generated artifacts")
        .unwrap();

    let stats = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
}

#[test]
fn test_stats_ignores_linguist_generated_patterns() {
    let repo = TestRepo::new();
    repo.filename(".gitattributes")
        .set_contents(crate::lines!["generated/** linguist-generated=true"]);
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit with gitattributes")
        .unwrap();

    repo.filename("src/main.rs")
        .set_contents(crate::lines!["fn run() {}".ai()]);
    repo.filename("generated/schema.ts")
        .set_contents(crate::lines!["export const schema = {};"]);
    repo.stage_all_and_commit("Add source and linguist-generated file")
        .unwrap();

    let stats = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
}

#[test]
fn test_stats_keeps_negative_linguist_patterns_counted() {
    let repo = TestRepo::new();
    repo.filename(".gitattributes").set_contents(crate::lines![
        "generated/** linguist-generated=true",
        "manual/** linguist-generated=false"
    ]);
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit with attrs")
        .unwrap();

    repo.filename("generated/out.ts")
        .set_contents(crate::lines!["export const ignored = true;"]);
    repo.filename("manual/kept.ts")
        .set_contents(crate::lines!["export const counted = true;".ai()]);
    repo.stage_all_and_commit("Add generated and manual files")
        .unwrap();

    let stats = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.ai_additions, 1);
}

#[test]
fn test_stats_in_bare_clone_uses_root_gitattributes_linguist_generated() {
    let repo = TestRepo::new();
    repo.filename(".gitattributes")
        .set_contents(crate::lines!["generated/** linguist-generated=true"]);
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit with gitattributes")
        .unwrap();

    repo.filename("src/main.rs")
        .set_contents(crate::lines!["fn run() {}".ai()]);
    repo.filename("generated/schema.ts")
        .set_contents(crate::lines!["export const schema = {};"]);
    repo.stage_all_and_commit("Add source and linguist-generated file")
        .unwrap();

    let temp = tempfile::tempdir().expect("tempdir");
    let bare = temp.path().join("repo.git");
    run_git(
        temp.path(),
        &[
            "clone",
            "--bare",
            repo.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );

    let output = Command::new(crate::repos::test_repo::get_binary_path())
        .args(["stats", "HEAD", "--json"])
        .current_dir(&bare)
        .env(
            "GIT_AI_TEST_DB_PATH",
            temp.path().join("db").to_str().unwrap(),
        )
        .output()
        .expect("git-ai stats should run in bare repo");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "git-ai stats failed in bare clone:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let combined = if stdout.is_empty() {
        stderr.to_string()
    } else if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{}{}", stdout, stderr)
    };
    let json = extract_json_object(&combined);
    let stats: CommitStats = serde_json::from_str(&json).expect("valid stats json");
    assert_eq!(stats.git_diff_added_lines, 1);
}

#[test]
fn test_stats_ignore_flag_is_additive_to_defaults() {
    let repo = TestRepo::new();
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    repo.filename("src/main.rs")
        .set_contents(crate::lines!["fn main() {}".ai()]);
    repo.filename("docs/keep.txt")
        .set_contents(crate::lines!["this line is human"]);
    repo.stage_all_and_commit("Add docs and source").unwrap();

    let baseline = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(baseline.git_diff_added_lines, 2);

    let ignored = stats_from_args(
        &repo,
        &["stats", "HEAD", "--json", "--ignore", "docs/keep.txt"],
    );
    assert_eq!(ignored.git_diff_added_lines, 1);
}

#[test]
fn test_stats_range_uses_default_ignores() {
    let repo = TestRepo::new();
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    let first = repo.stage_all_and_commit("Initial commit").unwrap();

    repo.filename("src/main.rs")
        .set_contents(crate::lines!["fn main() {}".ai()]);
    repo.filename("Cargo.lock")
        .set_contents(vec!["lockdata".to_string(); 700]);
    let second = repo
        .stage_all_and_commit("Add source and lockfile")
        .unwrap();

    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let raw = repo
        .git_ai(&["stats", &range, "--json"])
        .expect("git-ai stats range should succeed");
    let json = extract_json_object(&raw);
    let range_stats: git_ai::authorship::range_authorship::RangeAuthorshipStats =
        serde_json::from_str(&json).unwrap();

    assert_eq!(range_stats.range_stats.git_diff_added_lines, 1);
    assert_eq!(range_stats.range_stats.ai_additions, 1);
}

#[test]
fn test_post_commit_large_ignored_files_do_not_trigger_skip_warning() {
    let repo = TestRepo::new();
    repo.filename("README.md")
        .set_contents(crate::lines!["# Repo"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    repo.filename("Cargo.lock")
        .set_contents(vec!["lockfile-entry".to_string(); 7001]);
    let commit = repo
        .stage_all_and_commit("Large lockfile update")
        .expect("commit should succeed");

    assert!(
        !commit
            .stdout
            .contains("Skipped git-ai stats for large commit"),
        "large ignored files should not trigger post-commit skip warning: {}",
        commit.stdout
    );

    let stats = stats_from_args(&repo, &["stats", "HEAD", "--json"]);
    assert_eq!(stats.git_diff_added_lines, 0);
    assert_eq!(stats.ai_additions, 0);
    assert_eq!(stats.human_additions, 0);
}

/// Test that merge commits with AI-resolved conflicts correctly show AI stats.
/// Regression test for https://github.com/git-ai-project/git-ai/issues/910
///
/// When AI resolves a merge conflict, `git ai blame` correctly attributes lines to AI,
/// but `git ai stats head` was incorrectly showing 100% human / 0% AI because
/// stats_for_commit_stats() skipped AI acceptance counting for all merge commits.
///
/// The test simulates the real-world flow:
/// 1. Start merge (conflicts occur, auto-resolved with -X theirs but not committed)
/// 2. AI checkpoint marks the conflict resolution as AI-authored
/// 3. Commit the merge (hooks write authorship notes from the working log)
#[test]
fn test_stats_merge_commit_with_ai_conflict_resolution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create base file
    file.set_contents(crate::lines!["Line 1", "Line 2", "Line 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with changes that will conflict
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(1, "AI FEATURE VERSION".ai());
    repo.stage_all_and_commit("Feature change").unwrap();

    // Go back to default branch and make conflicting human changes
    repo.git(&["checkout", &default_branch]).unwrap();
    file = repo.filename("test.txt");
    file.replace_at(1, "HUMAN MAIN VERSION");
    repo.stage_all_and_commit("Human main change").unwrap();

    // Merge feature branch: resolve conflicts with theirs but don't commit yet.
    // This simulates starting a merge that an AI tool will resolve.
    repo.git(&["merge", "feature", "--no-commit", "-X", "theirs"])
        .unwrap();

    // Run AI checkpoint on the resolved file — this is what happens in practice when
    // an AI tool (Cursor, Copilot, etc.) resolves the conflict and git-ai tracks it.
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    // Commit the merge. The hooks read the working log (which now contains the AI
    // checkpoint) and write authorship notes with attestations on the merge commit.
    repo.stage_all_and_commit("Merge feature with AI conflict resolution")
        .unwrap();

    // Verify blame correctly shows AI attribution
    file = repo.filename("test.txt");
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "AI FEATURE VERSION".ai(),
        "Line 3".human(),
    ]);

    // Verify stats correctly show AI additions (this is the bug from issue #910)
    let stats = repo.stats().unwrap();

    // The merge commit introduces 1 line change vs first parent (AI FEATURE VERSION
    // replacing HUMAN MAIN VERSION). That line was authored by AI, so stats should
    // reflect the AI contribution.
    assert!(
        stats.ai_accepted > 0,
        "Merge commit with AI-resolved conflict should have ai_accepted > 0, got: ai_accepted={}, human_additions={}, ai_additions={}",
        stats.ai_accepted,
        stats.human_additions,
        stats.ai_additions,
    );
}

/// Test that non-conflicting merge commits with AI involvement also show correct AI stats.
/// This simulates a scenario where AI assists during a non-conflicting merge (e.g., AI
/// review/modification of merged files before committing).
#[test]
fn test_stats_merge_commit_non_conflicting_ai_changes() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Create base file
    file.set_contents(crate::lines!["Line 1", "Line 2", "Line 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI additions in a separate file (no conflict)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let mut ai_file = repo.filename("ai_feature.txt");
    ai_file.set_contents(crate::lines!["AI Line 1".ai(), "AI Line 2".ai()]);
    repo.stage_all_and_commit("AI feature additions").unwrap();

    // Go back to default branch and make human changes to a different file
    repo.git(&["checkout", &default_branch]).unwrap();
    file = repo.filename("test.txt");
    file.replace_at(1, "HUMAN CHANGE");
    repo.stage_all_and_commit("Human main change").unwrap();

    // Merge feature branch without committing
    repo.git(&["merge", "feature", "--no-commit"]).unwrap();

    // Run AI checkpoint on the new AI file — simulates AI involvement in the merge
    repo.git_ai(&["checkpoint", "mock_ai", "ai_feature.txt"])
        .unwrap();

    // Commit the merge
    repo.stage_all_and_commit("Merge feature with AI additions")
        .unwrap();

    // Stats for the merge commit
    let stats = repo.stats().unwrap();

    // The merge introduces ai_feature.txt (2 AI lines) relative to the first parent.
    // With the AI checkpoint, the authorship notes should attribute them to AI.
    assert!(
        stats.ai_accepted > 0,
        "Non-conflicting merge with AI changes should have ai_accepted > 0, got: ai_accepted={}, human_additions={}, git_diff_added_lines={}",
        stats.ai_accepted,
        stats.human_additions,
        stats.git_diff_added_lines,
    );
}

crate::reuse_tests_in_worktree!(
    test_authorship_log_stats,
    test_stats_cli_range,
    test_stats_cli_empty_tree_range,
    test_markdown_stats_deletion_only,
    test_markdown_stats_all_human,
    test_markdown_stats_all_ai,
    test_markdown_stats_mixed,
    test_markdown_stats_no_mixed,
    test_markdown_stats_minimal_human,
    test_markdown_stats_formatting,
    test_stats_default_ignores_snapshot_files,
    test_stats_default_ignores_lockfiles_and_generated_files,
    test_stats_ignores_linguist_generated_patterns,
    test_stats_keeps_negative_linguist_patterns_counted,
    test_stats_in_bare_clone_uses_root_gitattributes_linguist_generated,
    test_stats_ignore_flag_is_additive_to_defaults,
    test_stats_range_uses_default_ignores,
    test_post_commit_large_ignored_files_do_not_trigger_skip_warning,
    test_stats_merge_commit_with_ai_conflict_resolution,
    test_stats_merge_commit_non_conflicting_ai_changes,
);
