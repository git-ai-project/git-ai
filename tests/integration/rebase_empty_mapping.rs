use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Regression for #1476.
///
/// A rebase whose only replayed commit is already present upstream produces a
/// complete reflog segment, but no commit mapping. If that no-op segment is left
/// unprocessed, a later real rebase to the same target can select the stale
/// segment and skip authorship note rewriting for the new commit.
#[test]
fn test_noop_rebase_segment_does_not_steal_later_rebase_mapping() {
    let repo = TestRepo::new();

    let shared_path = repo.path().join("shared.txt");
    fs::write(&shared_path, "base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "shared.txt"])
        .unwrap();
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    let mut shared = repo.filename("shared.txt");
    shared.assert_committed_lines(crate::lines!["base".human()]);

    repo.git(&["checkout", "-b", "upstream-noop"]).unwrap();
    fs::write(&shared_path, "base\nequivalent\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "shared.txt"])
        .unwrap();
    repo.stage_all_and_commit("upstream equivalent patch")
        .unwrap();
    shared.assert_committed_lines(crate::lines!["base".human(), "equivalent".human()]);

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "feature-noop"]).unwrap();
    fs::write(&shared_path, "base\nequivalent\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "shared.txt"])
        .unwrap();
    repo.stage_all_and_commit("feature equivalent patch")
        .unwrap();
    shared.assert_committed_lines(crate::lines!["base".human(), "equivalent".human()]);

    repo.git(&["rebase", "upstream-noop"]).unwrap();
    shared.assert_committed_lines(crate::lines!["base".human(), "equivalent".human()]);

    let ai_path = repo.path().join("ai.txt");
    fs::write(&ai_path, "AI line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai.txt"]).unwrap();
    let ai_commit = repo.stage_all_and_commit("add ai work").unwrap();
    let mut ai_file = repo.filename("ai.txt");
    ai_file.assert_committed_lines(crate::lines!["AI line 1".ai()]);
    assert!(
        repo.read_authorship_note(&ai_commit.commit_sha).is_some(),
        "AI commit should have an authorship note before amend/rebase"
    );

    fs::write(&ai_path, "AI line 1\nAI line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai.txt"]).unwrap();
    repo.git(&["add", "ai.txt"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "add amended ai work"])
        .unwrap();
    let amended_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    ai_file.assert_committed_lines(crate::lines!["AI line 1".ai(), "AI line 2".ai()]);
    assert!(
        repo.read_authorship_note(&amended_sha).is_some(),
        "Amended AI commit should have an authorship note before the second rebase"
    );

    repo.git(&["checkout", "upstream-noop"]).unwrap();
    let upstream_path = repo.path().join("upstream.txt");
    fs::write(&upstream_path, "new upstream line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "upstream.txt"])
        .unwrap();
    repo.stage_all_and_commit("advance upstream").unwrap();
    let mut upstream_file = repo.filename("upstream.txt");
    upstream_file.assert_committed_lines(crate::lines!["new upstream line".human()]);

    repo.git(&["checkout", "feature-noop"]).unwrap();
    repo.git(&["rebase", "upstream-noop"]).unwrap();

    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&rebased_sha);
    assert!(
        note.is_some(),
        "Rebased AI commit should keep its authorship note"
    );
    ai_file.assert_committed_lines(crate::lines!["AI line 1".ai(), "AI line 2".ai()]);

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.ai_additions, 2);
    assert_eq!(stats.unknown_additions, 0);
}
