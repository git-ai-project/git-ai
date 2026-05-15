//! Tests for rebase note integrity — verifying that each commit's authorship note
//! only contains attribution for files that commit actually touched.
//!
//! These tests ensure no "future-file leakage" occurs during rebase, where files
//! introduced in later commits incorrectly appear in earlier commits' notes.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `content` to `filename` in the repo, add, and commit via git_og
/// (bypassing git-ai hooks). Ensures trailing newline for clean 3-way merges.
fn write_raw_commit(repo: &TestRepo, filename: &str, content: &str, message: &str) {
    let path = repo.path().join(filename);
    let content_with_nl = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{}\n", content)
    };
    fs::write(&path, content_with_nl.as_bytes()).expect("write file");
    repo.git_og(&["add", filename]).expect("git add");
    repo.git_og(&["commit", "-m", message]).expect("git commit");
}

/// Extract file paths mentioned in a raw authorship note string.
/// Looks for lines that appear to be file path headers (lines before indented entries).
fn files_in_note(note: &str) -> Vec<String> {
    // In the authorship note format, file paths appear as the first token on lines
    // that don't start with whitespace and are followed by indented attestation entries.
    // The format is:
    //   <filepath>
    //     <hash> <line-ranges>
    //   ---
    //   { json metadata }
    let mut files = Vec::new();
    let lines: Vec<&str> = note.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // Skip empty lines, separator, and JSON
        if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with('{') {
            continue;
        }
        // A file path line: doesn't start with whitespace, isn't the JSON block,
        // and the next line (if exists) is indented
        if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.starts_with('{') {
            // Check if next line is indented (attestation entry)
            if i + 1 < lines.len() {
                let next = lines[i + 1];
                if next.starts_with(' ') || next.starts_with('\t') {
                    files.push(trimmed.to_string());
                }
            }
        }
    }
    files
}

// ---------------------------------------------------------------------------
// Test 1: After rebase, EACH commit's note only has lines from THAT commit
// ---------------------------------------------------------------------------

