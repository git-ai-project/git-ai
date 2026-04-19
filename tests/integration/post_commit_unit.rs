use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::post_commit::{
    count_line_ranges, should_skip_expensive_post_commit_stats, StatsCostEstimate,
    STATS_SKIP_MAX_ADDED_LINES, STATS_SKIP_MAX_DELETED_LINES,
    STATS_SKIP_MAX_FILES_WITH_ADDITIONS, STATS_SKIP_MAX_HUNKS,
};

#[test]
fn test_count_line_ranges_handles_scattered_and_contiguous_lines() {
    assert_eq!(count_line_ranges(&[]), 0);
    assert_eq!(count_line_ranges(&[1]), 1);
    assert_eq!(count_line_ranges(&[1, 2, 3]), 1);
    assert_eq!(count_line_ranges(&[1, 3, 5]), 3);
    // Includes unsorted and duplicate values.
    assert_eq!(count_line_ranges(&[5, 3, 3, 4, 10]), 2);
}

#[test]
fn test_should_skip_expensive_post_commit_stats_thresholds() {
    let below_threshold = StatsCostEstimate {
        files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS - 1,
        added_lines: STATS_SKIP_MAX_ADDED_LINES - 1,
        hunk_ranges: STATS_SKIP_MAX_HUNKS - 1,
        deleted_lines: STATS_SKIP_MAX_DELETED_LINES - 1,
    };
    assert!(!should_skip_expensive_post_commit_stats(&below_threshold));

    let by_hunks = StatsCostEstimate {
        files_with_additions: 1,
        added_lines: 1,
        hunk_ranges: STATS_SKIP_MAX_HUNKS,
        deleted_lines: 0,
    };
    assert!(should_skip_expensive_post_commit_stats(&by_hunks));

    let by_added_lines = StatsCostEstimate {
        files_with_additions: 1,
        added_lines: STATS_SKIP_MAX_ADDED_LINES,
        hunk_ranges: 1,
        deleted_lines: 0,
    };
    assert!(should_skip_expensive_post_commit_stats(&by_added_lines));

    let by_files = StatsCostEstimate {
        files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
        added_lines: 1,
        hunk_ranges: 1,
        deleted_lines: 0,
    };
    assert!(should_skip_expensive_post_commit_stats(&by_files));

    let by_deleted_lines = StatsCostEstimate {
        files_with_additions: 0,
        added_lines: 0,
        hunk_ranges: 0,
        deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
    };
    assert!(should_skip_expensive_post_commit_stats(&by_deleted_lines));
}

#[test]
fn test_post_commit_empty_repo_with_checkpoint() {
    // Create an empty repo (no commits yet)
    let repo = TestRepo::new();

    // Write file without staging
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Append to file
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\nSecond line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    // Stage and commit - this triggers the post-commit hook
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // The key assertion: post_commit didn't panic. We can verify by checking authorship note exists
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // If post_commit ran successfully via the git hook, an authorship note should exist
    let note_result = repo.read_authorship_note(&head_sha);

    // It should succeed (the note was created during commit)
    assert!(
        note_result.is_some(),
        "post_commit should handle empty repo without errors"
    );
}

#[test]
fn test_post_commit_empty_repo_no_checkpoint() {
    // Create an empty repo (no commits yet)
    let repo = TestRepo::new();

    // Create a file without checkpointing
    std::fs::write(repo.path().join("test.txt"), "Hello, world!\n").unwrap();

    // Stage and commit without prior checkpoint - this triggers the post-commit hook
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Should not panic or error even with no working log
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // With no checkpoints, authorship log should have empty attestations
    let note = repo.read_authorship_note(&head_sha);
    assert!(note.is_some(), "Should have authorship note");

    // No checkpoints = no AI attribution, so note should have empty attestations
    let log = AuthorshipLog::deserialize_from_string(&note.unwrap()).unwrap();
    assert!(
        log.attestations.is_empty(),
        "Should have empty attestations when no checkpoints exist"
    );
}

#[test]
fn test_count_line_ranges_single_element() {
    assert_eq!(count_line_ranges(&[42]), 1);
}

