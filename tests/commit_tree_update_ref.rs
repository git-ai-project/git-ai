#[macro_use]
mod repos;

// Graphite-style restacks rewrite commits with `git commit-tree` + `git update-ref`.
// These tests model that plumbing path directly so they do not depend on `gt`.

use git_ai::git::find_repository_in_path;
use git_ai::git::refs::show_authorship_note;
use git_ai::git::repository::Repository as GitAiRepository;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::{GitTestMode, TestRepo};

fn setup_initial_commit(repo: &TestRepo) {
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Test Repo"]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
}

fn open_repo(repo: &TestRepo) -> GitAiRepository {
    find_repository_in_path(repo.path().to_str().unwrap())
        .expect("failed to open git-ai repository")
}

fn head_sha(repo: &TestRepo) -> String {
    repo.git(&["rev-parse", "HEAD"])
        .expect("rev-parse HEAD should succeed")
        .trim()
        .to_string()
}

fn commit_tree_rewrite_current_branch(
    repo: &TestRepo,
    branch: &str,
    new_parent: &str,
    message: &str,
) -> (String, String) {
    let old_head = head_sha(repo);
    let tree = repo
        .git(&["rev-parse", &format!("{}^{{tree}}", old_head)])
        .expect("rev-parse HEAD^{tree} should succeed")
        .trim()
        .to_string();

    let new_head = repo
        .git(&["commit-tree", &tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree should succeed")
        .trim()
        .to_string();

    repo.git(&[
        "update-ref",
        &format!("refs/heads/{}", branch),
        &new_head,
        &old_head,
    ])
    .expect("git update-ref should succeed");

    (old_head, new_head)
}

fn commit_tree_from_existing_tree(
    repo: &TestRepo,
    treeish: &str,
    new_parent: &str,
    message: &str,
) -> String {
    let tree = repo
        .git(&["rev-parse", &format!("{}^{{tree}}", treeish)])
        .expect("rev-parse tree should succeed")
        .trim()
        .to_string();

    repo.git(&["commit-tree", &tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree should succeed")
        .trim()
        .to_string()
}

fn graphite_style_restack_child_branch(
    repo: &TestRepo,
    branch: &str,
    old_head: &str,
    new_parent: &str,
    message: &str,
) -> String {
    let old_parent = repo
        .git(&["rev-parse", &format!("{}^", old_head)])
        .expect("rev-parse old parent should succeed")
        .trim()
        .to_string();
    let old_grandparent = repo
        .git(&["rev-parse", &format!("{}^", old_parent)])
        .expect("rev-parse old grandparent should succeed")
        .trim()
        .to_string();

    let synthetic_parent = commit_tree_from_existing_tree(repo, new_parent, &old_grandparent, "_");
    let merged_tree = repo
        .git(&[
            "merge-tree",
            "--allow-unrelated-histories",
            &synthetic_parent,
            old_head,
        ])
        .expect("git merge-tree should succeed")
        .trim()
        .to_string();

    let new_head = repo
        .git(&["commit-tree", &merged_tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree for rewritten child should succeed")
        .trim()
        .to_string();

    repo.git(&[
        "update-ref",
        &format!("refs/heads/{}", branch),
        &new_head,
        old_head,
    ])
    .expect("git update-ref should succeed");

    new_head
}

fn should_skip_for_hooks_mode() -> bool {
    let mode = std::env::var("GIT_AI_TEST_GIT_MODE").unwrap_or_else(|_| "wrapper".to_string());
    if matches!(GitTestMode::from_mode_name(&mode), GitTestMode::Hooks) {
        eprintln!("SKIP: commit-tree/update-ref regression only runs in wrapper mode");
        return true;
    }
    false
}

#[test]
fn test_commit_tree_update_ref_preserves_authorship_notes_on_reparent() {
    if should_skip_for_hooks_mode() {
        return;
    }

    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "ai line".ai()]);
    let feature_commit = repo
        .stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &feature_commit.commit_sha).is_some(),
        "expected initial feature commit to have an authorship note",
    );

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    let (old_head, new_head) = commit_tree_rewrite_current_branch(
        &repo,
        "feature",
        &main_commit.commit_sha,
        "feature commit",
    );

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_head).is_some(),
        "expected rewritten commit {} to preserve authorship note from {}",
        new_head,
        old_head,
    );

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines!["human line".human(), "ai line".ai()]);
}

