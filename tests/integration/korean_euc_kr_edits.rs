use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{NewCommit, TestRepo};
use git_ai::authorship::authorship_log::LineRange;
use std::fs;

const FILE_NAME: &str = "sum.c";

/// "// 두 수의 합계를 계산한다" in EUC-KR (CP949). Not valid UTF-8.
const KOREAN_COMMENT: &[u8] = &[
    0x2F, 0x2F, 0x20, 0xB5, 0xCE, 0x20, 0xBC, 0xF6, 0xC0, 0xC7, 0x20, 0xC7, 0xD5, 0xB0, 0xE8, 0xB8,
    0xA6, 0x20, 0xB0, 0xE8, 0xBB, 0xEA, 0xC7, 0xD1, 0xB4, 0xD9,
];

fn euc_kr_source(lines: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line);
        bytes.push(b'\n');
    }
    bytes
}

/// How git-ai renders the EUC-KR comment line in blame output.
fn korean_comment_as_blamed() -> String {
    String::from_utf8_lossy(KOREAN_COMMENT).into_owned()
}

/// Commits the EUC-KR base file without any checkpoint, so every line is untracked.
fn commit_euc_kr_base_file(repo: &TestRepo) {
    let base = euc_kr_source(&[
        b"#include <stdio.h>",
        KOREAN_COMMENT,
        b"int sum(int a, int b) {",
        b"    return a + b;",
        b"}",
    ]);
    fs::write(repo.path().join(FILE_NAME), base).unwrap();
    repo.stage_all_and_commit("Add EUC-KR source file").unwrap();

    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "#include <stdio.h>".unattributed_human(),
            korean_comment_as_blamed().unattributed_human(),
            "int sum(int a, int b) {".unattributed_human(),
            "    return a + b;".unattributed_human(),
            "}".unattributed_human(),
        ]);
}

/// Mocks an AI agent editing the EUC-KR file: pre-edit checkpoint, the edit, post-edit checkpoint.
fn ai_edits_euc_kr_file(repo: &TestRepo, lines: &[&[u8]], message: &str) -> NewCommit {
    repo.git_ai(&["checkpoint", "human", FILE_NAME])
        .expect("pre-edit checkpoint should succeed on an EUC-KR file");

    fs::write(repo.path().join(FILE_NAME), euc_kr_source(lines)).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", FILE_NAME])
        .expect("AI checkpoint should succeed on an EUC-KR file");

    repo.stage_all_and_commit(message)
        .expect("commit should succeed for an EUC-KR file")
}

/// Asserts the commit has an authorship note whose AI-attested lines for the file are `expected`.
fn assert_note_ai_line_ranges(repo: &TestRepo, commit: &NewCommit, expected: Vec<LineRange>) {
    let note = repo
        .read_authorship_note(&commit.commit_sha)
        .expect("commit should have an authorship note");
    assert!(
        note.contains(FILE_NAME),
        "authorship note should attest {FILE_NAME}, got:\n{note}"
    );

    let mut ai_line_ranges: Vec<LineRange> = commit
        .authorship_log
        .attestations
        .iter()
        .filter(|attestation| attestation.file_path == FILE_NAME)
        .flat_map(|attestation| &attestation.entries)
        .filter(|entry| !entry.hash.starts_with("h_"))
        .flat_map(|entry| entry.line_ranges.clone())
        .collect();
    ai_line_ranges.sort();
    assert_eq!(
        ai_line_ranges, expected,
        "unexpected AI line ranges in authorship note:\n{note}"
    );
}

#[test]
fn test_euc_kr_ai_adds_lines_before_korean_comment() {
    let repo = TestRepo::new();
    commit_euc_kr_base_file(&repo);

    let commit = ai_edits_euc_kr_file(
        &repo,
        &[
            b"#include <stdio.h>",
            b"#define SUM_LIMIT 100",
            b"static int sum_calls = 0;",
            KOREAN_COMMENT,
            b"int sum(int a, int b) {",
            b"    return a + b;",
            b"}",
        ],
        "AI adds lines before the Korean comment",
    );

    assert_note_ai_line_ranges(&repo, &commit, vec![LineRange::Range(2, 3)]);
    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "#include <stdio.h>".unattributed_human(),
            "#define SUM_LIMIT 100".ai(),
            "static int sum_calls = 0;".ai(),
            korean_comment_as_blamed().unattributed_human(),
            "int sum(int a, int b) {".unattributed_human(),
            "    return a + b;".unattributed_human(),
            "}".unattributed_human(),
        ]);
}

