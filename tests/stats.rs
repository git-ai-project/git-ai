mod repos;
use git_ai::authorship::stats::CommitStats;
use insta::assert_debug_snapshot;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json;

/// Extract the first complete JSON object from mixed stdout/stderr output.
fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

#[test]
fn test_authorship_log_stats() {
    let repo = TestRepo::new();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a brand new file with planets
    let mut file = repo.filename("planets.txt");
    file.set_contents(lines![
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

    file.set_contents(lines![
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

    file.assert_lines_and_blame(lines![
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
    file.set_contents(lines!["Line 1".human()]);
    let first = repo.stage_all_and_commit("Initial human").unwrap();

    // AI adds a line in a second commit
    file.set_contents(lines!["Line 1".human(), "Line 2".ai()]);
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
fn test_stats_cli_empty_tree_range() {
    let repo = TestRepo::new();

    // First commit: AI line
    let mut file = repo.filename("history.txt");
    file.set_contents(lines!["AI Line 1".ai()]);
    let _first = repo.stage_all_and_commit("Initial AI").unwrap();

    // Second commit: human line
    file.set_contents(lines!["AI Line 1".ai(), "Human Line 2".human()]);
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
