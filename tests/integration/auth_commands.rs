use crate::repos::test_repo::TestRepo;

#[test]
fn test_whoami_not_logged_in() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["whoami"]);
    // whoami exits with code 1 when not logged in
    assert!(result.is_err(), "whoami should fail when not logged in");
    let output = result.unwrap_err();
    assert!(
        output.contains("logged out") || output.contains("Credential backend"),
        "should indicate not logged in state, got: {}",
        output
    );
}

#[test]
fn test_logout_when_not_logged_in() {
    let repo = TestRepo::new();
    // logout should succeed even when not logged in (it just prints a message)
    let output = repo
        .git_ai(&["logout"])
        .expect("logout should succeed even when not logged in");
    assert!(
        output.contains("Not currently logged in"),
        "should indicate not currently logged in, got: {}",
        output
    );
}

#[test]
fn test_whoami_help() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["whoami", "--help"])
        .expect("whoami --help should succeed");
    assert!(
        output.contains("whoami") || output.contains("auth"),
        "help should mention whoami"
    );
}
