use crate::repos::test_repo::TestRepo;

#[test]
fn test_dashboard_runs_without_crash() {
    let repo = TestRepo::new();
    // dashboard will try to open a browser which may fail in CI, but it should
    // still print the URL and exit successfully
    let output = repo
        .git_ai(&["dashboard"])
        .expect("dashboard should succeed even if browser can't open");
    assert!(
        output.contains("dashboard") || output.contains("gitai.co"),
        "should mention dashboard URL, got: {}",
        output
    );
}

#[test]
fn test_upgrade_help() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["upgrade", "--help"])
        .expect("upgrade --help should succeed");
    assert!(
        output.contains("upgrade") || output.contains("Update"),
        "should show upgrade help"
    );
    assert!(output.contains("--check"), "should mention --check option");
}

#[test]
fn test_upgrade_check_only() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["upgrade", "--check"])
        .expect("upgrade --check should succeed");
    assert!(output.contains("git-ai v"), "should show current version");
}
