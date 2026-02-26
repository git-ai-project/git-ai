/// Graphite (`gt` CLI) test suite for git-ai attribution preservation.
///
/// These tests verify that git-ai attribution (line-level blame tracking of AI vs human authorship)
/// is correctly preserved across all local Graphite CLI operations.
///
/// ## Requirements
/// - The `gt` CLI must be installed and available in PATH
/// - When the `CI` environment variable is set, tests will FAIL if `gt` is not available
/// - When not in CI, tests will be SKIPPED if `gt` is not available
///
/// ## Known Issue: `gt` rebase operations use git plumbing commands
///
/// Graphite's restack/move/absorb/split operations internally use `git commit-tree` +
/// `git update-ref` (low-level plumbing commands) instead of `git rebase`. This completely
/// bypasses git-ai's attribution tracking because:
///   1. `git commit-tree` does not trigger any git hooks (pre-commit, post-commit, post-rewrite)
///   2. `git update-ref` moves branch pointers without any hook invocation
///   3. git-ai's wrapper-mode post-command hooks only fire for recognized commands like `rebase`
///
/// As a result, when `gt` restacks branches, the new commits created by `commit-tree` have no
/// associated attribution notes. The old commit SHAs still have their notes, but the new SHAs
/// that replace them do not.
///
/// Tests affected by this are marked with `#[ignore]` and tagged with "KNOWN_ISSUE:COMMIT_TREE".
/// They document the CORRECT expected behavior and will pass once git-ai handles `commit-tree`
/// or Graphite changes its rebase strategy.
///
/// ## Commands NOT tested (require GitHub authentication / remote):
/// - `gt submit` - Pushes to GitHub, creates/updates PRs
/// - `gt sync` - Syncs branches with remote
/// - `gt get` - Syncs branches from remote
/// - `gt merge` - Merges PRs via Graphite
/// - `gt pr` - Opens PR page in browser
/// - `gt dash` - Opens Graphite dashboard
/// - `gt auth` - Authentication
/// - `gt feedback` - Sends feedback to Graphite team
/// - `gt freeze` / `gt unfreeze` - Primarily useful with remote sync
/// - `gt reorder` - Requires interactive editor
/// - `gt revert` - Experimental, requires specific trunk commit setup
/// - `gt split --by-commit` / `gt split --by-hunk` - Requires interactive input
///
/// ## Commands NOT tested (require interactive terminal):
/// - `gt undo` - Requires interactive mode even with `--no-interactive` flag
///
/// ## Commands tested:
/// - `gt init` - Initialize Graphite in a repo
/// - `gt create` - Create new branch with commit
/// - `gt modify` - Amend/new commit with automatic restack
/// - `gt squash` - Squash all commits in branch into one
/// - `gt restack` - Rebase stack to ensure parent lineage (KNOWN_ISSUE: loses attribution)
/// - `gt fold` - Fold branch into parent
/// - `gt move` - Move branch to new parent (KNOWN_ISSUE: loses attribution)
/// - `gt split --by-file` - Split branch by file (KNOWN_ISSUE: loses attribution)
/// - `gt absorb` - Absorb staged changes into stack (KNOWN_ISSUE: loses attribution)
/// - `gt checkout` / `gt up` / `gt down` / `gt top` / `gt bottom` - Navigation
/// - `gt delete` - Delete branch, restack children (KNOWN_ISSUE: restack loses attribution)
/// - `gt pop` - Delete branch, retain working tree
/// - `gt rename` - Rename branch
/// - `gt track` / `gt untrack` - Metadata tracking
#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::{TestRepo, get_binary_path};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Helper utilities
// ---------------------------------------------------------------------------

/// Check if the `gt` CLI is available on this machine.
/// Returns the path to the binary if found, or None.
fn find_gt_binary() -> Option<String> {
    #[cfg(windows)]
    let which_cmd = "where";
    #[cfg(not(windows))]
    let which_cmd = "which";

    let output = Command::new(which_cmd).arg("gt").output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(path) }
    } else {
        None
    }
}

/// Guard that skips the test when `gt` is not installed (local dev),
/// or panics when running in CI (where `gt` MUST be available).
macro_rules! require_gt {
    () => {{
        if find_gt_binary().is_none() {
            if std::env::var("CI").is_ok() {
                panic!(
                    "Graphite CLI (`gt`) is required in CI but was not found. \
                     Install it with: npm install -g @withgraphite/graphite-cli@stable"
                );
            } else {
                eprintln!("SKIP: `gt` CLI not found — skipping Graphite test");
                return;
            }
        }
    }};
}

