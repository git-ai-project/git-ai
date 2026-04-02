use crate::repos::test_repo::TestRepo;
use std::fs;

fn write_base_files(repo: &TestRepo) {
    fs::write(repo.path().join("lines.md"), "base lines\n").expect("failed to write lines.md");
    fs::write(repo.path().join("alphabet.md"), "base alphabet\n")
        .expect("failed to write alphabet.md");
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
}

#[test]
fn test_explicit_path_checkpoint_only_tracks_the_explicit_file() {
    let repo = TestRepo::new();
    write_base_files(&repo);

    fs::write(
        repo.path().join("lines.md"),
        "line touched by first checkpoint\n",
    )
    .expect("failed to update lines.md");
    repo.git_ai(&["checkpoint", "mock_ai", "lines.md"])
        .expect("first explicit checkpoint should succeed");

    fs::write(
        repo.path().join("alphabet.md"),
        "line touched by second checkpoint\n",
    )
    .expect("failed to update alphabet.md");
    repo.git_ai(&["checkpoint", "mock_ai", "alphabet.md"])
        .expect("second explicit checkpoint should succeed");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    let latest = checkpoints.last().expect("latest checkpoint should exist");
    let latest_files = latest
        .entries
        .iter()
        .map(|entry| entry.file.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        latest_files,
        vec!["alphabet.md"],
        "explicit path checkpoints must not expand to other dirty AI-touched files"
    );
}

#[test]
fn test_explicit_path_checkpoint_skips_conflicted_files() {
    let repo = TestRepo::new();
    let conflict_path = repo.path().join("conflict.txt");
    fs::write(&conflict_path, "base\n").expect("failed to write conflict.txt");
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    let base_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature-branch"])
        .expect("feature branch checkout should succeed");
    fs::write(&conflict_path, "feature\n").expect("failed to write feature content");
    repo.git_og(&["add", "conflict.txt"])
        .expect("feature add should succeed");
    repo.git_og(&["commit", "-m", "feature commit"])
        .expect("feature commit should succeed");

    repo.git_og(&["checkout", &base_branch])
        .expect("return to base branch should succeed");
    fs::write(&conflict_path, "main\n").expect("failed to write main content");
    repo.git_og(&["add", "conflict.txt"])
        .expect("main add should succeed");
    repo.git_og(&["commit", "-m", "main commit"])
        .expect("main commit should succeed");

    let merge_result = repo.git_og(&["merge", "feature-branch"]);
    assert!(merge_result.is_err(), "merge should conflict");
    assert!(
        repo.git_og(&["status", "--short"])
            .expect("status should be readable")
            .contains("UU conflict.txt"),
        "merge should leave conflict.txt unmerged"
    );

    repo.git_ai(&["checkpoint", "mock_ai", "conflict.txt"])
        .expect("explicit conflict checkpoint should succeed without recording entries");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints.is_empty(),
        "explicit-path checkpoints should skip conflicted files entirely"
    );
}

#[test]
fn test_explicit_path_checkpoint_skips_binary_replacements() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("sample.txt");
    fs::write(&file_path, "hello\n").expect("failed to write sample.txt");
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    fs::write(&file_path, vec![0x00, 0x01, 0x02, 0xFF, 0xFE])
        .expect("failed to write binary replacement");

    repo.git_ai(&["checkpoint", "mock_ai", "sample.txt"])
        .expect("explicit binary checkpoint should succeed without recording entries");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints.is_empty(),
        "explicit-path checkpoints should skip files whose current contents are binary"
    );
}
