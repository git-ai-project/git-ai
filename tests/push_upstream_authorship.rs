#[macro_use]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::process::Command;

fn read_remote_authorship_note(repo: &TestRepo, commit_sha: &str) -> Option<String> {
    let git_dir = repo.path().to_str().expect("valid repo path");
    let output = Command::new("git")
        .args([
            "--git-dir",
            git_dir,
            "--no-pager",
            "notes",
            "--ref=ai",
            "show",
            commit_sha,
        ])
        .output()
        .expect("failed to run git notes show on remote");

    if output.status.success() {
        let note = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if note.is_empty() { None } else { Some(note) }
    } else {
        None
    }
}

#[test]
fn push_with_set_upstream_flag_pushes_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("upstream_feature.rs");
    file.set_contents(vec!["fn upstream_feature() {}".ai()]);
    let commit = local
        .stage_all_and_commit("add upstream feature")
        .expect("commit should succeed");

    local
        .git(&["push", "-u", "origin", "HEAD"])
        .expect("push with -u should succeed");

    let note = read_remote_authorship_note(&upstream, &commit.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed to the remote when using -u"
    );
}

#[test]
fn push_after_branch_set_upstream_pushes_authorship_notes() {
    let (local, upstream) = TestRepo::new_with_remote();

    let mut file = local.filename("upstream_branch.rs");
    file.set_contents(vec!["fn initial() {}".ai()]);
    local
        .stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    local
        .git(&["push", "origin", "HEAD"])
        .expect("initial push should succeed");

    let branch = local.current_branch();
    local
        .git(&["branch", "-u", &format!("origin/{}", branch)])
        .expect("branch -u should succeed");

    file.set_contents(vec!["fn initial() {}".ai(), "fn follow_up() {}".ai()]);
    let follow_up = local
        .stage_all_and_commit("follow-up commit")
        .expect("follow-up commit should succeed");

    local
        .git(&["push"])
        .expect("push with configured upstream should succeed");

    let note = read_remote_authorship_note(&upstream, &follow_up.commit_sha);
    assert!(
        note.is_some(),
        "expected authorship notes to be pushed after setting upstream with git branch -u"
    );
}

reuse_tests_in_worktree!(
    push_with_set_upstream_flag_pushes_authorship_notes,
    push_after_branch_set_upstream_pushes_authorship_notes,
);