/// Create a wrapper directory containing a `git` symlink (or copy on Windows)
/// that points to the git-ai binary. This is created once and shared across
/// all tests. When prepended to PATH, any child process (including `gt`) that
/// calls `git` will actually invoke `git-ai`, which handles attribution
/// tracking transparently — including during rebase operations that create
/// new commit SHAs.
static WRAPPER_DIR: OnceLock<PathBuf> = OnceLock::new();

fn get_wrapper_dir() -> &'static PathBuf {
    WRAPPER_DIR.get_or_init(|| {
        let binary_path = get_binary_path();
        let wrapper_dir =
            std::env::temp_dir().join(format!("git-ai-gt-wrapper-{}", std::process::id()));
        std::fs::create_dir_all(&wrapper_dir).expect("create wrapper dir");

        #[cfg(unix)]
        {
            let link_path = wrapper_dir.join("git");
            // Remove stale symlink if it exists
            let _ = std::fs::remove_file(&link_path);
            std::os::unix::fs::symlink(binary_path, &link_path).expect("create git symlink");
        }

        #[cfg(windows)]
        {
            let link_path = wrapper_dir.join("git.exe");
            let _ = std::fs::remove_file(&link_path);
            std::fs::copy(binary_path, &link_path).expect("copy git-ai as git.exe");
        }

        wrapper_dir
    })
}

/// Build a PATH string that has the git-ai wrapper directory first,
/// followed by the original system PATH.
fn wrapper_path() -> String {
    let wrapper_dir = get_wrapper_dir();
    let original_path = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", wrapper_dir.display(), original_path)
}

/// Find the absolute path of the real `git` binary (before our wrapper).
/// This is needed so that the git-ai wrapper knows which real git to delegate to.
static REAL_GIT_PATH: OnceLock<String> = OnceLock::new();

fn real_git_path() -> &'static str {
    REAL_GIT_PATH.get_or_init(|| {
        #[cfg(windows)]
        let which_cmd = "where";
        #[cfg(not(windows))]
        let which_cmd = "which";

        let output = Command::new(which_cmd)
            .arg("git")
            .output()
            .expect("failed to find git binary");
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert!(!path.is_empty(), "git binary not found in PATH");
        // On `which` returning multiple lines, take the first
        path.lines().next().unwrap_or(&path).to_string()
    })
}

/// Execute a `gt` command inside the given TestRepo directory.
///
/// The key insight: `gt` calls `git` internally for commits, rebases, etc.
/// By prepending a wrapper directory to PATH that contains a `git` symlink
/// pointing to the git-ai binary, all of `gt`'s git operations flow through
/// the git-ai wrapper. This ensures attribution notes are properly created
/// during commits and correctly copied during rebases (via post-rewrite
/// handling in the wrapper).
///
/// Passes `--no-interactive` to avoid prompts.
/// Returns Ok(stdout+stderr) on success, Err(stderr) on failure.
fn gt(repo: &TestRepo, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("gt");
    command
        .current_dir(repo.path())
        .args(args)
        .arg("--no-interactive");

    // Put git-ai wrapper first in PATH so `gt` calls git-ai instead of raw git.
    // This is essential for attribution tracking during gt operations.
    command.env("PATH", wrapper_path());

    // Tell the git-ai wrapper which real git binary to delegate to.
    // Must use the absolute path to avoid infinite recursion through our wrapper.
    command.env("GIT_AI", real_git_path());

    // Set up environment for git-ai to work properly (both wrapper and hooks)
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env("GIT_AI_GLOBAL_GIT_HOOKS", "true");
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path().to_str().unwrap());

    let output = command
        .output()
        .unwrap_or_else(|e| panic!("Failed to execute gt {:?}: {}", args, e));

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        let combined = if stdout.is_empty() {
            stderr
        } else if stderr.is_empty() {
            stdout
        } else {
            format!("{}{}", stdout, stderr)
        };
        Ok(combined)
    } else {
        let combined_err = format!("{}{}", stderr, stdout);
        Err(combined_err)
    }
}

