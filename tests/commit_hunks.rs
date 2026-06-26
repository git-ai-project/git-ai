#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use git_ai::commands::diff::{
    DiffCommandOptions, DiffJsonHunk, build_diff_artifacts_from_hunks,
    build_diff_artifacts_with_note, get_diff_with_line_numbers,
};
use git_ai::git::repository::Repository as GitAiRepository;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use sha2::{Digest, Sha256};
use std::fs;

fn get_repo(test_repo: &TestRepo) -> GitAiRepository {
    git_ai::git::find_repository_in_path(test_repo.path().to_str().unwrap())
        .expect("Failed to find repository")
}

fn get_parent_sha(test_repo: &TestRepo) -> String {
    match test_repo.git(&["rev-parse", "HEAD~1"]) {
        Ok(sha) => sha.trim().to_string(),
        // Initial commit has no parent - use empty tree
        Err(_) => "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(),
    }
}

fn compute_content_hash(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[test]
fn test_commit_hunks_basic_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    let content = "AI line 1\nAI line 2\nAI line 3\n";
    fs::write(&file_path, content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("add AI lines").unwrap();

    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(lines!["AI line 1".ai(), "AI line 2".ai(), "AI line 3".ai()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);

    // Use build_diff_artifacts_from_hunks (the optimized post-commit path)
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    // Should have exactly one addition hunk (all lines same attribution)
    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();
    assert_eq!(addition_hunks.len(), 1);

    let hunk = addition_hunks[0];
    assert_eq!(hunk.file_path, "example.txt");
    assert_eq!(hunk.hunk_kind, "addition");
    assert_eq!(hunk.start_line, 1);
    assert_eq!(hunk.end_line, 3);
    assert_eq!(hunk.commit_sha, commit.commit_sha);

    // Should have a prompt_id from the attestation
    assert!(hunk.prompt_id.is_some());

    // Content hash should match SHA256 of the 3 lines
    let expected_hash = compute_content_hash(&["AI line 1", "AI line 2", "AI line 3"]);
    assert_eq!(hunk.content_hash, expected_hash);

    // No deletion hunks
    let deletion_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "deletion")
        .collect();
    assert_eq!(deletion_hunks.len(), 0);

    // Verify parity with build_diff_artifacts_with_note
    let artifacts_old = build_diff_artifacts_with_note(
        &git_repo,
        &parent_sha,
        &commit.commit_sha,
        &DiffCommandOptions::default(),
        Some(&commit.authorship_log),
    )
    .unwrap();
    assert_eq!(artifacts.json_hunks.len(), artifacts_old.json_hunks.len());
    for (a, b) in artifacts
        .json_hunks
        .iter()
        .zip(artifacts_old.json_hunks.iter())
    {
        assert_eq!(a.file_path, b.file_path);
        assert_eq!(a.hunk_kind, b.hunk_kind);
        assert_eq!(a.start_line, b.start_line);
        assert_eq!(a.end_line, b.end_line);
        assert_eq!(a.content_hash, b.content_hash);
        assert_eq!(a.prompt_id, b.prompt_id);
        assert_eq!(a.human_id, b.human_id);
        assert_eq!(a.commit_sha, b.commit_sha);
    }
}

#[test]
fn test_commit_hunks_mixed_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("mixed.txt");

    // First: human writes lines
    let human_content = "Human line 1\nHuman line 2\n";
    fs::write(&file_path, human_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.txt"])
        .unwrap();

    // Then: AI adds lines after
    let mixed_content = "Human line 1\nHuman line 2\nAI line 1\nAI line 2\n";
    fs::write(&file_path, mixed_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "mixed.txt"])
        .unwrap();

    let commit = repo.stage_all_and_commit("mixed commit").unwrap();

    let mut file = repo.filename("mixed.txt");
    file.assert_committed_lines(lines![
        "Human line 1".human(),
        "Human line 2".human(),
        "AI line 1".ai(),
        "AI line 2".ai()
    ]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    // Should have 2 hunks: human (lines 1-2) and AI (lines 3-4)
    assert_eq!(addition_hunks.len(), 2);

    // First hunk: human lines
    let human_hunk = &addition_hunks[0];
    assert_eq!(human_hunk.file_path, "mixed.txt");
    assert_eq!(human_hunk.start_line, 1);
    assert_eq!(human_hunk.end_line, 2);
    assert!(human_hunk.human_id.is_some());
    assert!(human_hunk.prompt_id.is_none());

    // Second hunk: AI lines
    let ai_hunk = &addition_hunks[1];
    assert_eq!(ai_hunk.file_path, "mixed.txt");
    assert_eq!(ai_hunk.start_line, 3);
    assert_eq!(ai_hunk.end_line, 4);
    assert!(ai_hunk.prompt_id.is_some());
    assert!(ai_hunk.human_id.is_none());

    // Content hashes
    let human_hash = compute_content_hash(&["Human line 1", "Human line 2"]);
    assert_eq!(human_hunk.content_hash, human_hash);

    let ai_hash = compute_content_hash(&["AI line 1", "AI line 2"]);
    assert_eq!(ai_hunk.content_hash, ai_hash);
}

#[test]
fn test_commit_hunks_multiple_files() {
    let repo = TestRepo::new();

    let file_a = repo.path().join("a.txt");
    let file_b = repo.path().join("b.txt");

    fs::write(&file_a, "A line 1\nA line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "a.txt"]).unwrap();

    fs::write(&file_b, "B line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "b.txt"])
        .unwrap();

    let commit = repo.stage_all_and_commit("multi-file commit").unwrap();

    let mut file_a_test = repo.filename("a.txt");
    file_a_test.assert_committed_lines(lines!["A line 1".ai(), "A line 2".ai()]);
    let mut file_b_test = repo.filename("b.txt");
    file_b_test.assert_committed_lines(lines!["B line 1".human()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    assert_eq!(addition_hunks.len(), 2);

    let a_hunk = addition_hunks
        .iter()
        .find(|h| h.file_path == "a.txt")
        .unwrap();
    assert_eq!(a_hunk.start_line, 1);
    assert_eq!(a_hunk.end_line, 2);
    assert!(a_hunk.prompt_id.is_some());

    let b_hunk = addition_hunks
        .iter()
        .find(|h| h.file_path == "b.txt")
        .unwrap();
    assert_eq!(b_hunk.start_line, 1);
    assert_eq!(b_hunk.end_line, 1);
    assert!(b_hunk.human_id.is_some());
}

#[test]
fn test_commit_hunks_deletions_unattributed() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("deleteme.txt");

    // Initial commit with content
    fs::write(&file_path, "Line 1\nLine 2\nLine 3\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "deleteme.txt"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let mut file = repo.filename("deleteme.txt");
    file.assert_committed_lines(lines!["Line 1".ai(), "Line 2".ai(), "Line 3".ai()]);

    // Second commit: delete a line
    fs::write(&file_path, "Line 1\nLine 3\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "deleteme.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("delete line 2").unwrap();

    file.assert_committed_lines(lines!["Line 1".ai(), "Line 3".ai()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let deletion_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "deletion")
        .collect();

    assert_eq!(deletion_hunks.len(), 1);
    let del_hunk = deletion_hunks[0];
    assert_eq!(del_hunk.file_path, "deleteme.txt");
    assert_eq!(del_hunk.hunk_kind, "deletion");
    assert_eq!(del_hunk.start_line, 2);
    assert_eq!(del_hunk.end_line, 2);
    // Deletions are not attributed
    assert!(del_hunk.prompt_id.is_none());
    assert!(del_hunk.human_id.is_none());
    assert!(del_hunk.original_commit_sha.is_none());
}

#[test]
fn test_commit_hunks_consecutive_same_attribution_merges() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("merge.txt");

    // 10 consecutive AI lines should merge into one hunk
    let content = (1..=10)
        .map(|i| format!("AI line {}", i))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&file_path, &content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "merge.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("consecutive AI lines").unwrap();

    let mut file = repo.filename("merge.txt");
    file.assert_committed_lines(lines![
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
        "AI line 5".ai(),
        "AI line 6".ai(),
        "AI line 7".ai(),
        "AI line 8".ai(),
        "AI line 9".ai(),
        "AI line 10".ai()
    ]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    // All same attribution => single hunk
    assert_eq!(addition_hunks.len(), 1);
    assert_eq!(addition_hunks[0].start_line, 1);
    assert_eq!(addition_hunks[0].end_line, 10);
}

#[test]
fn test_commit_hunks_attribution_boundaries_split() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("split.txt");

    // Known human line first
    fs::write(&file_path, "Human\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "split.txt"])
        .unwrap();

    // Then AI adds
    fs::write(&file_path, "Human\nAI added\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "split.txt"])
        .unwrap();

    let commit = repo.stage_all_and_commit("boundary split").unwrap();

    let mut file = repo.filename("split.txt");
    file.assert_committed_lines(lines!["Human".human(), "AI added".ai()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    // Human and AI should be separate hunks
    assert_eq!(addition_hunks.len(), 2);

    // First hunk: human
    assert_eq!(addition_hunks[0].start_line, 1);
    assert_eq!(addition_hunks[0].end_line, 1);
    assert!(addition_hunks[0].prompt_id.is_none());
    assert!(addition_hunks[0].human_id.is_some());

    // Second hunk: AI
    assert_eq!(addition_hunks[1].start_line, 2);
    assert_eq!(addition_hunks[1].end_line, 2);
    assert!(addition_hunks[1].prompt_id.is_some());
}

#[test]
fn test_commit_hunks_session_id_extraction() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("session.txt");

    fs::write(&file_path, "Session line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "session.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("session test").unwrap();

    let mut file = repo.filename("session.txt");
    file.assert_committed_lines(lines!["Session line".ai()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    assert_eq!(addition_hunks.len(), 1);
    let hunk = addition_hunks[0];

    // The mock_ai preset uses session-format attestation hashes (s_ prefix)
    // If prompt_id starts with s_, session_id should be extracted
    if let Some(ref pid) = hunk.prompt_id
        && pid.starts_with("s_")
    {
        assert!(hunk.session_id.is_some());
        let sid = hunk.session_id.as_ref().unwrap();
        assert!(pid.starts_with(sid));
    }
}

#[test]
fn test_commit_hunks_empty_diff() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("empty.txt");

    // Create file in initial commit
    fs::write(&file_path, "content\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "empty.txt"])
        .unwrap();
    let first = repo.stage_all_and_commit("first").unwrap();

    let mut file = repo.filename("empty.txt");
    file.assert_committed_lines(lines!["content".ai()]);

    // Make same content commit (empty diff) by using allow-empty
    repo.git(&["commit", "--allow-empty", "-m", "empty"])
        .unwrap();
    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let git_repo = get_repo(&repo);
    let diff_hunks = get_diff_with_line_numbers(&git_repo, &first.commit_sha, &commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit_sha,
        None, // no authorship log for empty commit
    )
    .unwrap();

    assert_eq!(artifacts.json_hunks.len(), 0);
}

#[test]
fn test_commit_hunks_content_hash_correctness() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("hash_test.txt");

    let lines = ["first line", "second line", "third line"];
    let content = lines.join("\n") + "\n";
    fs::write(&file_path, &content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "hash_test.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("hash test").unwrap();

    let mut file = repo.filename("hash_test.txt");
    file.assert_committed_lines(lines![
        "first line".ai(),
        "second line".ai(),
        "third line".ai()
    ]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    assert_eq!(addition_hunks.len(), 1);
    let hunk = addition_hunks[0];

    // Manually compute expected hash
    let expected_hash = compute_content_hash(&["first line", "second line", "third line"]);
    assert_eq!(hunk.content_hash, expected_hash);
}

#[test]
fn test_commit_hunks_interleaved_attributions() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("interleaved.txt");

    // Human writes line 1
    fs::write(&file_path, "Human 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "interleaved.txt"])
        .unwrap();

    // AI adds lines 2-3
    fs::write(&file_path, "Human 1\nAI 1\nAI 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "interleaved.txt"])
        .unwrap();

    let commit = repo.stage_all_and_commit("interleaved").unwrap();

    let mut file = repo.filename("interleaved.txt");
    file.assert_committed_lines(lines!["Human 1".human(), "AI 1".ai(), "AI 2".ai()]);

    let git_repo = get_repo(&repo);
    let parent_sha = get_parent_sha(&repo);
    let diff_hunks =
        get_diff_with_line_numbers(&git_repo, &parent_sha, &commit.commit_sha).unwrap();
    let artifacts = build_diff_artifacts_from_hunks(
        &git_repo,
        diff_hunks,
        &commit.commit_sha,
        Some(&commit.authorship_log),
    )
    .unwrap();

    let addition_hunks: Vec<&DiffJsonHunk> = artifacts
        .json_hunks
        .iter()
        .filter(|h| h.hunk_kind == "addition")
        .collect();

    // Human hunk (line 1) + AI hunk (lines 2-3)
    assert_eq!(addition_hunks.len(), 2);

    assert_eq!(addition_hunks[0].start_line, 1);
    assert_eq!(addition_hunks[0].end_line, 1);
    assert!(addition_hunks[0].human_id.is_some());

    assert_eq!(addition_hunks[1].start_line, 2);
    assert_eq!(addition_hunks[1].end_line, 3);
    assert!(addition_hunks[1].prompt_id.is_some());
}
