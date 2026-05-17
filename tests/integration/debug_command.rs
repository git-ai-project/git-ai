use crate::repos::test_repo::TestRepo;

#[test]
fn test_debug_runs_successfully() {
    let repo = TestRepo::new();
    let output = repo
        .git_ai(&["debug"])
        .expect("debug command should succeed");
    assert!(
        output.contains("git-ai debug report"),
        "output should contain report header"
    );
}

#[test]
fn test_debug_contains_version_info() {
    let repo = TestRepo::new();
    let output = repo.git_ai(&["debug"]).expect("debug should succeed");
    assert!(
        output.contains("Git AI version:"),
        "should show git-ai version"
    );
    assert!(
        output.contains("Git version:"),
        "should show git version"
    );
}

#[test]
fn test_debug_contains_platform_info() {
    let repo = TestRepo::new();
    let output = repo.git_ai(&["debug"]).expect("debug should succeed");
    assert!(output.contains("Platform"), "should contain Platform section");
    assert!(output.contains("OS:"), "should show OS info");
    assert!(output.contains("Arch:"), "should show architecture");
}

#[test]
fn test_debug_contains_auth_section() {
    let repo = TestRepo::new();
    let output = repo.git_ai(&["debug"]).expect("debug should succeed");
    assert!(
        output.contains("Auth"),
        "should contain Auth section"
    );
}

#[test]
fn test_debug_contains_repo_section() {
    let repo = TestRepo::new();
    // Make an initial commit so the repo has a HEAD
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let output = repo.git_ai(&["debug"]).expect("debug should succeed");
    assert!(
        output.contains("Repository"),
        "should contain Repository section"
    );
}

#[test]
fn test_debug_contains_hooks_section() {
    let repo = TestRepo::new();
    let output = repo.git_ai(&["debug"]).expect("debug should succeed");
    assert!(output.contains("Hook"), "should contain Hooks section");
}

#[test]
fn test_debug_help_flag() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["debug", "--help"]);
    // --help prints to stderr and exits 0
    assert!(result.is_ok(), "debug --help should succeed");
}