#[test]
fn test_euc_kr_ai_adds_lines_after_korean_comment() {
    let repo = TestRepo::new();
    commit_euc_kr_base_file(&repo);

    let commit = ai_edits_euc_kr_file(
        &repo,
        &[
            b"#include <stdio.h>",
            KOREAN_COMMENT,
            b"#define SUM_LIMIT 100",
            b"static int sum_calls = 0;",
            b"int sum(int a, int b) {",
            b"    return a + b;",
            b"}",
        ],
        "AI adds lines after the Korean comment",
    );

    assert_note_ai_line_ranges(&repo, &commit, vec![LineRange::Range(3, 4)]);
    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "#include <stdio.h>".unattributed_human(),
            korean_comment_as_blamed().unattributed_human(),
            "#define SUM_LIMIT 100".ai(),
            "static int sum_calls = 0;".ai(),
            "int sum(int a, int b) {".unattributed_human(),
            "    return a + b;".unattributed_human(),
            "}".unattributed_human(),
        ]);
}

#[test]
fn test_euc_kr_ai_adds_lines_before_and_after_korean_comment() {
    let repo = TestRepo::new();
    commit_euc_kr_base_file(&repo);

    let commit = ai_edits_euc_kr_file(
        &repo,
        &[
            b"#include <stdio.h>",
            b"#define SUM_LIMIT 100",
            KOREAN_COMMENT,
            b"static int sum_calls = 0;",
            b"int sum(int a, int b) {",
            b"    return a + b;",
            b"}",
        ],
        "AI adds lines around the Korean comment",
    );

    assert_note_ai_line_ranges(
        &repo,
        &commit,
        vec![LineRange::Single(2), LineRange::Single(4)],
    );
    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "#include <stdio.h>".unattributed_human(),
            "#define SUM_LIMIT 100".ai(),
            korean_comment_as_blamed().unattributed_human(),
            "static int sum_calls = 0;".ai(),
            "int sum(int a, int b) {".unattributed_human(),
            "    return a + b;".unattributed_human(),
            "}".unattributed_human(),
        ]);
}

// TODO Lossy decoding maps different EUC-KR byte sequences to the same replacement characters, so
// an edit that only swaps Korean text for Korean text of the same shape is invisible to the
// checkpoint diff. Needs a lossless decoding shared by every content read in the pipeline.
#[test]
#[ignore]
fn test_euc_kr_ai_rewrites_korean_comment_with_same_lossy_shape() {
    // "// 평균" and "// 결과" in EUC-KR: different bytes, identical lossy text.
    let comment_before: &[u8] = &[0x2F, 0x2F, 0x20, 0xC6, 0xF2, 0xB1, 0xD5];
    let comment_after: &[u8] = &[0x2F, 0x2F, 0x20, 0xB0, 0xE1, 0xB0, 0xFA];
    let comment_as_blamed = String::from_utf8_lossy(comment_after).into_owned();
    assert_eq!(String::from_utf8_lossy(comment_before), comment_as_blamed);

    let repo = TestRepo::new();
    let base = euc_kr_source(&[b"int a;", comment_before, b"int b;"]);
    fs::write(repo.path().join(FILE_NAME), base).unwrap();
    repo.stage_all_and_commit("Add EUC-KR source file").unwrap();
    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "int a;".unattributed_human(),
            comment_as_blamed.clone().unattributed_human(),
            "int b;".unattributed_human(),
        ]);

    let commit = ai_edits_euc_kr_file(
        &repo,
        &[b"int a;", comment_after, b"int b;"],
        "AI rewrites the Korean comment",
    );

    assert_note_ai_line_ranges(&repo, &commit, vec![LineRange::Single(2)]);
    repo.filename(FILE_NAME)
        .assert_committed_lines(crate::lines![
            "int a;".unattributed_human(),
            comment_as_blamed.ai(),
            "int b;".unattributed_human(),
        ]);
}

crate::reuse_tests_in_worktree!(
    test_euc_kr_ai_adds_lines_before_korean_comment,
    test_euc_kr_ai_adds_lines_after_korean_comment,
    test_euc_kr_ai_adds_lines_before_and_after_korean_comment,
);