#[test]
fn test_commit_tree_update_ref_moves_working_log_to_rewritten_head() {
    if should_skip_for_hooks_mode() {
        return;
    }

    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "committed ai".ai()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    feature_file.set_contents_no_stage(lines![
        "human line",
        "committed ai".ai(),
        "pending ai".ai(),
    ]);

    let old_head = head_sha(&repo);
    let git_ai_repo = open_repo(&repo);
    assert!(
        git_ai_repo.storage.has_working_log(&old_head),
        "expected dirty branch to have a working log before rewrite",
    );

    let (_, new_head) = commit_tree_rewrite_current_branch(
        &repo,
        "feature",
        &main_commit.commit_sha,
        "feature commit",
    );

    let git_ai_repo = open_repo(&repo);
    assert!(
        git_ai_repo.storage.has_working_log(&new_head),
        "expected working log to follow rewritten HEAD from {} to {}",
        old_head,
        new_head,
    );
    assert!(
        !git_ai_repo.storage.has_working_log(&old_head),
        "expected working log for old HEAD {} to be renamed away",
        old_head,
    );

    repo.git(&["add", "-A"]).expect("git add should succeed");
    repo.commit("commit after plumbing rewrite")
        .expect("commit after plumbing rewrite should succeed");

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines![
        "human line".human(),
        "committed ai".ai(),
        "pending ai".ai(),
    ]);
}

#[test]
fn test_reset_keep_rewrite_preserves_authorship_notes_on_current_branch() {
    if should_skip_for_hooks_mode() {
        return;
    }

    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "ai line".ai()]);
    let feature_commit = repo
        .stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &feature_commit.commit_sha).is_some(),
        "expected initial feature commit to have an authorship note",
    );

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    let old_head = head_sha(&repo);
    let new_head =
        commit_tree_from_existing_tree(&repo, &old_head, &main_commit.commit_sha, "feature commit");

    repo.git(&["reset", "--keep", &new_head])
        .expect("git reset --keep should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_head).is_some(),
        "expected rewritten current-branch commit {} to preserve authorship note from {}",
        new_head,
        old_head,
    );

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines!["human line".human(), "ai line".ai()]);
}

#[test]
fn test_update_ref_restack_after_parent_amend_preserves_child_attribution() {
    if should_skip_for_hooks_mode() {
        return;
    }

    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "parent"])
        .expect("checkout parent should succeed");
    let mut parent_file = repo.filename("parent.txt");
    parent_file.set_contents(lines!["parent ai".ai(), "parent human"]);
    let parent_commit = repo
        .stage_all_and_commit("parent")
        .expect("parent commit should succeed");

    repo.git(&["checkout", "-b", "child"])
        .expect("checkout child should succeed");
    let mut child_file = repo.filename("child.txt");
    child_file.set_contents(lines!["child ai".ai(), "child human"]);
    let child_commit = repo
        .stage_all_and_commit("child")
        .expect("child commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &child_commit.commit_sha).is_some(),
        "expected initial child commit to have an authorship note",
    );

    repo.git(&["checkout", "parent"])
        .expect("checkout parent should succeed");
    let mut parent_file2 = repo.filename("parent2.txt");
    parent_file2.set_contents(lines!["parent2 ai".ai()]);
    repo.git(&["add", "-A"]).expect("git add should succeed");
    repo.git(&["commit", "--amend", "-m", "modified parent"])
        .expect("git commit --amend should succeed");

    let amended_parent_head = head_sha(&repo);
    assert_ne!(
        amended_parent_head, parent_commit.commit_sha,
        "expected parent amend to rewrite the parent branch"
    );

    let new_child_head = graphite_style_restack_child_branch(
        &repo,
        "child",
        &child_commit.commit_sha,
        &amended_parent_head,
        "child",
    );

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_child_head).is_some(),
        "expected rewritten child commit {} to preserve authorship note from {}",
        new_child_head,
        child_commit.commit_sha,
    );

    repo.git(&["checkout", "child"])
        .expect("checkout child should succeed");
    let mut rewritten_child_file = repo.filename("child.txt");
    rewritten_child_file.assert_lines_and_blame(lines!["child ai".ai(), "child human".human()]);
}