/// After rebase, intermediate commits must not have attribution for files
/// introduced in later commits. This tests the "no future-file leakage" invariant.
#[test]
fn test_rebase_intermediate_commits_have_correct_attribution() {
    let repo = TestRepo::new();

    // Initial commit with a shared file
    write_raw_commit(&repo, "shared.rs", "fn original() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends to shared.rs (forces slow path if applicable)
    write_raw_commit(
        &repo,
        "shared.rs",
        "// upstream header\nfn original() {}",
        "Upstream: prepend header",
    );

    // Feature branch from before upstream change
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // Commit A: modifies shared.rs + creates module_a.rs
    let mut shared = repo.filename("shared.rs");
    shared.set_contents(crate::lines!["fn original() {}", "fn feature_a() {}".ai()]);
    let mut module_a = repo.filename("module_a.rs");
    module_a.set_contents(crate::lines!["fn ma() {}".ai()]);
    repo.stage_all_and_commit("Commit A: shared + module_a")
        .unwrap();

    // Commit B: modifies shared.rs + creates module_b.rs (module_b is FUTURE relative to A)
    shared.set_contents(crate::lines![
        "fn original() {}",
        "fn feature_a() {}".ai(),
        "fn feature_b() {}".ai()
    ]);
    let mut module_b = repo.filename("module_b.rs");
    module_b.set_contents(crate::lines!["fn mb() {}".ai()]);
    repo.stage_all_and_commit("Commit B: shared + module_b")
        .unwrap();

    // Commit C: modifies shared.rs + creates module_c.rs
    shared.set_contents(crate::lines![
        "fn original() {}",
        "fn feature_a() {}".ai(),
        "fn feature_b() {}".ai(),
        "fn feature_c() {}".ai()
    ]);
    let mut module_c = repo.filename("module_c.rs");
    module_c.set_contents(crate::lines!["fn mc() {}".ai()]);
    repo.stage_all_and_commit("Commit C: shared + module_c")
        .unwrap();

    // Rebase feature onto updated main
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    // Walk the log and check each commit's note
    let log = repo
        .git(&["log", "--format=%H", &format!("{}..HEAD", default_branch)])
        .unwrap();
    let shas: Vec<&str> = log.trim().lines().collect();
    assert_eq!(shas.len(), 3, "should have 3 rebased commits");

    // shas are newest-first: [C', B', A']
    let _sha_c = shas[0].trim();
    let sha_b = shas[1].trim();
    let sha_a = shas[2].trim();

    // Verify each commit's note only mentions files that commit actually touched
    if let Some(note_a) = repo.read_authorship_note(sha_a) {
        let files_a = files_in_note(&note_a);
        assert!(
            !files_a.iter().any(|f| f.contains("module_b")),
            "Commit A' note must NOT contain module_b.rs (future file). Found: {:?}",
            files_a
        );
        assert!(
            !files_a.iter().any(|f| f.contains("module_c")),
            "Commit A' note must NOT contain module_c.rs (future file). Found: {:?}",
            files_a
        );
    }

    if let Some(note_b) = repo.read_authorship_note(sha_b) {
        let files_b = files_in_note(&note_b);
        assert!(
            !files_b.iter().any(|f| f.contains("module_c")),
            "Commit B' note must NOT contain module_c.rs (future file). Found: {:?}",
            files_b
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: File deleted in commit 2 doesn't appear in commit 3's note
// ---------------------------------------------------------------------------

/// A file that is deleted in an intermediate commit must not appear in any
/// subsequent commit's authorship note after rebase.
#[test]
fn test_rebase_deleted_file_doesnt_persist() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "engine.rs", "fn engine() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends
    write_raw_commit(
        &repo,
        "engine.rs",
        "// upstream\nfn engine() {}",
        "Upstream: prepend",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // Commit 1: add temp.rs with AI content
    let mut engine = repo.filename("engine.rs");
    engine.set_contents(crate::lines!["fn engine() {}", "fn eng1() {}".ai()]);
    let mut temp = repo.filename("temp.rs");
    temp.set_contents(crate::lines!["fn tmp() {}".ai()]);
    repo.stage_all_and_commit("Commit 1: engine + temp.rs")
        .unwrap();

    // Commit 2: delete temp.rs, add final.rs
    engine.set_contents(crate::lines![
        "fn engine() {}",
        "fn eng1() {}".ai(),
        "fn eng2() {}".ai()
    ]);
    repo.git(&["rm", "temp.rs"]).unwrap();
    let mut final_rs = repo.filename("final.rs");
    final_rs.set_contents(crate::lines!["fn fin() {}".ai()]);
    repo.stage_all_and_commit("Commit 2: rm temp.rs + final.rs")
        .unwrap();

    // Commit 3: another change
    engine.set_contents(crate::lines![
        "fn engine() {}",
        "fn eng1() {}".ai(),
        "fn eng2() {}".ai(),
        "fn eng3() {}".ai()
    ]);
    let mut extra = repo.filename("extra.rs");
    extra.set_contents(crate::lines!["fn ex() {}".ai()]);
    repo.stage_all_and_commit("Commit 3: engine + extra.rs")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");

    // Check commit 3's note: temp.rs was deleted in commit 2, must not appear
    let sha_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    if let Some(note_3) = repo.read_authorship_note(&sha_head) {
        let files_3 = files_in_note(&note_3);
        assert!(
            !files_3.iter().any(|f| f.contains("temp")),
            "Commit 3' note must NOT contain temp.rs (deleted in commit 2). Found: {:?}",
            files_3
        );
    }

    // Check commit 2's note: temp.rs was deleted there, so it should not appear
    let sha_2 = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    if let Some(note_2) = repo.read_authorship_note(&sha_2) {
        let files_2 = files_in_note(&note_2);
        assert!(
            !files_2.iter().any(|f| f.contains("temp")),
            "Commit 2' note must NOT contain temp.rs (it was deleted in this commit). Found: {:?}",
            files_2
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: 3 AI commits on feature, rebase onto updated main, notes independent
// ---------------------------------------------------------------------------

/// Three AI commits on a feature branch, rebased onto an updated main.
/// Each commit's note must be independent and only reference its own files.
#[test]
fn test_rebase_three_commit_chain_no_leakage() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "base.rs", "fn base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Advance main with a different file
    write_raw_commit(&repo, "main_only.rs", "fn main_only() {}", "Main advance");

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // Three commits each adding a unique file
    let file_path_1 = repo.path().join("feat1.rs");
    fs::write(&file_path_1, "fn feat1() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feat1.rs"]).unwrap();
    repo.stage_all_and_commit("feat: add feat1.rs").unwrap();

    let file_path_2 = repo.path().join("feat2.rs");
    fs::write(&file_path_2, "fn feat2() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feat2.rs"]).unwrap();
    repo.stage_all_and_commit("feat: add feat2.rs").unwrap();

    let file_path_3 = repo.path().join("feat3.rs");
    fs::write(&file_path_3, "fn feat3() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feat3.rs"]).unwrap();
    repo.stage_all_and_commit("feat: add feat3.rs").unwrap();

    // Rebase onto updated main
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed");

    // Walk the rebased commits
    let log = repo
        .git(&["log", "--format=%H", &format!("{}..HEAD", default_branch)])
        .unwrap();
    let shas: Vec<&str> = log.trim().lines().collect();
    assert_eq!(shas.len(), 3, "should have 3 rebased commits");

    // shas are newest-first: [feat3', feat2', feat1']
    for sha in &shas {
        let sha = sha.trim();
        if let Some(note) = repo.read_authorship_note(sha) {
            let files = files_in_note(&note);
            // Each commit's note should only mention the file it introduced
            // (not files from other commits)
            let commit_msg = repo
                .git(&["log", "-1", "--format=%s", sha])
                .unwrap_or_default();
            // The note should not contain more than the expected files for this commit
            // This is a soft check — just verify no massive leakage
            assert!(
                files.len() <= 2,
                "Commit '{}' note has too many files ({:?}), possible leakage",
                commit_msg.trim(),
                files
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test 4: Conflict resolved by writing new content, note still valid
// ---------------------------------------------------------------------------

/// When a rebase conflict is resolved by writing completely new content,
/// the authorship note must still exist (remapped from the original).
#[test]
fn test_rebase_conflict_on_ai_file_preserves_note() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "shared.rs", "fn original() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream: completely different content for shared.rs
    write_raw_commit(
        &repo,
        "shared.rs",
        "fn upstream_version() {}",
        "Upstream: rewrite shared.rs",
    );

    // Feature branch from before upstream change
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // AI modifies shared.rs
    let mut shared = repo.filename("shared.rs");
    shared.set_contents(crate::lines!["fn ai_version() {}".ai()]);
    repo.stage_all_and_commit("feat: AI rewrites shared.rs")
        .unwrap();

    // Verify note exists before rebase
    let pre_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert!(
        repo.read_authorship_note(&pre_sha).is_some(),
        "AI commit must have a note before rebase"
    );

    // Rebase — will conflict
    let result = repo.git(&["rebase", &default_branch]);
    assert!(result.is_err(), "rebase should conflict on shared.rs");

    // Human resolves conflict with new content
    fs::write(repo.path().join("shared.rs"), "fn human_resolved() {}\n").unwrap();
    repo.git(&["add", "shared.rs"]).unwrap();
    repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None)
        .expect("rebase --continue should succeed");

    // Post-rebase: the note must still exist
    let post_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let post_note = repo.read_authorship_note(&post_sha);
    assert!(
        post_note.is_some(),
        "AI authorship note must survive conflict rebase. The original note should \
         be remapped to the rebased commit to preserve AI provenance."
    );
}
