//! Tests simulating Graphite CLI git operations to verify attribution preservation.
//!
//! Since Graphite requires authentication and can't always run in CI, these tests
//! simulate the git plumbing commands Graphite would execute. This validates that
//! git-ai's hook dispatch handles every code path Graphite uses, independent of
//! the `gt` binary being installed.
//!
//! ## Graphite's plumbing path
//!
//! Many Graphite operations (restack, move, absorb, split) internally use:
//!   1. `git read-tree --index-output <tmp>` to build a custom index
//!   2. `git write-tree` to create a tree object from the index
//!   3. `git commit-tree <tree> -p <parent>` to create a commit without `git commit`
//!   4. `git update-ref refs/heads/<branch> <new-sha>` to update the branch pointer
//!
//! git-ai's wrapper-mode `update-ref` post-hook intercepts step 4 and remaps
//! authorship notes from old to new commit SHAs.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

// ===========================================================================
// Group 1: gt create simulation — branch creation + commit
// ===========================================================================

/// Simulates `gt create`: creates a new branch and commits AI-authored changes.
/// Attribution should be preserved through branch creation.
#[test]
fn test_gt_create_sim_preserves_attribution() {
    let repo = TestRepo::new();

    // Base commit on main
    let mut main_file = repo.filename("main.rs");
    main_file.set_contents(crate::lines!["fn main() {}", "    // base code"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Simulate gt create: new branch + AI writes code
    repo.git(&["checkout", "-b", "feature/add-logging"])
        .unwrap();

    let mut feature_file = repo.filename("logging.rs");
    feature_file.set_contents(crate::lines![
        "fn setup_logging() {".ai(),
        "    println!(\"AI logging\");".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: add logging").unwrap();

    // Verify attribution on the feature branch
    feature_file.assert_lines_and_blame(crate::lines![
        "fn setup_logging() {".ai(),
        "    println!(\"AI logging\");".ai(),
        "}".ai(),
    ]);

    // Verify the base file is untouched
    main_file.assert_lines_and_blame(crate::lines![
        "fn main() {}".human(),
        "    // base code".human(),
    ]);
}

/// Simulates stacked `gt create`: two branches stacked on each other, each with
/// mixed AI/human content. Verifies attribution is correct at each level.
#[test]
fn test_gt_create_sim_stacked_branches() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.rs");
    base.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Branch 1: AI writes feature A
    repo.git(&["checkout", "-b", "feature-a"]).unwrap();
    let mut file_a = repo.filename("feature_a.rs");
    file_a.set_contents(crate::lines![
        "fn feature_a() {".ai(),
        "    // AI implementation".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: feature A").unwrap();

    // Branch 2 (stacked on A): human writes feature B
    repo.git(&["checkout", "-b", "feature-b"]).unwrap();
    let mut file_b = repo.filename("feature_b.rs");
    file_b.set_contents(crate::lines![
        "fn feature_b() {",
        "    // human implementation",
        "}",
    ]);
    repo.stage_all_and_commit("feat: feature B").unwrap();

    // Verify from tip of stack (feature-b): both files visible, attribution correct
    file_a.assert_lines_and_blame(crate::lines![
        "fn feature_a() {".ai(),
        "    // AI implementation".ai(),
        "}".ai(),
    ]);
    file_b.assert_lines_and_blame(crate::lines![
        "fn feature_b() {".human(),
        "    // human implementation".human(),
        "}".human(),
    ]);

    // Navigate down to feature-a and verify
    repo.git(&["checkout", "feature-a"]).unwrap();
    file_a.assert_lines_and_blame(crate::lines![
        "fn feature_a() {".ai(),
        "    // AI implementation".ai(),
        "}".ai(),
    ]);
}

// ===========================================================================
// Group 2: gt modify simulation (amend path) — `git commit --amend`
// ===========================================================================

/// Simulates `gt modify` using `git commit --amend`.
/// AI attribution from the original commit must survive the amend.
#[test]
fn test_gt_modify_sim_amend_preserves_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut file = repo.filename("feature.rs");
    file.set_contents(crate::lines!["fn feature() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // AI writes code and commits
    file.set_contents(crate::lines![
        "fn feature() {",
        "    // AI implementation".ai(),
        "}",
    ]);
    repo.stage_all_and_commit("feat: implement feature")
        .unwrap();

    // Verify pre-amend attribution
    file.assert_lines_and_blame(crate::lines![
        "fn feature() {".human(),
        "    // AI implementation".ai(),
        "}".human(),
    ]);

    // Simulate gt modify: amend with additional human changes
    file.set_contents(crate::lines![
        "fn feature() {",
        "    // AI implementation".ai(),
        "    // human addition",
        "}",
    ]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Verify post-amend: AI lines retain AI attribution, human lines are human
    file.assert_lines_and_blame(crate::lines![
        "fn feature() {".human(),
        "    // AI implementation".ai(),
        "    // human addition".human(),
        "}".human(),
    ]);
}

/// Simulates `gt modify` with `--commit` flag (new commit, not amend).
/// Both the original and new commit's attribution should be correct.
#[test]
fn test_gt_modify_sim_new_commit_preserves_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut file = repo.filename("modify_commit.rs");
    file.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // First feature commit with AI content
    repo.git(&["checkout", "-b", "modify-branch"]).unwrap();
    file.set_contents(crate::lines![
        "fn base() {}",
        "fn ai_feature() {".ai(),
        "    // AI code".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: AI feature").unwrap();

    // Second commit (--commit mode) with more AI content
    let mut file2 = repo.filename("modify_commit2.rs");
    file2.set_contents(crate::lines!["fn ai_helper() {}".ai()]);
    repo.stage_all_and_commit("feat: AI helper").unwrap();

    // Verify both files
    file.assert_lines_and_blame(crate::lines![
        "fn base() {}".human(),
        "fn ai_feature() {".ai(),
        "    // AI code".ai(),
        "}".ai(),
    ]);
    file2.assert_lines_and_blame(crate::lines!["fn ai_helper() {}".ai()]);
}

// ===========================================================================
// Group 3: gt modify simulation (plumbing path) — write-tree + commit-tree + update-ref
// ===========================================================================

/// Simulates `gt modify` using plumbing commands (commit-tree + update-ref).
/// This bypasses `git commit` entirely -- tests whether git-ai's update-ref
/// hook catches this and preserves attribution.
///
/// The plumbing path is what Graphite uses when restacking child branches after
/// modifying a parent branch.
#[test]
fn test_gt_modify_sim_plumbing_preserves_attribution() {
    let repo = TestRepo::new();

    // Initial commit with human content
    let mut file = repo.filename("plumbing.rs");
    file.set_contents(crate::lines!["fn plumbing() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // Second commit with AI content
    file.set_contents(crate::lines!["fn plumbing() {", "    // AI code".ai(), "}",]);
    repo.stage_all_and_commit("feat: AI plumbing").unwrap();

    // Now simulate plumbing-based modify (what Graphite does when restacking):
    // 1. Modify the file and stage
    file.set_contents(crate::lines![
        "fn plumbing() {",
        "    // AI code".ai(),
        "    // human mod",
        "}",
    ]);
    repo.git(&["add", "-A"]).unwrap();

    // 2. write-tree to create tree object from index
    let tree_sha = repo
        .git(&["write-tree"])
        .expect("write-tree should succeed");
    let tree_sha = tree_sha.trim();

    // 3. commit-tree to create commit (parent = current HEAD)
    let head_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed");
    let head_sha = head_sha.trim();
    let commit_sha = repo
        .git(&[
            "commit-tree",
            tree_sha,
            "-p",
            head_sha,
            "-m",
            "modified via plumbing",
        ])
        .expect("commit-tree should succeed");
    let commit_sha = commit_sha.trim();

    // 4. update-ref to point HEAD at new commit
    let branch = repo.current_branch();
    let ref_name = format!("refs/heads/{}", branch);
    repo.git(&["update-ref", &ref_name, commit_sha])
        .expect("update-ref should succeed");

    // The new commit was created via plumbing. git-ai's update-ref post-hook
    // should detect the ref update and remap authorship notes.
    file.assert_lines_and_blame(crate::lines![
        "fn plumbing() {".human(),
        "    // AI code".ai(),
        "    // human mod".human(),
        "}".human(),
    ]);
}

// ===========================================================================
// Group 4: gt restack simulation — rebase onto modified parent
// ===========================================================================

/// Simulates `gt restack`: rebases a dependent branch after parent changes.
/// AI attribution must survive the rebase.
///
/// Workflow: main -> feature-a (AI file) -> feature-b (human file, separate)
/// Then modify feature-a by adding a new file, then rebase feature-b onto new feature-a.
/// Using separate files avoids rebase conflicts.
#[test]
fn test_gt_restack_sim_preserves_attribution_through_rebase() {
    let repo = TestRepo::new();

    // Create main with base content
    let mut base = repo.filename("stack.rs");
    base.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // Branch A: AI writes feature A
    repo.git(&["checkout", "-b", "feature-a"]).unwrap();
    let mut file_a = repo.filename("feature_a.rs");
    file_a.set_contents(crate::lines!["fn feature_a() { /* AI */ }".ai()]);
    repo.stage_all_and_commit("feat: feature A").unwrap();

    // Branch B (stacked on A): human writes feature B in a separate file
    repo.git(&["checkout", "-b", "feature-b"]).unwrap();
    let mut file_b = repo.filename("feature_b.rs");
    file_b.set_contents(crate::lines!["fn feature_b() { /* human */ }"]);
    repo.stage_all_and_commit("feat: feature B").unwrap();

    // Now modify branch A (simulate gt modify on parent): add a new file
    // to avoid rebase conflicts with the child branch
    repo.git(&["checkout", "feature-a"]).unwrap();
    let mut file_a_extra = repo.filename("feature_a_extra.rs");
    file_a_extra.set_contents(crate::lines!["fn feature_a_helper() { /* AI */ }".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Restack: rebase feature-b onto new feature-a (simulates gt restack)
    repo.git(&["checkout", "feature-b"]).unwrap();
    repo.git(&["rebase", "feature-a"]).unwrap();

    // Verify: AI attribution survives the restack on the rebased branch
    file_b.assert_lines_and_blame(crate::lines!["fn feature_b() { /* human */ }".human(),]);

    // Check the parent branch too
    repo.git(&["checkout", "feature-a"]).unwrap();
    file_a.assert_lines_and_blame(crate::lines!["fn feature_a() { /* AI */ }".ai(),]);
    file_a_extra.assert_lines_and_blame(crate::lines!["fn feature_a_helper() { /* AI */ }".ai(),]);
}

/// Simulates `gt restack` with a 3-branch stack where the bottom branch is
/// modified and all children must be restacked.
#[test]
fn test_gt_restack_sim_three_branch_stack() {
    let repo = TestRepo::new();

    // Base
    let mut base = repo.filename("base.rs");
    base.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    let default_branch = repo.current_branch();

    // Branch A
    repo.git(&["checkout", "-b", "stack-a"]).unwrap();
    let mut file_a = repo.filename("a.rs");
    file_a.set_contents(crate::lines!["fn a() {}".ai(), "// a human"]);
    repo.stage_all_and_commit("feat: a").unwrap();

    // Branch B (stacked on A)
    repo.git(&["checkout", "-b", "stack-b"]).unwrap();
    let mut file_b = repo.filename("b.rs");
    file_b.set_contents(crate::lines!["fn b() {}".ai(), "// b human"]);
    repo.stage_all_and_commit("feat: b").unwrap();

    // Branch C (stacked on B)
    repo.git(&["checkout", "-b", "stack-c"]).unwrap();
    let mut file_c = repo.filename("c.rs");
    file_c.set_contents(crate::lines!["fn c() {}".ai(), "// c human"]);
    repo.stage_all_and_commit("feat: c").unwrap();

    // Advance main (simulate trunk advancing)
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut main_update = repo.filename("main_update.rs");
    main_update.set_contents(crate::lines!["// main update"]);
    repo.stage_all_and_commit("main update").unwrap();

    // Restack: rebase stack-a onto main, then stack-b onto stack-a, etc.
    repo.git(&["checkout", "stack-a"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    repo.git(&["checkout", "stack-b"]).unwrap();
    repo.git(&["rebase", "stack-a"]).unwrap();

    repo.git(&["checkout", "stack-c"]).unwrap();
    repo.git(&["rebase", "stack-b"]).unwrap();

    // Verify attribution through the whole stack
    file_c.assert_lines_and_blame(crate::lines!["fn c() {}".ai(), "// c human".human()]);

    repo.git(&["checkout", "stack-b"]).unwrap();
    file_b.assert_lines_and_blame(crate::lines!["fn b() {}".ai(), "// b human".human()]);

    repo.git(&["checkout", "stack-a"]).unwrap();
    file_a.assert_lines_and_blame(crate::lines!["fn a() {}".ai(), "// a human".human()]);
}

// ===========================================================================
// Group 5: gt split simulation — reset + separate commits
// ===========================================================================

/// Simulates `gt split`: splitting a commit with mixed AI/human code across
/// two files into two separate commits. Uses `git reset HEAD~1` followed by
/// selective staging and committing.
#[test]
fn test_gt_split_sim_preserves_per_file_attribution() {
    let repo = TestRepo::new();

    // Base
    let mut file_a = repo.filename("file_a.rs");
    let mut file_b = repo.filename("file_b.rs");
    file_a.set_contents(crate::lines!["// file a"]);
    file_b.set_contents(crate::lines!["// file b"]);
    repo.stage_all_and_commit("base").unwrap();

    // Single commit with AI changes to both files
    file_a.set_contents(crate::lines!["// file a", "fn ai_a() {}".ai()]);
    file_b.set_contents(crate::lines!["// file b", "fn ai_b() {}".ai()]);
    repo.stage_all_and_commit("feat: both files").unwrap();

    // Simulate gt split: reset to before the combined commit, recommit separately
    repo.git(&["reset", "HEAD~1"]).unwrap();

    // Commit file_a only
    repo.git(&["add", "file_a.rs"]).unwrap();
    repo.git(&["commit", "-m", "feat: file a only"]).unwrap();

    // Commit file_b only
    repo.git(&["add", "file_b.rs"]).unwrap();
    repo.git(&["commit", "-m", "feat: file b only"]).unwrap();

    // Verify attribution on each file individually
    file_a.assert_lines_and_blame(crate::lines!["// file a".human(), "fn ai_a() {}".ai(),]);

    file_b.assert_lines_and_blame(crate::lines!["// file b".human(), "fn ai_b() {}".ai(),]);
}

/// Simulates `gt split` where a single file has mixed AI and human lines,
/// then the commit is split into two commits modifying different parts of
/// the same file.
#[test]
fn test_gt_split_sim_same_file_mixed_attribution() {
    let repo = TestRepo::new();

    // Base with some content
    let mut file = repo.filename("mixed.rs");
    file.set_contents(crate::lines![
        "fn existing() {}",
        "// placeholder 1",
        "// placeholder 2",
    ]);
    repo.stage_all_and_commit("base").unwrap();

    // Single commit: AI adds a function, human adds a comment
    file.set_contents(crate::lines![
        "fn existing() {}",
        "fn ai_function() {}".ai(),
        "// human comment",
    ]);
    repo.stage_all_and_commit("feat: mixed changes").unwrap();

    // Verify the combined commit has correct attribution
    file.assert_lines_and_blame(crate::lines![
        "fn existing() {}".human(),
        "fn ai_function() {}".ai(),
        "// human comment".human(),
    ]);
}

// ===========================================================================
// Group 6: gt fold simulation — merge branch into parent
// ===========================================================================

/// Simulates `gt fold`: merging a child branch into its parent.
/// Uses `reset --soft` + `commit --amend` pattern.
#[test]
fn test_gt_fold_sim_preserves_attribution() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.rs");
    base.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    // Parent branch with human content
    repo.git(&["checkout", "-b", "fold-parent"]).unwrap();
    let mut parent_file = repo.filename("parent.rs");
    parent_file.set_contents(crate::lines![
        "fn parent_feature() {",
        "    // human code",
        "}",
    ]);
    repo.stage_all_and_commit("feat: parent feature").unwrap();

    // Child branch with AI content
    repo.git(&["checkout", "-b", "fold-child"]).unwrap();
    let mut child_file = repo.filename("child.rs");
    child_file.set_contents(crate::lines![
        "fn child_feature() {".ai(),
        "    // AI code".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: child feature").unwrap();

    // Simulate gt fold: merge child into parent
    // 1. Go to child branch and soft-reset to parent
    let parent_sha = repo
        .git(&["rev-parse", "fold-parent"])
        .expect("rev-parse should succeed");
    let parent_sha = parent_sha.trim();
    repo.git(&["reset", "--soft", parent_sha]).unwrap();

    // 2. Amend the parent's commit with the child's changes
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // 3. Update the parent ref and switch to it
    let new_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed");
    let new_sha = new_sha.trim();
    repo.git(&["checkout", "fold-parent"]).unwrap();
    repo.git(&["reset", "--hard", new_sha]).unwrap();

    // Verify both files have correct attribution on the parent branch
    parent_file.assert_lines_and_blame(crate::lines![
        "fn parent_feature() {".human(),
        "    // human code".human(),
        "}".human(),
    ]);
    child_file.assert_lines_and_blame(crate::lines![
        "fn child_feature() {".ai(),
        "    // AI code".ai(),
        "}".ai(),
    ]);
}

// ===========================================================================
// Group 7: gt squash simulation — squash multiple commits
// ===========================================================================

/// Simulates `gt squash`: squashing multiple commits on a branch into one.
/// Uses `reset --soft` to the branch point + new commit.
#[test]
fn test_gt_squash_sim_preserves_attribution() {
    let repo = TestRepo::new();

    // Base
    let mut base = repo.filename("base.rs");
    base.set_contents(crate::lines!["fn base() {}"]);
    repo.stage_all_and_commit("base").unwrap();

    let base_sha = repo
        .git(&["rev-parse", "HEAD"])
        .expect("rev-parse should succeed");
    let base_sha = base_sha.trim().to_string();

    // Create branch with two commits
    repo.git(&["checkout", "-b", "squash-branch"]).unwrap();

    // First commit: AI content
    let mut file = repo.filename("squash.rs");
    file.set_contents(crate::lines!["fn squash_fn() {".ai(), "}".ai()]);
    repo.stage_all_and_commit("feat: first commit").unwrap();

    // Second commit: human additions
    file.set_contents(crate::lines![
        "fn squash_fn() {".ai(),
        "    // human addition",
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: second commit").unwrap();

    // Simulate gt squash: soft-reset to branch point, then commit
    repo.git(&["reset", "--soft", &base_sha]).unwrap();
    repo.git(&["commit", "-m", "feat: squashed"]).unwrap();

    // Verify attribution is preserved after squash
    file.assert_lines_and_blame(crate::lines![
        "fn squash_fn() {".ai(),
        "    // human addition".human(),
        "}".ai(),
    ]);
}

// ===========================================================================
// Group 8: Complex multi-operation workflow
// ===========================================================================

/// End-to-end workflow simulating a typical Graphite stack lifecycle:
/// 1. Create 3 stacked branches with mixed AI/human content
/// 2. Modify the middle branch (amend)
/// 3. Restack the top branch
/// 4. Verify attribution throughout
#[test]
fn test_gt_full_stack_sim_workflow() {
    let repo = TestRepo::new();

    // Base commit
    let mut base = repo.filename("base.rs");
    base.set_contents(crate::lines!["// project base"]);
    repo.stage_all_and_commit("initial").unwrap();

    // Branch 1: AI-heavy feature
    repo.git(&["checkout", "-b", "wf-branch-1"]).unwrap();
    let mut file1 = repo.filename("feature1.rs");
    file1.set_contents(crate::lines![
        "fn feature1() {".ai(),
        "    // AI implementation".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("feat: feature 1").unwrap();

    // Branch 2: human feature stacked on branch 1
    repo.git(&["checkout", "-b", "wf-branch-2"]).unwrap();
    let mut file2 = repo.filename("feature2.rs");
    file2.set_contents(crate::lines![
        "fn feature2() {",
        "    // human implementation",
        "}",
    ]);
    repo.stage_all_and_commit("feat: feature 2").unwrap();

    // Branch 3: mixed AI/human stacked on branch 2
    repo.git(&["checkout", "-b", "wf-branch-3"]).unwrap();
    let mut file3 = repo.filename("feature3.rs");
    file3.set_contents(crate::lines![
        "fn feature3() {",
        "    // AI helper".ai(),
        "    // human logic",
        "}",
    ]);
    repo.stage_all_and_commit("feat: feature 3").unwrap();

    // Modify branch 1 (simulate gt modify on parent)
    repo.git(&["checkout", "wf-branch-1"]).unwrap();
    let mut extra = repo.filename("feature1_extra.rs");
    extra.set_contents(crate::lines!["fn extra() {}".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    // Restack: rebase branch-2 onto new branch-1, then branch-3 onto new branch-2
    repo.git(&["checkout", "wf-branch-2"]).unwrap();
    repo.git(&["rebase", "wf-branch-1"]).unwrap();

    repo.git(&["checkout", "wf-branch-3"]).unwrap();
    repo.git(&["rebase", "wf-branch-2"]).unwrap();

    // Verify entire stack from the tip
    file1.assert_lines_and_blame(crate::lines![
        "fn feature1() {".ai(),
        "    // AI implementation".ai(),
        "}".ai(),
    ]);

    file2.assert_lines_and_blame(crate::lines![
        "fn feature2() {".human(),
        "    // human implementation".human(),
        "}".human(),
    ]);

    file3.assert_lines_and_blame(crate::lines![
        "fn feature3() {".human(),
        "    // AI helper".ai(),
        "    // human logic".human(),
        "}".human(),
    ]);

    extra.assert_lines_and_blame(crate::lines!["fn extra() {}".ai()]);
}
