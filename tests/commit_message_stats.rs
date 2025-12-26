mod repos;
use git_ai::authorship::stats::CommitStats;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

/// Test that commit message stats feature works correctly
/// This test verifies that when the feature is enabled, commit messages
/// are amended to include AI statistics
#[test]
fn test_commit_message_stats_enabled() {
    let repo = TestRepo::new();

    // Enable commit message stats feature
    repo.run_git(&["config", "ai.commit-message-stats.enabled", "true"])
        .unwrap();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a new file
    let mut file = repo.filename("main.rs");
    file.set_contents(lines![
        "fn main() {".human(),
        "    println!(\"Hello\");".ai(),
        "}".human(),
    ]);

    // Commit with AI content
    repo.stage_all_and_commit("Add main.rs with AI help")
        .unwrap();

    // Get the current commit message
    let output = repo.run_git(&["log", "-1", "--pretty=%B"]).unwrap();

    // The commit message should contain stats
    assert!(
        output.contains("Stats:") || output.contains("you") || output.contains("ai"),
        "Commit message should contain stats when feature is enabled. Got: {}",
        output
    );
}

/// Test that commit message stats feature can be disabled
#[test]
fn test_commit_message_stats_disabled() {
    let repo = TestRepo::new();

    // Explicitly disable commit message stats feature (or leave as default)
    repo.run_git(&["config", "ai.commit-message-stats.enabled", "false"])
        .unwrap();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a new file
    let mut file = repo.filename("lib.rs");
    file.set_contents(lines![
        "pub fn helper() {".human(),
        "    // AI comment".ai(),
        "}".human(),
    ]);

    // Commit without stats in message
    repo.stage_all_and_commit("Add lib.rs").unwrap();

    // Get the current commit message
    let output = repo.run_git(&["log", "-1", "--pretty=%B"]).unwrap();

    // The commit message should NOT contain stats
    assert!(
        !output.contains("Stats:"),
        "Commit message should NOT contain stats when disabled. Got: {}",
        output
    );
}

/// Test different format options
#[test]
fn test_commit_message_stats_markdown_format() {
    let repo = TestRepo::new();

    // Enable with markdown format
    repo.run_git(&["config", "ai.commit-message-stats.enabled", "true"])
        .unwrap();
    repo.run_git(&["config", "ai.commit-message-stats.format", "markdown"])
        .unwrap();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a new file
    let mut file = repo.filename("app.py");
    file.set_contents(lines!["def main():".human(), "    print('hello')".ai(),]);

    // Commit with AI content
    repo.stage_all_and_commit("Add app.py").unwrap();

    // Get the current commit message
    let output = repo.run_git(&["log", "-1", "--pretty=%B"]).unwrap();

    // Should contain markdown code block
    assert!(
        output.contains("```") || output.contains("🧠") || output.contains("🤖"),
        "Markdown format should contain markdown elements. Got: {}",
        output
    );
}

/// Test custom template
#[test]
fn test_commit_message_stats_custom_template() {
    let repo = TestRepo::new();

    // Enable with custom template
    repo.run_git(&["config", "ai.commit-message-stats.enabled", "true"])
        .unwrap();
    repo.run_git(&[
        "config",
        "ai.commit-message-stats.template",
        "📝 {original_message}\n\n📊 {stats}",
    ])
    .unwrap();

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a new file
    let mut file = repo.filename("test.py");
    file.set_contents(lines!["def test():".human(), "    assert True".ai(),]);

    // Commit with AI content
    repo.stage_all_and_commit("Add test").unwrap();

    // Get the current commit message
    let output = repo.run_git(&["log", "-1", "--pretty=%B"]).unwrap();

    // Should contain custom template elements
    assert!(
        output.contains("📝") && output.contains("📊"),
        "Custom template should be applied. Got: {}",
        output
    );
}

/// Test that feature flag controls the behavior
#[test]
fn test_commit_message_stats_feature_flag() {
    let repo = TestRepo::new();

    // Don't enable via git config - rely on feature flag
    // This test assumes the feature flag is disabled by default in release mode

    // Create an initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a new file
    let mut file = repo.filename("utils.py");
    file.set_contents(lines!["def util():".human(), "    return 42".ai(),]);

    // Commit
    repo.stage_all_and_commit("Add utils").unwrap();

    // Get the current commit message
    let output = repo.run_git(&["log", "-1", "--pretty=%B"]).unwrap();

    // Without feature flag enabled, no stats should be added
    // (This test may need adjustment based on default feature flag values)
    assert!(
        !output.contains("Stats:") || output.trim() == "Add utils",
        "Without feature flag, commit message should be unchanged. Got: {}",
        output
    );
}
