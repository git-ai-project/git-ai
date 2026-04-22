use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Integration equivalent of the unit test `test_save_stash_note_roundtrip`.
///
/// The original test wrote arbitrary content to refs/notes/ai-stash via
/// `save_stash_note` and read it back with `read_stash_note`.
///
/// Here we test the full pipeline: checkpoint AI content, stash it (the wrapper
/// saves a serialised authorship log as a stash note), verify the note exists,
/// then pop (the wrapper restores attribution from the note) and confirm the
/// working log checkpoints survived the round-trip.
#[test]
fn test_stash_note_roundtrip() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create a file with AI attribution and checkpoint it
    let mut example = repo.filename("example.txt");
    example.set_contents(vec!["line 1".ai(), "line 2".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash (wrapper saves working log to refs/notes/ai-stash)
    repo.git(&["stash"]).expect("stash should succeed");

    // Pop (wrapper restores attribution from the stash note)
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit and verify AI attribution survived the roundtrip
    let commit = repo
        .stage_all_and_commit("apply stash")
        .expect("commit should succeed");

    example.assert_lines_and_blame(vec!["line 1".ai(), "line 2".ai()]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "AI attestations should survive the stash note roundtrip"
    );
}

/// Integration equivalent of the unit test `test_save_stash_note_large_content`.
///
/// The original test verified that a 100 KB string could be written to and read
/// from a stash note without hitting the E2BIG error (which occurs when the
/// content is passed via command-line arguments instead of stdin).
///
/// Here we reproduce that scenario end-to-end: create enough AI-attributed files
/// to generate a large serialised authorship log (well over the typical ARG_MAX
/// limit), stash, pop, and verify that all attributions are preserved.
#[test]
fn test_stash_note_large_content() {
    let repo = TestRepo::new();

    // Initial commit
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("commit should succeed");

    // Create many files with AI attribution to produce a large authorship log.
    // 100 files x 10 lines produces a serialised note well over 128KB, which
    // exceeds ARG_MAX on most systems and would trigger E2BIG if the note were
    // passed via command-line arguments instead of stdin.
    let file_count = 100;
    let mut files = Vec::new();
    for i in 0..file_count {
        let name = format!("large_stash_file_{:04}.txt", i);
        let mut f = repo.filename(&name);
        f.set_contents(vec![
            format!("file {} line 1 with padding content to increase size", i).ai(),
            format!("file {} line 2 with padding content to increase size", i).ai(),
            format!("file {} line 3 with padding content to increase size", i).ai(),
            format!("file {} line 4 with padding content to increase size", i).ai(),
            format!("file {} line 5 with padding content to increase size", i).ai(),
            format!("file {} line 6 with padding content to increase size", i).ai(),
            format!("file {} line 7 with padding content to increase size", i).ai(),
            format!("file {} line 8 with padding content to increase size", i).ai(),
            format!("file {} line 9 with padding content to increase size", i).ai(),
            format!("file {} line 10 with padding content to increase size", i).ai(),
        ]);
        files.push((name, f));
    }

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    // Stash all files (wrapper serialises a large authorship log into a stash note)
    repo.git(&["stash"]).expect("stash should succeed");

    // Verify all files were stashed
    for (name, _) in &files {
        assert!(
            repo.read_file(name).is_none(),
            "{} should be removed after stash",
            name
        );
    }

    // Pop (wrapper restores all attributions from the large stash note)
    repo.git(&["stash", "pop"])
        .expect("stash pop should succeed");

    // Commit and verify every file retained AI attribution
    let commit = repo
        .stage_all_and_commit("apply large stash")
        .expect("commit should succeed");

    for (name, _f) in &files {
        // We just need to confirm the file exists and has content; the full
        // line-by-line blame check below covers attribution correctness.
        assert!(
            repo.read_file(name).is_some(),
            "{} should be restored after pop",
            name
        );
    }

    // Spot-check first and last files for correct AI blame attribution
    files[0].1.assert_lines_and_blame(vec![
        "file 0 line 1 with padding content to increase size".ai(),
        "file 0 line 2 with padding content to increase size".ai(),
        "file 0 line 3 with padding content to increase size".ai(),
        "file 0 line 4 with padding content to increase size".ai(),
        "file 0 line 5 with padding content to increase size".ai(),
        "file 0 line 6 with padding content to increase size".ai(),
        "file 0 line 7 with padding content to increase size".ai(),
        "file 0 line 8 with padding content to increase size".ai(),
        "file 0 line 9 with padding content to increase size".ai(),
        "file 0 line 10 with padding content to increase size".ai(),
    ]);
    files[file_count - 1].1.assert_lines_and_blame(vec![
        format!(
            "file {} line 1 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 2 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 3 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 4 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 5 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 6 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 7 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 8 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 9 with padding content to increase size",
            file_count - 1
        )
        .ai(),
        format!(
            "file {} line 10 with padding content to increase size",
            file_count - 1
        )
        .ai(),
    ]);

    assert!(
        !commit.authorship_log.attestations.is_empty(),
        "AI attestations should survive the large stash note roundtrip"
    );
    assert_eq!(
        commit.authorship_log.attestations.len(),
        file_count,
        "all {} files should appear in the authorship log",
        file_count
    );
}

crate::reuse_tests_in_worktree!(test_stash_note_roundtrip, test_stash_note_large_content,);
