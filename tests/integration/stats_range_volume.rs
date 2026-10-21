//! `git-ai stats --json A..B` puts the same additive `*_chars` / `*_bytes`
//! keys on `range_stats` as single-commit JSON. They are payload+LF sums of
//! the range's classified added/deleted lines from one `start..end` diff.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use crate::stats::range_stats_from_args_with_env;
use git_ai::authorship::range_authorship::RangeAuthorshipStats;
use std::fs;

fn range_stats_from_args(repo: &TestRepo, args: &[&str]) -> RangeAuthorshipStats {
    range_stats_from_args_with_env(repo, args, &[])
}

/// Independently computed payload+LF sums: chars = Unicode scalars + 1
/// synthetic LF per line, bytes = UTF-8 length + 1 synthetic LF per line.
fn payload_volume(lines: &[&str]) -> (u64, u64) {
    lines.iter().fold((0u64, 0u64), |(chars, bytes), line| {
        (
            chars + line.chars().count() as u64 + 1,
            bytes + line.len() as u64 + 1,
        )
    })
}

/// Mixed two-commit range (known-human + AI + untracked, plus a deleted
/// line): every `range_stats` char/byte field must equal the payload+LF sum of
/// the same classified line set as its line field.
#[test]
fn test_stats_range_volume_mixed_multi_commit() {
    let repo = TestRepo::new();

    // Base commit (range start boundary; its content is NOT part of the
    // range diff): one known-human line.
    fs::write(repo.path().join("file.txt"), "Base line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "file.txt"])
        .unwrap();
    let base = repo.stage_all_and_commit("base commit").unwrap();
    let mut file = repo.filename("file.txt");
    file.assert_committed_lines(lines!["Base line".human()]);

    // Commit 2 inside the range: known-human rewrite of file.txt (deletes
    // "Base line") plus a new AI-authored file. Multi-byte 🎉 exercises the
    // chars != bytes split (1 scalar, 4 UTF-8 bytes).
    fs::write(repo.path().join("file.txt"), "Human line 🎉\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "file.txt"])
        .unwrap();
    fs::write(repo.path().join("ai.txt"), "AI line 🎉\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai.txt"]).unwrap();
    repo.stage_all_and_commit("human and ai commit").unwrap();
    file.assert_committed_lines(lines!["Human line 🎉".human()]);
    let mut ai_file = repo.filename("ai.txt");
    ai_file.assert_committed_lines(lines!["AI line 🎉".ai()]);

    // Commit 3 inside the range: untracked line via plain git (no authorship
    // note), so the line lands in the unknown bucket.
    fs::write(repo.path().join("untracked.txt"), "Untracked line 🎉\n").unwrap();
    repo.git_og(&["add", "untracked.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "untracked commit"]).unwrap();
    let mut untracked_file = repo.filename("untracked.txt");
    untracked_file.assert_committed_lines(lines!["Untracked line 🎉".unattributed_human()]);

    let head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let range = format!("{}..{}", base.commit_sha, head);
    let stats = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let range_stats = &stats.range_stats;

    let (human_chars, human_bytes) = payload_volume(&["Human line 🎉"]);
    let (ai_chars, ai_bytes) = payload_volume(&["AI line 🎉"]);
    let (unknown_chars, unknown_bytes) = payload_volume(&["Untracked line 🎉"]);
    let (deleted_chars, deleted_bytes) = payload_volume(&["Base line"]);
    assert!(ai_bytes > ai_chars, "🎉 is 1 scalar but 4 UTF-8 bytes");
    assert!(human_bytes > human_chars);
    assert!(unknown_bytes > unknown_chars);

    assert_eq!(range_stats.git_diff_added_lines, 3);
    assert_eq!(range_stats.git_diff_deleted_lines, 1);
    assert_eq!(range_stats.human_additions, 1);
    assert_eq!(range_stats.ai_additions, 1);
    assert_eq!(range_stats.ai_accepted, 1);
    assert_eq!(range_stats.unknown_additions, 1);

    assert_eq!(range_stats.human_additions_chars, human_chars);
    assert_eq!(range_stats.human_additions_bytes, human_bytes);
    assert_eq!(range_stats.unknown_additions_chars, unknown_chars);
    assert_eq!(range_stats.unknown_additions_bytes, unknown_bytes);
    assert_eq!(range_stats.ai_additions_chars, ai_chars);
    assert_eq!(range_stats.ai_additions_bytes, ai_bytes);
    assert_eq!(range_stats.ai_accepted_chars, ai_chars);
    assert_eq!(range_stats.ai_accepted_bytes, ai_bytes);
    assert_eq!(
        range_stats.git_diff_added_chars,
        human_chars + ai_chars + unknown_chars
    );
    assert_eq!(
        range_stats.git_diff_added_bytes,
        human_bytes + ai_bytes + unknown_bytes
    );
    assert_eq!(range_stats.git_diff_deleted_chars, deleted_chars);
    assert_eq!(range_stats.git_diff_deleted_bytes, deleted_bytes);

    let tool_stats = range_stats
        .tool_model_breakdown
        .get("mock_ai::unknown")
        .expect("mock_ai::unknown tool headline should exist");
    assert_eq!(tool_stats.ai_accepted, 1);
    assert_eq!(tool_stats.ai_additions, 1);
    assert_eq!(tool_stats.ai_additions_chars, ai_chars);
    assert_eq!(tool_stats.ai_additions_bytes, ai_bytes);
    assert_eq!(tool_stats.ai_accepted_chars, ai_chars);
    assert_eq!(tool_stats.ai_accepted_bytes, ai_bytes);
}

/// Range whose commits have no authorship notes at all: every added line is
/// unattested, so the unknown bucket carries the full added volume and no AI
/// or human volume is invented. git-diff char/byte fields still reflect the
/// raw range diff.
#[test]
fn test_stats_range_volume_no_authorship_notes() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("range.txt"), "first\n").unwrap();
    repo.git_og(&["add", "range.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "first raw commit"]).unwrap();
    let first = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    fs::write(repo.path().join("range.txt"), "first\nsecond 🎉\n").unwrap();
    repo.git_og(&["add", "range.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "second raw commit"]).unwrap();

    fs::write(repo.path().join("other.txt"), "third line\n").unwrap();
    repo.git_og(&["add", "other.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "third raw commit"]).unwrap();
    let head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let range = format!("{}..{}", first, head);
    let stats = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let range_stats = &stats.range_stats;

    let (unknown_chars, unknown_bytes) = payload_volume(&["second 🎉", "third line"]);
    assert!(unknown_bytes > unknown_chars);

    assert_eq!(range_stats.git_diff_added_lines, 2);
    assert_eq!(range_stats.git_diff_deleted_lines, 0);
    assert_eq!(range_stats.human_additions, 0);
    assert_eq!(range_stats.ai_additions, 0);
    assert_eq!(range_stats.ai_accepted, 0);
    assert_eq!(range_stats.unknown_additions, 2);

    assert_eq!(range_stats.human_additions_chars, 0);
    assert_eq!(range_stats.human_additions_bytes, 0);
    assert_eq!(range_stats.ai_additions_chars, 0);
    assert_eq!(range_stats.ai_additions_bytes, 0);
    assert_eq!(range_stats.ai_accepted_chars, 0);
    assert_eq!(range_stats.ai_accepted_bytes, 0);
    assert_eq!(range_stats.unknown_additions_chars, unknown_chars);
    assert_eq!(range_stats.unknown_additions_bytes, unknown_bytes);
    assert_eq!(range_stats.git_diff_added_chars, unknown_chars);
    assert_eq!(range_stats.git_diff_added_bytes, unknown_bytes);
    assert_eq!(range_stats.git_diff_deleted_chars, 0);
    assert_eq!(range_stats.git_diff_deleted_bytes, 0);
    assert!(
        range_stats.tool_model_breakdown.is_empty(),
        "range with no authorship notes should have no per-tool breakdown"
    );
}

/// Range `--ignore` must drop volume the same way it drops lines. A
/// glob-ignored path contributes 0 lines and 0 chars/bytes on every
/// `range_stats` char/byte field (git-diff, bucket, and per-tool), while the
/// non-ignored file keeps its full payload+LF volume. The baseline run (no
/// flag) asserts the blob volume is otherwise included, so the drop is
/// attributable to the matcher, not to the file being unattested.
#[test]
fn test_stats_range_ignore_glob_drops_volume() {
    let repo = TestRepo::new();

    // Base commit (range start boundary; not part of the range diff).
    fs::write(repo.path().join("README.md"), "# Repo\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    let base = repo.stage_all_and_commit("base commit").unwrap();

    // One in-range commit: an AI source line plus an AI blob file that the
    // `--ignore *.blob` glob will exclude. `*.blob` is not covered by the
    // default ignore patterns, so only the explicit flag drops it.
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::create_dir_all(repo.path().join("assets")).unwrap();
    fs::write(repo.path().join("src/main.rs"), "fn main() \u{1F389}\n").unwrap();
    fs::write(
        repo.path().join("assets/data.blob"),
        "blob payload one \u{1F389}\nblob payload two\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let head = repo.stage_all_and_commit("ai commit").unwrap();

    let mut main_file = repo.filename("src/main.rs");
    main_file.assert_committed_lines(lines!["fn main() \u{1F389}".ai()]);
    let mut blob_file = repo.filename("assets/data.blob");
    blob_file.assert_committed_lines(lines![
        "blob payload one \u{1F389}".ai(),
        "blob payload two".ai()
    ]);

    let range = format!("{}..{}", base.commit_sha, head.commit_sha);
    let (main_chars, main_bytes) = payload_volume(&["fn main() \u{1F389}"]);
    let (blob_chars, blob_bytes) =
        payload_volume(&["blob payload one \u{1F389}", "blob payload two"]);

    // Baseline (no --ignore): the blob's lines and volume are included.
    let baseline = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let base_stats = &baseline.range_stats;
    assert_eq!(base_stats.git_diff_added_lines, 3);
    assert_eq!(base_stats.ai_additions, 3);
    assert_eq!(base_stats.git_diff_added_chars, main_chars + blob_chars);
    assert_eq!(base_stats.git_diff_added_bytes, main_bytes + blob_bytes);
    assert_eq!(base_stats.ai_additions_chars, main_chars + blob_chars);
    assert_eq!(base_stats.ai_additions_bytes, main_bytes + blob_bytes);

    // Ignored: the blob path adds 0 lines and 0 chars/bytes.
    let ignored = range_stats_from_args(&repo, &["stats", &range, "--json", "--ignore", "*.blob"]);
    let range_stats = &ignored.range_stats;

    assert_eq!(range_stats.git_diff_added_lines, 1);
    assert_eq!(range_stats.git_diff_deleted_lines, 0);
    assert_eq!(range_stats.ai_additions, 1);
    assert_eq!(range_stats.ai_accepted, 1);
    assert_eq!(range_stats.human_additions, 0);
    assert_eq!(range_stats.unknown_additions, 0);

    assert_eq!(range_stats.git_diff_added_chars, main_chars);
    assert_eq!(range_stats.git_diff_added_bytes, main_bytes);
    assert_eq!(range_stats.git_diff_deleted_chars, 0);
    assert_eq!(range_stats.git_diff_deleted_bytes, 0);
    assert_eq!(range_stats.ai_additions_chars, main_chars);
    assert_eq!(range_stats.ai_additions_bytes, main_bytes);
    assert_eq!(range_stats.ai_accepted_chars, main_chars);
    assert_eq!(range_stats.ai_accepted_bytes, main_bytes);
    assert_eq!(range_stats.human_additions_chars, 0);
    assert_eq!(range_stats.human_additions_bytes, 0);
    assert_eq!(range_stats.unknown_additions_chars, 0);
    assert_eq!(range_stats.unknown_additions_bytes, 0);

    let tool_stats = range_stats
        .tool_model_breakdown
        .get("mock_ai::unknown")
        .expect("mock_ai::unknown tool headline should exist");
    assert_eq!(tool_stats.ai_accepted, 1);
    assert_eq!(tool_stats.ai_additions_chars, main_chars);
    assert_eq!(tool_stats.ai_additions_bytes, main_bytes);
    assert_eq!(tool_stats.ai_accepted_chars, main_chars);
    assert_eq!(tool_stats.ai_accepted_bytes, main_bytes);
}

/// Git C-quotes non-ASCII paths in diff headers, and the on-disk name may be
/// NFC or NFD. An NFC ignore pattern must still drop the path's lines and
/// chars/bytes from every range bucket.
fn assert_range_ignores_non_ascii_path_volume(ignored_file_name: &str) {
    let repo = TestRepo::new();

    fs::write(repo.path().join(".git-ai-ignore"), "café.txt\n").unwrap();
    fs::write(repo.path().join(ignored_file_name), "ignored\n").unwrap();
    fs::write(repo.path().join("app.txt"), "kept\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human"]).unwrap();
    let first = repo.stage_all_and_commit("initial").unwrap();
    let mut app_file = repo.filename("app.txt");
    app_file.assert_committed_lines(lines!["kept".human()]);

    fs::write(
        repo.path().join(ignored_file_name),
        "ignored\nignored addition \u{1F389}\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("app.txt"),
        "kept\nkept addition \u{1F389}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    let second = repo.stage_all_and_commit("add lines").unwrap();
    app_file.assert_committed_lines(lines!["kept".human(), "kept addition \u{1F389}".ai()]);

    let range = format!("{}..{}", first.commit_sha, second.commit_sha);
    let stats = range_stats_from_args_with_env(
        &repo,
        &["stats", &range, "--json"],
        &[("LC_ALL", "en_US.UTF-8"), ("LANG", "en_US.UTF-8")],
    );
    let range_stats = &stats.range_stats;
    let (kept_chars, kept_bytes) = payload_volume(&["kept addition \u{1F389}"]);

    assert_eq!(range_stats.git_diff_added_lines, 1);
    assert_eq!(range_stats.git_diff_deleted_lines, 0);
    assert_eq!(range_stats.ai_additions, 1);
    assert_eq!(range_stats.git_diff_added_chars, kept_chars);
    assert_eq!(range_stats.git_diff_added_bytes, kept_bytes);
    assert_eq!(range_stats.git_diff_deleted_chars, 0);
    assert_eq!(range_stats.git_diff_deleted_bytes, 0);
    assert_eq!(range_stats.ai_additions_chars, kept_chars);
    assert_eq!(range_stats.ai_additions_bytes, kept_bytes);
    assert_eq!(range_stats.ai_accepted_chars, kept_chars);
    assert_eq!(range_stats.ai_accepted_bytes, kept_bytes);
    assert_eq!(range_stats.human_additions_chars, 0);
    assert_eq!(range_stats.unknown_additions_chars, 0);
}

#[test]
fn test_stats_range_ignore_precomposed_non_ascii_path_drops_volume() {
    assert_range_ignores_non_ascii_path_volume("caf\u{e9}.txt");
}

#[test]
fn test_stats_range_ignore_decomposed_non_ascii_path_drops_volume() {
    assert_range_ignores_non_ascii_path_volume("cafe\u{301}.txt");
}

/// Range stats apply `stats_max_file_bytes` the same way single-commit stats
/// do: an over-cap file keeps its line counts but contributes 0 to every
/// char/byte field, and raising the cap restores its volume.
#[test]
fn test_stats_range_file_cap_skips_oversized_file_volume() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|patch| {
        patch.stats_max_file_bytes = Some(50);
    });

    fs::write(repo.path().join("README.md"), "# Repo\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    let base = repo.stage_all_and_commit("base commit").unwrap();
    let mut readme = repo.filename("README.md");
    readme.assert_committed_lines(lines!["# Repo".human()]);

    fs::write(repo.path().join("small.txt"), "tiny\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "small.txt"])
        .unwrap();
    repo.stage_all_and_commit("small human").unwrap();
    let mut small_file = repo.filename("small.txt");
    small_file.assert_committed_lines(lines!["tiny".human()]);

    let big_line = "a".repeat(100); // 101 counted bytes with the synthetic LF
    fs::write(repo.path().join("large.txt"), format!("{big_line}\n")).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "large.txt"])
        .unwrap();
    let head = repo.stage_all_and_commit("large ai").unwrap();
    let mut large_file = repo.filename("large.txt");
    large_file.assert_committed_lines(lines![big_line.as_str().ai()]);

    let range = format!("{}..{}", base.commit_sha, head.commit_sha);
    let (small_chars, small_bytes) = payload_volume(&["tiny"]);
    let (large_chars, large_bytes) = payload_volume(&[big_line.as_str()]);

    let stats = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let range_stats = &stats.range_stats;
    assert_eq!(range_stats.git_diff_added_lines, 2);
    assert_eq!(range_stats.human_additions, 1);
    assert_eq!(range_stats.ai_additions, 1);
    assert_eq!(range_stats.unknown_additions, 0);
    assert_eq!(range_stats.git_diff_added_chars, small_chars);
    assert_eq!(range_stats.git_diff_added_bytes, small_bytes);
    assert_eq!(range_stats.human_additions_chars, small_chars);
    assert_eq!(range_stats.human_additions_bytes, small_bytes);
    assert_eq!(range_stats.ai_additions_chars, 0);
    assert_eq!(range_stats.ai_additions_bytes, 0);
    assert_eq!(range_stats.ai_accepted_chars, 0);
    assert_eq!(range_stats.ai_accepted_bytes, 0);
    assert_eq!(range_stats.unknown_additions_chars, 0);
    assert_eq!(range_stats.unknown_additions_bytes, 0);
    let tool_stats = range_stats
        .tool_model_breakdown
        .get("mock_ai::unknown")
        .expect("mock_ai::unknown tool headline should exist");
    assert_eq!(tool_stats.ai_accepted, 1);
    assert_eq!(tool_stats.ai_accepted_chars, 0);
    assert_eq!(tool_stats.ai_accepted_bytes, 0);

    repo.patch_git_ai_config(|patch| {
        patch.stats_max_file_bytes = Some(1_000_000);
    });
    let stats = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let range_stats = &stats.range_stats;
    assert_eq!(range_stats.git_diff_added_chars, small_chars + large_chars);
    assert_eq!(range_stats.git_diff_added_bytes, small_bytes + large_bytes);
    assert_eq!(range_stats.ai_additions_chars, large_chars);
    assert_eq!(range_stats.ai_additions_bytes, large_bytes);
    let tool_stats = range_stats
        .tool_model_breakdown
        .get("mock_ai::unknown")
        .expect("mock_ai::unknown tool headline should exist");
    assert_eq!(tool_stats.ai_accepted_chars, large_chars);
    assert_eq!(tool_stats.ai_accepted_bytes, large_bytes);
}

/// Range line totals use the same `--find-renames=1%` rename detection as
/// single-commit stats and the range attribution diffs, so a weakly similar
/// rename counts only its edited lines (not a full delete + add) and the
/// three line buckets still sum to the git-diff total.
#[test]
fn test_stats_range_low_similarity_rename_counts_edited_lines_only() {
    let repo = TestRepo::new();

    let original: Vec<String> = (1..=10).map(|n| format!("shared line {n}")).collect();
    fs::write(
        repo.path().join("old.txt"),
        format!("{}\n", original.join("\n")),
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "old.txt"])
        .unwrap();
    let base = repo.stage_all_and_commit("base commit").unwrap();
    let mut old_file = repo.filename("old.txt");
    let expected_old: Vec<_> = original.iter().map(|line| line.as_str().human()).collect();
    old_file.assert_committed_lines(expected_old);

    // Keep 2 of 10 lines (about 20% similar, below git's default 50% rename
    // threshold) and rename the file.
    let mut rewritten = vec![original[0].clone(), original[1].clone()];
    rewritten.extend((3..=10).map(|n| format!("rewritten line {n}")));
    fs::remove_file(repo.path().join("old.txt")).unwrap();
    fs::write(
        repo.path().join("new.txt"),
        format!("{}\n", rewritten.join("\n")),
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "new.txt"]).unwrap();
    let head = repo.stage_all_and_commit("rename and rewrite").unwrap();
    // The AI checkpoint sees new.txt as a new file, so every line is AI.
    let mut new_file = repo.filename("new.txt");
    let expected_new: Vec<_> = rewritten.iter().map(|line| line.as_str().ai()).collect();
    new_file.assert_committed_lines(expected_new);

    let range = format!("{}..{}", base.commit_sha, head.commit_sha);
    let stats = range_stats_from_args(&repo, &["stats", &range, "--json"]);
    let range_stats = &stats.range_stats;
    assert_eq!(range_stats.git_diff_added_lines, 8);
    assert_eq!(range_stats.git_diff_deleted_lines, 8);
    assert_eq!(
        range_stats.ai_additions + range_stats.human_additions + range_stats.unknown_additions,
        range_stats.git_diff_added_lines
    );

    let commit_stats = crate::stats::stats_from_args(&repo, &["stats", &head.commit_sha, "--json"]);
    assert_eq!(
        commit_stats.git_diff_added_lines,
        range_stats.git_diff_added_lines
    );
    assert_eq!(
        commit_stats.git_diff_deleted_lines,
        range_stats.git_diff_deleted_lines
    );
}
