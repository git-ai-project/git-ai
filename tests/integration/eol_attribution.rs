use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::range_authorship::range_authorship;
use git_ai::authorship::stats::stats_for_commit_stats;
use git_ai::git::repository::{CommitRange, find_repository_in_path};

const FILE_PATH: &str = "src/eol.rs";
const LF_BASE: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
const CRLF_WITH_DELTA: &str = "fn alpha() {}\r\nfn beta() {}\r\nfn gamma() {}\r\nfn delta() {}\r\n";

#[test]
fn test_stats_lf_to_crlf_rewrite_counts_only_logical_addition() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), LF_BASE).unwrap();
    repo.stage_all_and_commit("Initial LF file").unwrap();

    std::fs::write(repo.path().join(FILE_PATH), CRLF_WITH_DELTA).unwrap();
    repo.stage_all_and_commit("Rewrite to CRLF with one new line")
        .unwrap();

    let head_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, head_sha.trim(), &[]).unwrap();

    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.git_diff_deleted_lines, 0);
    assert_eq!(stats.ai_additions, 0);
    assert_eq!(stats.ai_accepted, 0);
    assert_eq!(stats.unknown_additions, 1);
}

#[test]
fn test_stats_append_after_no_final_newline_still_counts_previous_line_change() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), "fn alpha() {}\nfn beta() {}").unwrap();
    repo.stage_all_and_commit("Initial file without final newline")
        .unwrap();

    std::fs::write(
        repo.path().join(FILE_PATH),
        "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n",
    )
    .unwrap();
    repo.stage_all_and_commit("Append after adding final newline")
        .unwrap();

    let head_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, head_sha.trim(), &[]).unwrap();

    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.git_diff_deleted_lines, 1);
}

#[test]
fn test_ai_checkpoint_lf_to_crlf_rewrite_attributes_only_new_line_to_ai() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), LF_BASE).unwrap();
    repo.stage_all_and_commit("Initial LF file").unwrap();

    std::fs::write(repo.path().join(FILE_PATH), CRLF_WITH_DELTA).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", FILE_PATH]).unwrap();
    repo.stage_all_and_commit("AI adds one line while file becomes CRLF")
        .unwrap();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.git_diff_deleted_lines, 0);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 0);
    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.ai_accepted, 1);

    repo.filename(FILE_PATH).assert_committed_lines(lines![
        "fn alpha() {}".human(),
        "fn beta() {}".human(),
        "fn gamma() {}".human(),
        "fn delta() {}".ai(),
    ]);
}

#[test]
fn test_ai_checkpoint_lf_to_crlf_top_insert_preserves_existing_authorship() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), "fn alpha() {}\nfn beta() {}\n").unwrap();
    repo.stage_all_and_commit("Initial LF file").unwrap();

    std::fs::write(
        repo.path().join(FILE_PATH),
        "fn inserted() {}\r\nfn alpha() {}\r\nfn beta() {}\r\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", FILE_PATH]).unwrap();
    repo.stage_all_and_commit("AI inserts at top while file becomes CRLF")
        .unwrap();

    let stats = repo.stats().unwrap();
    assert_eq!(stats.git_diff_added_lines, 1);
    assert_eq!(stats.git_diff_deleted_lines, 0);
    assert_eq!(stats.ai_additions, 1);
    assert_eq!(stats.ai_accepted, 1);

    repo.filename(FILE_PATH).assert_committed_lines(lines![
        "fn inserted() {}".ai(),
        "fn alpha() {}".human(),
        "fn beta() {}".human(),
    ]);
}

#[test]
fn test_range_stats_lf_to_crlf_rewrite_counts_only_logical_addition() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), LF_BASE).unwrap();
    repo.stage_all_and_commit("Initial LF file").unwrap();
    let first_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();

    std::fs::write(repo.path().join(FILE_PATH), CRLF_WITH_DELTA).unwrap();
    repo.stage_all_and_commit("Rewrite to CRLF with one new line")
        .unwrap();
    let second_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let commit_range = CommitRange::new_infer_refname(
        &gitai_repo,
        first_sha.trim().to_string(),
        second_sha.trim().to_string(),
        Some("HEAD".to_string()),
    )
    .unwrap();
    let stats = range_authorship(commit_range, false, &[], None).unwrap();

    assert_eq!(stats.range_stats.git_diff_added_lines, 1);
    assert_eq!(stats.range_stats.git_diff_deleted_lines, 0);
}

#[test]
fn test_range_ai_accepted_lf_to_crlf_rewrite_counts_only_logical_addition() {
    let repo = TestRepo::new();

    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join(FILE_PATH), LF_BASE).unwrap();
    repo.stage_all_and_commit("Initial LF file").unwrap();
    let first_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();

    std::fs::write(repo.path().join(FILE_PATH), CRLF_WITH_DELTA).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", FILE_PATH]).unwrap();
    repo.stage_all_and_commit("AI adds one line while file becomes CRLF")
        .unwrap();
    let second_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let commit_range = CommitRange::new_infer_refname(
        &gitai_repo,
        first_sha.trim().to_string(),
        second_sha.trim().to_string(),
        Some("HEAD".to_string()),
    )
    .unwrap();
    let stats = range_authorship(commit_range, false, &[], None).unwrap();

    assert_eq!(stats.range_stats.git_diff_added_lines, 1);
    assert_eq!(stats.range_stats.git_diff_deleted_lines, 0);
    assert_eq!(stats.range_stats.ai_additions, 1);
    assert_eq!(stats.range_stats.ai_accepted, 1);
}