#[test]
fn test_count_line_ranges_all_contiguous() {
    assert_eq!(count_line_ranges(&[1, 2, 3, 4, 5]), 1);
}

#[test]
fn test_count_line_ranges_all_scattered() {
    assert_eq!(count_line_ranges(&[1, 10, 20, 30]), 4);
}

#[test]
fn test_count_line_ranges_duplicates() {
    assert_eq!(count_line_ranges(&[5, 5, 5]), 1);
}

#[test]
fn test_count_line_ranges_unsorted() {
    // After sort+dedup: [1, 2, 5, 6, 10] -> ranges: [1,2], [5,6], [10]
    assert_eq!(count_line_ranges(&[10, 5, 6, 1, 2]), 3);
}

#[test]
fn test_count_line_ranges_two_ranges() {
    assert_eq!(count_line_ranges(&[1, 2, 3, 10, 11, 12]), 2);
}

#[test]
fn test_should_skip_stats_exactly_at_thresholds() {
    // Exactly at the hunks threshold alone should trigger skip.
    let at_hunks = StatsCostEstimate {
        files_with_additions: 0,
        added_lines: 0,
        hunk_ranges: STATS_SKIP_MAX_HUNKS,
        deleted_lines: 0,
    };
    assert!(
        should_skip_expensive_post_commit_stats(&at_hunks),
        "Exactly at hunk threshold should skip"
    );

    // Exactly at added-lines threshold alone should trigger skip.
    let at_added = StatsCostEstimate {
        files_with_additions: 0,
        added_lines: STATS_SKIP_MAX_ADDED_LINES,
        hunk_ranges: 0,
        deleted_lines: 0,
    };
    assert!(
        should_skip_expensive_post_commit_stats(&at_added),
        "Exactly at added-lines threshold should skip"
    );

    // Exactly at files-with-additions threshold alone should trigger skip.
    let at_files = StatsCostEstimate {
        files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
        added_lines: 0,
        hunk_ranges: 0,
        deleted_lines: 0,
    };
    assert!(
        should_skip_expensive_post_commit_stats(&at_files),
        "Exactly at files-with-additions threshold should skip"
    );

    // Exactly at deleted-lines threshold alone should trigger skip.
    let at_deleted = StatsCostEstimate {
        files_with_additions: 0,
        added_lines: 0,
        hunk_ranges: 0,
        deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
    };
    assert!(
        should_skip_expensive_post_commit_stats(&at_deleted),
        "Exactly at deleted-lines threshold should skip"
    );

    // All at zero should NOT skip.
    let all_zero = StatsCostEstimate {
        files_with_additions: 0,
        added_lines: 0,
        hunk_ranges: 0,
        deleted_lines: 0,
    };
    assert!(
        !should_skip_expensive_post_commit_stats(&all_zero),
        "All zero values should not skip"
    );
}

#[test]
fn test_post_commit_utf8_filename_with_ai_attribution() {
    // Create a repo with an initial commit
    let repo = TestRepo::new();

    // Create initial file and commit
    std::fs::write(repo.path().join("README.md"), "# Test\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create a file with Chinese characters in the filename
    let chinese_filename = "中文文件.txt";
    std::fs::write(repo.path().join(chinese_filename), "Hello, 世界!\n").unwrap();
    repo.git(&["add", chinese_filename]).unwrap();

    // Trigger AI checkpoint
    repo.git_ai(&["checkpoint", "mock_ai", chinese_filename])
        .unwrap();

    // Commit - this triggers the post-commit hook
    repo.stage_all_and_commit("Add Chinese file").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let note = repo
        .read_authorship_note(&head_sha)
        .expect("should have authorship note");

    // The note should reference the Chinese filename
    // Deserialize and check attestations contain the file
    let log = AuthorshipLog::deserialize_from_string(&note).unwrap();

    // Debug output
    println!(
        "Authorship log attestations: {:?}",
        log.attestations
    );

    // The attestation should include the Chinese filename
    assert_eq!(
        log.attestations.len(),
        1,
        "Should have 1 attestation for the Chinese-named file"
    );
    assert_eq!(
        log.attestations[0].file_path, chinese_filename,
        "File path should be the UTF-8 filename"
    );
}