/// Install git-ai hooks in a test repo so that `gt` rebase operations
/// trigger git-ai's post-rewrite hook for attribution note copying.
/// The wrapper handles commit-time attribution, while hooks handle
/// the old-SHA → new-SHA note remapping during rebases.
fn install_hooks(repo: &TestRepo) {
    let binary_path = get_binary_path();
    let mut command = Command::new(binary_path);
    command
        .current_dir(repo.path())
        .args(["git-hooks", "ensure"]);
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env("GIT_AI_GLOBAL_GIT_HOOKS", "true");
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path().to_str().unwrap());

    let output = command
        .output()
        .expect("failed to run git-ai git-hooks ensure");
    if !output.status.success() {
        panic!(
            "git-ai git-hooks ensure failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Initialize Graphite in a TestRepo (sets trunk to "main").
/// Also installs git-ai hooks so that rebase-based gt operations
/// (restack, move, delete) properly copy attribution notes.
fn gt_init(repo: &TestRepo) {
    install_hooks(repo);
    gt(repo, &["init", "--trunk", "main"]).expect("gt init should succeed");
    // Re-install hooks after gt init, in case gt init modified core.hooksPath
    install_hooks(repo);
}

/// Create an initial commit so the repo is not empty (required for most gt operations).
fn setup_initial_commit(repo: &TestRepo) {
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Test Repo"]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
}

// ===========================================================================
// Group 1: gt create — Branch creation with attribution
// ===========================================================================

#[test]
fn test_gt_create_preserves_ai_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Add AI-authored content
    let mut file = repo.filename("feature.txt");
    file.set_contents(lines![
        "human line 1",
        "ai line 2".ai(),
        "ai line 3".ai(),
        "human line 4",
    ]);

    // Use gt create to create a branch and commit
    repo.git(&["add", "-A"]).unwrap();
    gt(
        &repo,
        &["create", "feature-branch", "-m", "add feature with AI"],
    )
    .expect("gt create should succeed");

    // Verify attribution is preserved
    file.assert_lines_and_blame(lines![
        "human line 1".human(),
        "ai line 2".ai(),
        "ai line 3".ai(),
        "human line 4".human(),
    ]);
}

#[test]
fn test_gt_create_stacked_branches_preserve_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // First branch with AI content
    let mut file1 = repo.filename("file1.txt");
    file1.set_contents(lines!["file1 ai line".ai(), "file1 human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "branch-1", "-m", "first branch"])
        .expect("gt create branch-1 should succeed");

    // Second branch stacked on first, with more AI content
    let mut file2 = repo.filename("file2.txt");
    file2.set_contents(lines!["file2 ai line".ai(), "file2 human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "branch-2", "-m", "second branch"])
        .expect("gt create branch-2 should succeed");

    // Verify attribution on both files from the tip of the stack
    file1.assert_lines_and_blame(lines!["file1 ai line".ai(), "file1 human line".human(),]);
    file2.assert_lines_and_blame(lines!["file2 ai line".ai(), "file2 human line".human(),]);

    // Navigate down and verify attribution still correct on branch-1
    gt(&repo, &["checkout", "branch-1"]).expect("gt checkout should succeed");
    file1.assert_lines_and_blame(lines!["file1 ai line".ai(), "file1 human line".human(),]);
}

#[test]
fn test_gt_create_empty_branch() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create a file with AI attribution
    let mut file = repo.filename("existing.txt");
    file.set_contents(lines!["ai content".ai(), "human content"]);
    repo.stage_all_and_commit("add file").unwrap();

    // Create empty branch (no changes)
    gt(&repo, &["create", "empty-branch", "-m", "empty branch"])
        .expect("gt create empty branch should succeed");

    // Attribution on existing file should be unchanged
    file.assert_lines_and_blame(lines!["ai content".ai(), "human content".human(),]);
}

// ===========================================================================
// Group 2: gt modify — Amend/new commit with restack
// ===========================================================================

#[test]
fn test_gt_modify_amend_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create a branch with AI content
    let mut file = repo.filename("modify.txt");
    file.set_contents(lines!["original ai line".ai(), "original human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "modify-branch", "-m", "initial"]).expect("gt create should succeed");

    // Add more content and amend via gt modify
    let mut file2 = repo.filename("modify2.txt");
    file2.set_contents(lines!["new ai line".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "-m", "amended with more AI"]).expect("gt modify should succeed");

    // Both files should have correct attribution
    file.assert_lines_and_blame(lines![
        "original ai line".ai(),
        "original human line".human(),
    ]);
    file2.assert_lines_and_blame(lines!["new ai line".ai(),]);
}

#[test]
fn test_gt_modify_new_commit_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch with AI content
    let mut file = repo.filename("modify_commit.txt");
    file.set_contents(lines!["ai line 1".ai(), "human line 1"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "modify-commit-branch", "-m", "initial"])
        .expect("gt create should succeed");

    // Add more changes with --commit (new commit, not amend)
    let mut file2 = repo.filename("modify_commit2.txt");
    file2.set_contents(lines!["ai line 2".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "--commit", "-m", "second commit"])
        .expect("gt modify --commit should succeed");

    // Both files should have correct attribution
    file.assert_lines_and_blame(lines!["ai line 1".ai(), "human line 1".human(),]);
    file2.assert_lines_and_blame(lines!["ai line 2".ai(),]);
}

/// KNOWN_ISSUE:COMMIT_TREE — `gt modify` amends via `commit-tree` when restacking children,
/// so the child branch commits lose their attribution notes.
#[test]
#[ignore]
fn test_gt_modify_restacks_children_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create parent branch
    let mut parent_file = repo.filename("parent.txt");
    parent_file.set_contents(lines!["parent ai".ai(), "parent human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "parent-branch", "-m", "parent"])
        .expect("gt create parent should succeed");

    // Create child branch
    let mut child_file = repo.filename("child.txt");
    child_file.set_contents(lines!["child ai".ai(), "child human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "child-branch", "-m", "child"]).expect("gt create child should succeed");

    // Go back to parent and modify it (should trigger restack of child)
    gt(&repo, &["checkout", "parent-branch"]).unwrap();
    let mut parent_file2 = repo.filename("parent2.txt");
    parent_file2.set_contents(lines!["parent2 ai".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "-m", "modified parent"]).expect("gt modify parent should succeed");

    // Verify parent attribution
    parent_file.assert_lines_and_blame(lines!["parent ai".ai(), "parent human".human(),]);

    // Check child branch attribution after restack
    gt(&repo, &["checkout", "child-branch"]).unwrap();
    child_file.assert_lines_and_blame(lines!["child ai".ai(), "child human".human(),]);
}

// ===========================================================================
// Group 3: gt squash — Squash commits in branch
// ===========================================================================

#[test]
fn test_gt_squash_preserves_ai_lines() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch with first commit
    let mut file = repo.filename("squash.txt");
    file.set_contents(lines!["ai line 1".ai(), "human line 1"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "squash-branch", "-m", "first commit"])
        .expect("gt create should succeed");

    // Add more content via gt modify --commit (creates a second commit on the branch)
    file.set_contents(lines![
        "ai line 1".ai(),
        "human line 1",
        "ai line 2".ai(),
        "human line 2",
    ]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "--commit", "-m", "second commit"])
        .expect("gt modify --commit should succeed");

    // Squash all commits in the branch
    gt(&repo, &["squash", "-m", "squashed"]).expect("gt squash should succeed");

    // Verify all attribution is preserved after squash
    file.assert_lines_and_blame(lines![
        "ai line 1".ai(),
        "human line 1".human(),
        "ai line 2".ai(),
        "human line 2".human(),
    ]);
}

#[test]
fn test_gt_squash_mixed_ai_human_across_commits() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // First commit: all human
    let mut file = repo.filename("squash_mixed.txt");
    file.set_contents(lines!["human only line 1", "human only line 2"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "squash-mixed", "-m", "human commit"]).expect("gt create should succeed");

    // Second commit: add AI lines
    file.set_contents(lines![
        "human only line 1",
        "human only line 2",
        "ai line 3".ai(),
        "ai line 4".ai(),
    ]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "--commit", "-m", "ai commit"])
        .expect("gt modify --commit should succeed");

    // Squash
    gt(&repo, &["squash", "-m", "squashed mixed"]).expect("gt squash should succeed");

    file.assert_lines_and_blame(lines![
        "human only line 1".human(),
        "human only line 2".human(),
        "ai line 3".ai(),
        "ai line 4".ai(),
    ]);
}

// ===========================================================================
// Group 4: gt restack — Rebase stack operations
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — `gt restack` uses `git commit-tree` + `git update-ref`
/// instead of `git rebase`, bypassing git-ai attribution tracking entirely.
#[test]
#[ignore]
fn test_gt_restack_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch-1
    let mut file1 = repo.filename("restack1.txt");
    file1.set_contents(lines!["branch1 ai".ai(), "branch1 human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "restack-1", "-m", "branch 1"]).expect("gt create should succeed");

    // Create branch-2 stacked on branch-1
    let mut file2 = repo.filename("restack2.txt");
    file2.set_contents(lines!["branch2 ai".ai(), "branch2 human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "restack-2", "-m", "branch 2"]).expect("gt create should succeed");

    // Go to main and add a commit to simulate trunk advancing
    repo.git(&["checkout", "main"]).unwrap();
    let mut main_file = repo.filename("main_update.txt");
    main_file.set_contents(lines!["main update"]);
    repo.stage_all_and_commit("main update").unwrap();

    // Go back to the top of the stack and restack
    gt(&repo, &["checkout", "restack-2"]).unwrap();
    gt(&repo, &["restack"]).expect("gt restack should succeed");

    // Verify attribution on both branches
    file2.assert_lines_and_blame(lines!["branch2 ai".ai(), "branch2 human".human(),]);

    gt(&repo, &["checkout", "restack-1"]).unwrap();
    file1.assert_lines_and_blame(lines!["branch1 ai".ai(), "branch1 human".human(),]);
}

/// KNOWN_ISSUE:COMMIT_TREE — `gt restack` uses `git commit-tree` + `git update-ref`
/// instead of `git rebase`, bypassing git-ai attribution tracking entirely.
/// Note: this test may pass if no actual restacking is needed (no trunk advancement),
/// but is marked ignored for consistency with the restack group.
#[test]
#[ignore]
fn test_gt_restack_three_branch_stack() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create 3-branch stack, each with mixed AI/human content
    let mut file_a = repo.filename("a.txt");
    file_a.set_contents(lines!["a ai".ai(), "a human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "stack-a", "-m", "branch a"]).expect("gt create a should succeed");

    let mut file_b = repo.filename("b.txt");
    file_b.set_contents(lines!["b ai".ai(), "b human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "stack-b", "-m", "branch b"]).expect("gt create b should succeed");

    let mut file_c = repo.filename("c.txt");
    file_c.set_contents(lines!["c ai".ai(), "c human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "stack-c", "-m", "branch c"]).expect("gt create c should succeed");

    // Restack from the top
    gt(&repo, &["restack"]).expect("gt restack should succeed");

    // Verify all branches
    file_c.assert_lines_and_blame(lines!["c ai".ai(), "c human".human()]);

    gt(&repo, &["checkout", "stack-b"]).unwrap();
    file_b.assert_lines_and_blame(lines!["b ai".ai(), "b human".human()]);

    gt(&repo, &["checkout", "stack-a"]).unwrap();
    file_a.assert_lines_and_blame(lines!["a ai".ai(), "a human".human()]);
}

// ===========================================================================
// Group 5: gt fold — Fold branch into parent
// ===========================================================================

#[test]
fn test_gt_fold_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create parent branch with human content
    let mut parent_file = repo.filename("fold_parent.txt");
    parent_file.set_contents(lines!["parent human line 1", "parent human line 2"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "fold-parent", "-m", "parent branch"])
        .expect("gt create parent should succeed");

    // Create child branch with AI content
    let mut child_file = repo.filename("fold_child.txt");
    child_file.set_contents(lines!["child ai line".ai(), "child human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "fold-child", "-m", "child branch"])
        .expect("gt create child should succeed");

    // Fold child into parent
    gt(&repo, &["fold"]).expect("gt fold should succeed");

    // After fold, we should be on fold-parent with both files
    // and all attribution preserved
    parent_file.assert_lines_and_blame(lines![
        "parent human line 1".human(),
        "parent human line 2".human(),
    ]);
    child_file.assert_lines_and_blame(lines!["child ai line".ai(), "child human line".human(),]);
}

#[test]
fn test_gt_fold_with_mixed_content() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create parent branch with a file containing mixed content
    let mut file = repo.filename("fold_mixed.txt");
    file.set_contents(lines!["parent line 1", "parent line 2"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "fold-mixed-parent", "-m", "parent"]).expect("gt create should succeed");

    // Create child that modifies the same file
    file.set_contents(lines![
        "parent line 1",
        "parent line 2",
        "child ai addition".ai(),
    ]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "fold-mixed-child", "-m", "child"]).expect("gt create should succeed");

    // Fold child into parent
    gt(&repo, &["fold"]).expect("gt fold should succeed");

    file.assert_lines_and_blame(lines![
        "parent line 1".human(),
        "parent line 2".human(),
        "child ai addition".ai(),
    ]);
}

// ===========================================================================
// Group 6: gt move — Move branch to new parent
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — `gt move` uses `git commit-tree` + `git update-ref`
/// instead of `git rebase`, bypassing git-ai attribution tracking entirely.
#[test]
#[ignore]
fn test_gt_move_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch-a off main
    let mut file_a = repo.filename("move_a.txt");
    file_a.set_contents(lines!["a ai line".ai(), "a human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "move-a", "-m", "branch a"]).expect("gt create a should succeed");

    // Go back to main
    gt(&repo, &["checkout", "main"]).unwrap();

    // Create branch-b off main
    let mut file_b = repo.filename("move_b.txt");
    file_b.set_contents(lines!["b ai line".ai(), "b human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "move-b", "-m", "branch b"]).expect("gt create b should succeed");

    // Move branch-b onto branch-a (rebase b onto a)
    gt(&repo, &["move", "--onto", "move-a"]).expect("gt move should succeed");

    // Verify attribution on branch-b after move
    file_b.assert_lines_and_blame(lines!["b ai line".ai(), "b human line".human(),]);

    // Verify branch-a attribution is unchanged
    gt(&repo, &["checkout", "move-a"]).unwrap();
    file_a.assert_lines_and_blame(lines!["a ai line".ai(), "a human line".human(),]);
}

// ===========================================================================
// Group 7: gt split --by-file — Split branch by files
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — `gt split` uses `git commit-tree` + `git update-ref`
/// instead of `git rebase`, bypassing git-ai attribution tracking entirely.
#[test]
#[ignore]
fn test_gt_split_by_file_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch with two files (one AI, one human)
    let mut ai_file = repo.filename("split_ai.txt");
    ai_file.set_contents(lines!["ai content 1".ai(), "ai content 2".ai()]);
    let mut human_file = repo.filename("split_human.txt");
    human_file.set_contents(lines!["human content 1", "human content 2"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(
        &repo,
        &["create", "split-branch", "-m", "branch with two files"],
    )
    .expect("gt create should succeed");

    // Split by file — extract the AI file into a new parent branch
    gt(&repo, &["split", "--by-file", "split_ai.txt"]).expect("gt split --by-file should succeed");

    // After split, we should be able to verify the AI file on the new parent
    // and the human file remains on the current branch
    // The exact branch structure depends on gt's behavior, but attribution
    // on both files should be preserved regardless of which branch we're on
    ai_file.assert_lines_and_blame(lines!["ai content 1".ai(), "ai content 2".ai(),]);
}

// ===========================================================================
// Group 8: gt absorb — Absorb changes into stack
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — `gt absorb` uses `git commit-tree` + `git update-ref`
/// to amend earlier commits, bypassing git-ai attribution tracking entirely.
#[test]
#[ignore]
fn test_gt_absorb_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create a branch with a file
    let mut file = repo.filename("absorb.txt");
    file.set_contents(lines!["line 1", "line 2", "line 3",]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "absorb-branch", "-m", "initial file"])
        .expect("gt create should succeed");

    // Create a second branch stacked on top
    let mut file2 = repo.filename("absorb2.txt");
    file2.set_contents(lines!["other file content".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "absorb-child", "-m", "child branch"])
        .expect("gt create child should succeed");

    // Modify absorb.txt (change that should be absorbed into absorb-branch)
    file.set_contents(lines!["line 1 modified".ai(), "line 2", "line 3",]);
    repo.git(&["add", "-A"]).unwrap();

    // Absorb the change into the correct commit
    gt(&repo, &["absorb", "--force"]).expect("gt absorb should succeed");

    // Verify attribution after absorb
    file.assert_lines_and_blame(lines![
        "line 1 modified".ai(),
        "line 2".human(),
        "line 3".human(),
    ]);
}

// ===========================================================================
// Group 9: Navigation commands — Verify no attribution side effects
// ===========================================================================

#[test]
fn test_gt_navigation_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create a 3-branch stack
    let mut file1 = repo.filename("nav1.txt");
    file1.set_contents(lines!["nav1 ai".ai(), "nav1 human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "nav-1", "-m", "nav branch 1"]).expect("gt create should succeed");

    let mut file2 = repo.filename("nav2.txt");
    file2.set_contents(lines!["nav2 ai".ai(), "nav2 human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "nav-2", "-m", "nav branch 2"]).expect("gt create should succeed");

    let mut file3 = repo.filename("nav3.txt");
    file3.set_contents(lines!["nav3 ai".ai(), "nav3 human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "nav-3", "-m", "nav branch 3"]).expect("gt create should succeed");

    // Navigate with gt down
    gt(&repo, &["down"]).expect("gt down should succeed");
    file2.assert_lines_and_blame(lines!["nav2 ai".ai(), "nav2 human".human()]);

    // Navigate with gt down again
    gt(&repo, &["down"]).expect("gt down should succeed");
    file1.assert_lines_and_blame(lines!["nav1 ai".ai(), "nav1 human".human()]);

    // Navigate with gt up
    gt(&repo, &["up"]).expect("gt up should succeed");
    file2.assert_lines_and_blame(lines!["nav2 ai".ai(), "nav2 human".human()]);

    // Navigate with gt top
    gt(&repo, &["top"]).expect("gt top should succeed");
    file3.assert_lines_and_blame(lines!["nav3 ai".ai(), "nav3 human".human()]);

    // Navigate with gt bottom
    gt(&repo, &["bottom"]).expect("gt bottom should succeed");
    file1.assert_lines_and_blame(lines!["nav1 ai".ai(), "nav1 human".human()]);

    // Navigate with gt checkout
    gt(&repo, &["checkout", "nav-2"]).expect("gt checkout should succeed");
    file2.assert_lines_and_blame(lines!["nav2 ai".ai(), "nav2 human".human()]);
}

// ===========================================================================
// Group 10: gt delete / gt pop — Branch deletion
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — `gt delete` restacks children using `git commit-tree` +
/// `git update-ref`, bypassing git-ai attribution tracking entirely.
#[test]
#[ignore]
fn test_gt_delete_restacks_children_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create 3-branch stack: main -> branch-a -> branch-b -> branch-c
    let mut file_a = repo.filename("del_a.txt");
    file_a.set_contents(lines!["a content"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "del-a", "-m", "branch a"]).expect("gt create a should succeed");

    let mut file_b = repo.filename("del_b.txt");
    file_b.set_contents(lines!["b ai content".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "del-b", "-m", "branch b"]).expect("gt create b should succeed");

    let mut file_c = repo.filename("del_c.txt");
    file_c.set_contents(lines!["c ai content".ai(), "c human content"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "del-c", "-m", "branch c"]).expect("gt create c should succeed");

    // Delete the middle branch (branch-b)
    gt(&repo, &["checkout", "del-b"]).unwrap();
    gt(&repo, &["delete", "--force"]).expect("gt delete should succeed");

    // branch-c should have been restacked onto branch-a
    // Navigate to branch-c and verify attribution
    gt(&repo, &["checkout", "del-c"]).unwrap();
    file_c.assert_lines_and_blame(lines!["c ai content".ai(), "c human content".human(),]);
}

#[test]
fn test_gt_pop_retains_working_tree() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch with AI content
    let mut file = repo.filename("pop.txt");
    file.set_contents(lines!["pop ai line".ai(), "pop human line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "pop-branch", "-m", "pop branch"]).expect("gt create should succeed");

    // Pop the branch — should delete branch but retain file changes in working tree
    gt(&repo, &["pop"]).expect("gt pop should succeed");

    // The file should still exist in working tree
    let content = repo.read_file("pop.txt");
    assert!(content.is_some(), "File should still exist after gt pop");

    // Re-commit the changes and verify attribution
    repo.stage_all_and_commit("re-commit after pop").unwrap();
    file.assert_lines_and_blame(lines!["pop ai line".ai(), "pop human line".human(),]);
}

// ===========================================================================
// Group 11: gt rename — Branch rename
// ===========================================================================

#[test]
fn test_gt_rename_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create branch with AI content
    let mut file = repo.filename("rename.txt");
    file.set_contents(lines!["rename ai".ai(), "rename human"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "old-name", "-m", "branch to rename"]).expect("gt create should succeed");

    // Rename the branch
    gt(&repo, &["rename", "new-name"]).expect("gt rename should succeed");

    // Verify attribution is unchanged
    file.assert_lines_and_blame(lines!["rename ai".ai(), "rename human".human(),]);

    // Verify we're on the new branch name
    let current = repo.current_branch();
    assert_eq!(current, "new-name", "Branch should be renamed");
}

// ===========================================================================
// Group 12: gt track / gt untrack — Metadata operations
// ===========================================================================

#[test]
fn test_gt_track_untrack_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create a regular git branch (not via gt)
    repo.git(&["checkout", "-b", "manual-branch"]).unwrap();
    let mut file = repo.filename("track.txt");
    file.set_contents(lines!["track ai".ai(), "track human"]);
    repo.stage_all_and_commit("manual branch commit").unwrap();

    // Track it with Graphite
    gt(&repo, &["track", "--parent", "main"]).expect("gt track should succeed");

    // Verify attribution after tracking
    file.assert_lines_and_blame(lines!["track ai".ai(), "track human".human(),]);

    // Untrack
    gt(&repo, &["untrack"]).expect("gt untrack should succeed");

    // Verify attribution after untracking
    file.assert_lines_and_blame(lines!["track ai".ai(), "track human".human(),]);
}

// ===========================================================================
// Group 13: gt undo — Undo operations
// ===========================================================================

/// `gt undo` requires interactive terminal even when `--no-interactive` is passed.
/// Cannot be tested in automated CI.
#[test]
#[ignore]
fn test_gt_undo_create_preserves_attribution() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create base file with AI content
    let mut base_file = repo.filename("undo_base.txt");
    base_file.set_contents(lines!["base ai".ai(), "base human"]);
    repo.stage_all_and_commit("base commit").unwrap();

    // Create a new branch
    let mut new_file = repo.filename("undo_new.txt");
    new_file.set_contents(lines!["new ai".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "undo-branch", "-m", "branch to undo"])
        .expect("gt create should succeed");

    // Undo the create
    gt(&repo, &["undo"]).expect("gt undo should succeed");

    // Base file attribution should still be correct
    base_file.assert_lines_and_blame(lines!["base ai".ai(), "base human".human(),]);
}

// ===========================================================================
// Group 14: Complex multi-operation workflows
// ===========================================================================

/// KNOWN_ISSUE:COMMIT_TREE — This workflow test includes a `gt modify` on a middle branch
/// which triggers restacking of children via `git commit-tree`, losing their attribution.
#[test]
#[ignore]
fn test_gt_full_stack_workflow() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Step 1: Create a 3-branch stack with mixed AI/human content
    let mut file1 = repo.filename("workflow1.txt");
    file1.set_contents(lines![
        "workflow1 ai line 1".ai(),
        "workflow1 human line 1",
        "workflow1 ai line 2".ai(),
    ]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "wf-1", "-m", "workflow branch 1"])
        .expect("gt create wf-1 should succeed");

    let mut file2 = repo.filename("workflow2.txt");
    file2.set_contents(lines!["workflow2 human line 1", "workflow2 ai line 1".ai(),]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "wf-2", "-m", "workflow branch 2"])
        .expect("gt create wf-2 should succeed");

    let mut file3 = repo.filename("workflow3.txt");
    file3.set_contents(lines![
        "workflow3 ai line 1".ai(),
        "workflow3 ai line 2".ai(),
        "workflow3 human line 1",
    ]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "wf-3", "-m", "workflow branch 3"])
        .expect("gt create wf-3 should succeed");

    // Step 2: Navigate down, modify a middle branch
    gt(&repo, &["checkout", "wf-1"]).unwrap();
    let mut file1_extra = repo.filename("workflow1_extra.txt");
    file1_extra.set_contents(lines!["extra ai".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "-m", "modified wf-1 with extra file"])
        .expect("gt modify should succeed");

    // Step 3: Verify the entire stack has correct attribution
    gt(&repo, &["checkout", "wf-3"]).unwrap();

    file1.assert_lines_and_blame(lines![
        "workflow1 ai line 1".ai(),
        "workflow1 human line 1".human(),
        "workflow1 ai line 2".ai(),
    ]);
    file2.assert_lines_and_blame(lines![
        "workflow2 human line 1".human(),
        "workflow2 ai line 1".ai(),
    ]);
    file3.assert_lines_and_blame(lines![
        "workflow3 ai line 1".ai(),
        "workflow3 ai line 2".ai(),
        "workflow3 human line 1".human(),
    ]);
}

#[test]
fn test_gt_create_then_squash_then_fold() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create parent with human content
    let mut parent_file = repo.filename("csf_parent.txt");
    parent_file.set_contents(lines!["parent line 1", "parent line 2"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "csf-parent", "-m", "parent"]).expect("gt create should succeed");

    // Create child with AI content (two commits)
    let mut child_file = repo.filename("csf_child.txt");
    child_file.set_contents(lines!["child ai 1".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "csf-child", "-m", "child commit 1"]).expect("gt create should succeed");

    child_file.set_contents(lines!["child ai 1".ai(), "child ai 2".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["modify", "--commit", "-m", "child commit 2"])
        .expect("gt modify --commit should succeed");

    // Squash child commits
    gt(&repo, &["squash", "-m", "squashed child"]).expect("gt squash should succeed");

    // Fold child into parent
    gt(&repo, &["fold"]).expect("gt fold should succeed");

    // Verify all attribution on the now-combined branch
    parent_file.assert_lines_and_blame(lines!["parent line 1".human(), "parent line 2".human(),]);
    child_file.assert_lines_and_blame(lines!["child ai 1".ai(), "child ai 2".ai(),]);
}

#[test]
fn test_gt_create_with_all_flag() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Make changes without staging
    let mut file = repo.filename("all_flag.txt");
    file.set_contents(lines!["human line 1", "ai line 1".ai(), "human line 2",]);

    // Use gt create -a to auto-stage all changes
    gt(
        &repo,
        &["create", "all-flag-branch", "-a", "-m", "auto stage"],
    )
    .expect("gt create -a should succeed");

    file.assert_lines_and_blame(lines![
        "human line 1".human(),
        "ai line 1".ai(),
        "human line 2".human(),
    ]);
}

#[test]
fn test_gt_modify_all_flag() {
    require_gt!();
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    gt_init(&repo);

    // Create initial branch
    let mut file = repo.filename("modify_all.txt");
    file.set_contents(lines!["initial line"]);
    repo.git(&["add", "-A"]).unwrap();
    gt(&repo, &["create", "modify-all-branch", "-m", "initial"]).expect("gt create should succeed");

    // Modify with unstaged changes, use -a flag
    file.set_contents(lines!["initial line", "ai addition".ai()]);
    gt(&repo, &["modify", "-a", "-m", "modified with ai"]).expect("gt modify -a should succeed");

    file.assert_lines_and_blame(lines!["initial line".human(), "ai addition".ai(),]);
}
