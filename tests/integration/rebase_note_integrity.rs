/// Tests for intermediate-commit note integrity after rebase.
///
/// ## The Bug
///
/// `rewrite_authorship_after_rebase_v2` (src/authorship/rebase_authorship.rs) has a
/// slow-path processing loop that seeds `cached_file_attestation_text` and
/// `existing_files` from the **full cumulative state of the last pre-rebase commit**
/// (all commits in the chain combined). When it writes the note for an *intermediate*
/// new commit K, it emits ALL entries in `cached_file_attestation_text` that appear in
/// `existing_files`, which includes files introduced by commits K+1, K+2, … (future
/// commits).
///
/// Concrete example from PR #967 (5-commit chain):
///   - Commit f70ab45e (early – daemon changes only): note shows `revert_hooks.rs`
///     attributed, but revert_hooks.rs was first introduced in a LATER commit
///     (f1fdede4). Every intermediate commit ended up with the same large set of
///     file attestations as the tip commit, and `accepted_lines` counts became
///     non-monotonic (earlier commits showed higher counts than later ones).
///
/// ## When the slow path fires
///
/// The fast path (`try_fast_path_rebase_note_remap_cached`) just copies original
/// notes verbatim (only updating `base_commit_sha`) and is correct. It only fires
/// when the AI-touched file blobs are *identical* between old and new commits. If
/// *any* tracked file's blob changes after rebasing (e.g. the upstream prepended a
/// header to a file the feature also modifies), the fast path is skipped and the
/// buggy slow path runs.
///
/// ## Setup pattern used to force the slow path
///
/// Each test creates a `shared.rs` file (with a proper trailing newline, committed via
/// `git_og` to avoid the no-trailing-newline issue with `set_contents`). The upstream
/// branch prepends a header to `shared.rs`. The feature branch then APPENDS to
/// `shared.rs` via `set_contents` (which writes content without a trailing newline –
/// that's fine because git can merge "prepend on upstream" with "append on feature"
/// even when they have different trailing-newline styles).
///
/// After rebasing, the shared.rs blob in each feature commit differs from its
/// pre-rebase counterpart, so `tracked_paths_match_for_commit_pairs` returns false
/// → fast path is bypassed → the buggy slow path runs.
///
/// ## Expected vs broken behaviour
///
/// | Commit | Expected note files                | Broken note files (current)          |
/// |--------|------------------------------------|--------------------------------------|
/// | A′     | shared.rs + module_a.rs            | shared.rs + module_a.rs + module_b.rs ← LEAK |
/// | B′     | shared.rs + module_a.rs + module_b.rs | correct (it is the tip)           |
///
/// These tests are intentionally written to **FAIL** with the current (buggy) code
/// and to **PASS** once the bug is fixed.
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn total_accepted_lines(note: &str) -> u32 {
    let log = AuthorshipLog::deserialize_from_string(note)
        .expect("should parse authorship note as AuthorshipLog");
    log.metadata
        .prompts
        .values()
        .map(|p| p.accepted_lines)
        .sum()
}

fn files_in_note(note: &str) -> Vec<String> {
    let log = AuthorshipLog::deserialize_from_string(note)
        .expect("should parse authorship note as AuthorshipLog");
    log.attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect()
}

/// Write `content` to `filename` in the repo's working directory, add, and
/// commit via `git_og` (bypassing git-ai hooks). The content is written with
/// a trailing newline so that 3-way merges work correctly when the feature
/// branch later appends content via `set_contents` (which omits trailing
/// newlines).
fn write_raw_commit(repo: &TestRepo, filename: &str, content: &str, message: &str) {
    let path = repo.path().join(filename);
    // Ensure content ends with newline for clean 3-way merge behaviour
    let content_with_nl = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{}\n", content)
    };
    std::fs::write(&path, content_with_nl.as_bytes()).expect("write file");
    repo.git_og(&["add", filename]).expect("git add");
    repo.git_og(&["commit", "-m", message]).expect("git commit");
}

// ---------------------------------------------------------------------------
// Test 1: future-file attribution must not leak into earlier commit notes
// ---------------------------------------------------------------------------

/// After a rebase where the slow path fires (upstream prepended a line to
/// shared.rs, diverging blobs), commit A′'s note must NOT reference module_b.rs,
/// which was only introduced by commit B (a later commit).
///
/// Broken: the slow path seeds `cached_file_attestation_text` + `existing_files`
/// from the final pre-rebase state. module_b.rs is in that state (added by B),
/// so it leaks into every intermediate commit's note including A′.
#[test]
fn test_rebase_future_file_does_not_leak_into_earlier_commit_note() {
    let repo = TestRepo::new();

    // Initial commit: shared.rs with a proper trailing newline (via git_og).
    // Feature branch will APPEND lines; upstream will PREPEND.
    // 3-way merge: prepend (upstream) + append (feature) = non-conflicting.
    write_raw_commit(&repo, "shared.rs", "fn original() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream PREPENDS a header line to shared.rs.
    // After rebasing, every feature commit that touches shared.rs will have a
    // different blob OID → fast path cannot fire → slow path runs.
    write_raw_commit(
        &repo,
        "shared.rs",
        "// upstream header\nfn original() {}",
        "Upstream: prepend header to shared.rs",
    );

    // Feature branch starts from BEFORE the upstream commit.
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // Commit A: appends AI lines to shared.rs + creates module_a.rs.
    // module_b.rs does NOT exist at this point.
    let mut shared = repo.filename("shared.rs");
    shared.set_contents(crate::lines![
        "fn original() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai()
    ]);
    let mut module_a = repo.filename("module_a.rs");
    module_a.set_contents(crate::lines!["fn ma() {}".ai()]);
    repo.stage_all_and_commit("Commit A: shared (append) + module_a.rs")
        .unwrap();

    // Commit B: appends more AI lines to shared.rs + creates module_b.rs.
    shared.set_contents(crate::lines![
        "fn original() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn b1() {}".ai(),
        "fn b2() {}".ai()
    ]);
    let mut module_b = repo.filename("module_b.rs");
    module_b.set_contents(crate::lines!["fn mb1() {}".ai(), "fn mb2() {}".ai()]);
    repo.stage_all_and_commit("Commit B: shared (append) + module_b.rs")
        .unwrap();

    // Rebase feature onto the advanced main branch (non-conflicting).
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let new_sha_b = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let new_sha_a = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note_a = repo
        .read_authorship_note(&new_sha_a)
        .expect("commit A′ must have an authorship note after rebase");
    let note_b = repo
        .read_authorship_note(&new_sha_b)
        .expect("commit B′ must have an authorship note after rebase");

    let files_a = files_in_note(&note_a);
    let files_b = files_in_note(&note_b);

    // -----------------------------------------------------------------------
    // Core assertion: module_b.rs was introduced in commit B (AFTER commit A).
    // Commit A′'s note must NOT reference module_b.rs.
    // With the slow-path bug, the cache is pre-seeded from the final pre-rebase
    // state which already includes module_b.rs → it leaks into A′'s note.
    // -----------------------------------------------------------------------
    assert!(
        !files_a.iter().any(|f| f.contains("module_b")),
        "REBASE NOTE CORRUPTION (slow-path future-file leak): \
         Commit A′'s note contains 'module_b.rs', but module_b.rs was only \
         introduced in commit B (a later commit). \
         The slow path seeds cached_file_attestation_text from the full \
         pre-rebase HEAD state, causing future files to appear in earlier \
         commit notes. Files found in A′'s note: {:?}",
        files_a
    );

    // Sanity: A′ should reference the files A actually introduced.
    assert!(
        files_a
            .iter()
            .any(|f| f.contains("module_a") || f.contains("shared")),
        "Commit A′'s note should contain module_a.rs or shared.rs, \
         but found: {:?}",
        files_a
    );

    // Sanity: B′ (tip) must include module_b.rs.
    assert!(
        files_b.iter().any(|f| f.contains("module_b")),
        "Commit B′'s note should contain module_b.rs, but found: {:?}",
        files_b
    );
}

// ---------------------------------------------------------------------------
// Test 2: accepted_lines must not be inflated for intermediate commits
// ---------------------------------------------------------------------------

/// Two-commit feature branch where both commits append to a shared file that
/// the upstream prepended to (forcing the slow path without conflicts).
///
/// Commit 1 adds exactly 10 AI lines. Commit 2 adds 10 more. After rebase,
/// commit 1′'s `accepted_lines` should reflect only its own ~10 lines, not
/// the full-chain total of ~20.
///
/// Broken: the slow path writes the full-chain `accepted_lines` to every
/// intermediate commit because `current_attributions` starts at the final
/// pre-rebase state and is never rewound to the per-commit checkpoint.
#[test]
fn test_rebase_intermediate_commit_accepted_lines_not_inflated() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "impl.rs", "fn base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends to impl.rs (diverges blobs → forces slow path).
    write_raw_commit(
        &repo,
        "impl.rs",
        "// upstream header\nfn base() {}",
        "Upstream: prepend to impl.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut shared = repo.filename("impl.rs");

    // Commit 1: appends exactly 10 AI lines to impl.rs.
    shared.set_contents(crate::lines![
        "fn base() {}",
        "fn c01() {}".ai(),
        "fn c02() {}".ai(),
        "fn c03() {}".ai(),
        "fn c04() {}".ai(),
        "fn c05() {}".ai(),
        "fn c06() {}".ai(),
        "fn c07() {}".ai(),
        "fn c08() {}".ai(),
        "fn c09() {}".ai(),
        "fn c10() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit 1: 10 AI lines appended to impl.rs")
        .unwrap();

    // Commit 2: appends 10 more AI lines to impl.rs.
    shared.set_contents(crate::lines![
        "fn base() {}",
        "fn c01() {}".ai(),
        "fn c02() {}".ai(),
        "fn c03() {}".ai(),
        "fn c04() {}".ai(),
        "fn c05() {}".ai(),
        "fn c06() {}".ai(),
        "fn c07() {}".ai(),
        "fn c08() {}".ai(),
        "fn c09() {}".ai(),
        "fn c10() {}".ai(),
        "fn c11() {}".ai(),
        "fn c12() {}".ai(),
        "fn c13() {}".ai(),
        "fn c14() {}".ai(),
        "fn c15() {}".ai(),
        "fn c16() {}".ai(),
        "fn c17() {}".ai(),
        "fn c18() {}".ai(),
        "fn c19() {}".ai(),
        "fn c20() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit 2: 10 more AI lines appended to impl.rs")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let new_sha2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let new_sha1 = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note1 = repo
        .read_authorship_note(&new_sha1)
        .expect("commit 1′ should have an authorship note");
    let note2 = repo
        .read_authorship_note(&new_sha2)
        .expect("commit 2′ should have an authorship note");

    let lines1 = total_accepted_lines(&note1);
    let lines2 = total_accepted_lines(&note2);

    // Commit 1 introduced 10 AI lines; commit 2 introduced 10 more.
    // Commit 1′ should report ~10 accepted_lines, not ~20.
    assert!(
        lines1 < lines2,
        "REBASE NOTE CORRUPTION (slow-path inflated accepted_lines): \
         commit 1′ has accepted_lines={} and commit 2′ has accepted_lines={}. \
         Commit 1 introduced only 10 AI lines while commit 2 adds 10 more, so \
         1′ should have a strictly smaller count than 2′.\n\
         If equal: the slow path wrote the full-chain total to every intermediate \
         commit instead of the per-commit share.",
        lines1,
        lines2
    );

    // Tighter bound: commit 1 should report approximately 10 lines.
    assert!(
        lines1 <= 12,
        "REBASE NOTE CORRUPTION: commit 1′ reports accepted_lines={} but \
         should be ~10 (it only introduced 10 AI lines). A value significantly \
         above 10 means the note was seeded from the full 20-line pre-rebase state.",
        lines1
    );

    // Per-commit-delta: commit 2′ reports only its OWN new 10 lines, not the
    // cumulative 20.  The primary invariant is that 1′ is smaller than 2′ (or
    // at most equal), verified above.  Accept any reasonable per-commit value.
    assert!(
        lines2 >= 8,
        "Commit 2′ introduced 10 new AI lines; expected accepted_lines ~10, got {}.",
        lines2
    );
}

// ---------------------------------------------------------------------------
// Test 3: three-commit chain — no future-file leakage (slow path forced)
// ---------------------------------------------------------------------------

/// Three-commit feature branch, each appending to a shared file that upstream
/// prepended to (forcing slow path). Each commit also adds its own unique file.
///
/// After rebase:
///   - A′ note: must NOT contain unit_b.rs or unit_c.rs (future files)
///   - B′ note: must NOT contain unit_c.rs (future file)
///   - C′ note: tip commit, no future-leak concern
#[test]
fn test_rebase_three_commits_no_future_file_leakage() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "core.rs", "fn core_base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends to core.rs → forces slow path.
    write_raw_commit(
        &repo,
        "core.rs",
        "// upstream\nfn core_base() {}",
        "Upstream: prepend to core.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut shared = repo.filename("core.rs");

    // Commit A: appends to core.rs + adds unit_a.rs.
    shared.set_contents(crate::lines!["fn core_base() {}", "fn core_a() {}".ai()]);
    let mut unit_a = repo.filename("unit_a.rs");
    unit_a.set_contents(crate::lines!["fn ua() {}".ai()]);
    repo.stage_all_and_commit("Commit A: core + unit_a")
        .unwrap();

    // Commit B: appends to core.rs + adds unit_b.rs.
    shared.set_contents(crate::lines![
        "fn core_base() {}",
        "fn core_a() {}".ai(),
        "fn core_b() {}".ai()
    ]);
    let mut unit_b = repo.filename("unit_b.rs");
    unit_b.set_contents(crate::lines!["fn ub() {}".ai()]);
    repo.stage_all_and_commit("Commit B: core + unit_b")
        .unwrap();

    // Commit C: appends to core.rs + adds unit_c.rs.
    shared.set_contents(crate::lines![
        "fn core_base() {}",
        "fn core_a() {}".ai(),
        "fn core_b() {}".ai(),
        "fn core_c() {}".ai()
    ]);
    let mut unit_c = repo.filename("unit_c.rs");
    unit_c.set_contents(crate::lines!["fn uc() {}".ai()]);
    repo.stage_all_and_commit("Commit C: core + unit_c")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let _sha_c = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let sha_b = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    let sha_a = repo
        .git(&["rev-parse", "HEAD~2"])
        .unwrap()
        .trim()
        .to_string();

    let note_a = repo
        .read_authorship_note(&sha_a)
        .expect("commit A′ should have note");
    let note_b = repo
        .read_authorship_note(&sha_b)
        .expect("commit B′ should have note");

    let files_a = files_in_note(&note_a);
    let files_b = files_in_note(&note_b);

    // A′ must not reference unit_b.rs or unit_c.rs (future files).
    assert!(
        !files_a.iter().any(|f| f.contains("unit_b")),
        "REBASE NOTE CORRUPTION: commit A′'s note contains 'unit_b.rs', \
         which was only introduced in commit B (a later commit). \
         Files in A′: {:?}",
        files_a
    );
    assert!(
        !files_a.iter().any(|f| f.contains("unit_c")),
        "REBASE NOTE CORRUPTION: commit A′'s note contains 'unit_c.rs', \
         which was only introduced in commit C (a later commit). \
         Files in A′: {:?}",
        files_a
    );

    // B′ must not reference unit_c.rs (future file relative to B).
    assert!(
        !files_b.iter().any(|f| f.contains("unit_c")),
        "REBASE NOTE CORRUPTION: commit B′'s note contains 'unit_c.rs', \
         which was only introduced in commit C (a later commit). \
         Files in B′: {:?}",
        files_b
    );

    // Sanity: A′ should reference what A actually introduced.
    assert!(
        files_a
            .iter()
            .any(|f| f.contains("unit_a") || f.contains("core")),
        "Commit A′ should reference unit_a.rs or core.rs, but found: {:?}",
        files_a
    );
}

// ---------------------------------------------------------------------------
// Test 4: deleted file must not reappear in later commit notes (slow path)
// ---------------------------------------------------------------------------

/// Commit A appends to shared file + adds temp.rs.
/// Commit B appends to shared file + deletes temp.rs + adds final.rs.
/// Commit C appends to shared file + adds extra.rs.
/// Upstream prepends to shared file (forces slow path).
///
/// After rebase:
///   - B′ note: must NOT contain temp.rs (it was deleted in B)
///   - B′ note: must NOT contain extra.rs (introduced in future commit C)
///   - C′ note: must NOT contain temp.rs (deleted before C ever ran)
#[test]
fn test_rebase_deleted_file_does_not_persist_in_later_notes() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "engine.rs", "fn engine_base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends to engine.rs → forces slow path.
    write_raw_commit(
        &repo,
        "engine.rs",
        "// upstream\nfn engine_base() {}",
        "Upstream: prepend to engine.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut engine = repo.filename("engine.rs");

    // Commit A: appends to engine.rs + adds temp.rs.
    engine.set_contents(crate::lines!["fn engine_base() {}", "fn eng_a() {}".ai()]);
    let mut temp = repo.filename("temp.rs");
    temp.set_contents(crate::lines!["fn tmp1() {}".ai(), "fn tmp2() {}".ai()]);
    repo.stage_all_and_commit("Commit A: engine + temp.rs")
        .unwrap();

    // Commit B: appends to engine.rs + removes temp.rs + adds final.rs.
    engine.set_contents(crate::lines![
        "fn engine_base() {}",
        "fn eng_a() {}".ai(),
        "fn eng_b() {}".ai()
    ]);
    repo.git(&["rm", "temp.rs"]).unwrap();
    let mut final_rs = repo.filename("final.rs");
    final_rs.set_contents(crate::lines!["fn fin() {}".ai()]);
    repo.stage_all_and_commit("Commit B: engine + rm temp.rs + final.rs")
        .unwrap();

    // Commit C: appends to engine.rs + adds extra.rs.
    engine.set_contents(crate::lines![
        "fn engine_base() {}",
        "fn eng_a() {}".ai(),
        "fn eng_b() {}".ai(),
        "fn eng_c() {}".ai()
    ]);
    let mut extra = repo.filename("extra.rs");
    extra.set_contents(crate::lines!["fn ex() {}".ai()]);
    repo.stage_all_and_commit("Commit C: engine + extra.rs")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let sha_c = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let sha_b = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note_b = repo
        .read_authorship_note(&sha_b)
        .expect("commit B′ should have note");
    let note_c = repo
        .read_authorship_note(&sha_c)
        .expect("commit C′ should have note");

    let files_b = files_in_note(&note_b);
    let files_c = files_in_note(&note_c);

    // B′: temp.rs was deleted in B — must not appear.
    assert!(
        !files_b.iter().any(|f| f.contains("temp")),
        "REBASE NOTE CORRUPTION: commit B′ contains 'temp.rs', which was \
         deleted in commit B. files_b: {:?}",
        files_b
    );

    // B′: extra.rs was introduced in commit C (future) — must not appear.
    assert!(
        !files_b.iter().any(|f| f.contains("extra")),
        "REBASE NOTE CORRUPTION: commit B′ contains 'extra.rs', which was \
         only introduced in commit C (a later commit). files_b: {:?}",
        files_b
    );

    // C′: temp.rs was deleted in B (before C) — must not appear in C.
    assert!(
        !files_c.iter().any(|f| f.contains("temp")),
        "REBASE NOTE CORRUPTION: commit C′ contains 'temp.rs', which was \
         deleted in commit B (before C). files_c: {:?}",
        files_c
    );

    // Sanity: final.rs must appear in B′ (B introduced it).
    assert!(
        files_b.iter().any(|f| f.contains("final")),
        "Commit B′ should contain final.rs, but found: {:?}",
        files_b
    );
    // Per-commit-delta: C′ only contains files that C itself touched (engine.rs, extra.rs).
    // final.rs was introduced in B, not C, so it must NOT appear in C′.
    assert!(
        !files_c.iter().any(|f| f.contains("final")),
        "Commit C′ should NOT contain final.rs (per-commit-delta: C didn't touch it), \
         but found: {:?}",
        files_c
    );
}

// ---------------------------------------------------------------------------
// Test 5: line-level blame + accepted_lines correctness after slow-path rebase
// ---------------------------------------------------------------------------

/// After a slow-path rebase the per-line AI blame attribution must be correct
/// and `accepted_lines` for an intermediate commit must be strictly less than
/// for the tip commit.
///
/// Two-commit chain, both appending to a file whose upstream prepended a line.
///   - A′ note: must NOT contain separate.rs (introduced by B)
///   - accepted_lines for A′ < accepted_lines for B′
///   - Line-level blame on main.rs reflects the expected attribution
#[test]
fn test_rebase_slow_path_line_attribution_is_correct() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "main.rs", "fn original() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends to main.rs → forces slow path.
    write_raw_commit(
        &repo,
        "main.rs",
        "// upstream\nfn original() {}",
        "Upstream: prepend to main.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut main_rs = repo.filename("main.rs");

    // Commit A: appends 3 AI lines to main.rs (no separate.rs yet).
    main_rs.set_contents(crate::lines![
        "fn original() {}",
        "fn ai_a1() {}".ai(),
        "fn ai_a2() {}".ai(),
        "fn ai_a3() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit A: 3 AI lines").unwrap();

    // Commit B: appends 3 more AI lines to main.rs + adds separate.rs.
    main_rs.set_contents(crate::lines![
        "fn original() {}",
        "fn ai_a1() {}".ai(),
        "fn ai_a2() {}".ai(),
        "fn ai_a3() {}".ai(),
        "fn ai_b1() {}".ai(),
        "fn ai_b2() {}".ai(),
        "fn ai_b3() {}".ai()
    ]);
    let mut separate = repo.filename("separate.rs");
    separate.set_contents(crate::lines!["fn sep() {}".ai()]);
    repo.stage_all_and_commit("Commit B: 3 more AI lines + separate.rs")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let sha_b = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let sha_a = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note_a = repo
        .read_authorship_note(&sha_a)
        .expect("commit A′ should have a note");
    let note_b = repo
        .read_authorship_note(&sha_b)
        .expect("commit B′ should have a note");

    let files_a = files_in_note(&note_a);

    // A′: separate.rs was introduced in B (future) — must not appear in A.
    assert!(
        !files_a.iter().any(|f| f.contains("separate")),
        "REBASE NOTE CORRUPTION: commit A′'s note contains 'separate.rs', \
         which was only introduced in commit B. Files in A′: {:?}",
        files_a
    );

    // accepted_lines for A′ (3 lines) must be less than B′ (7 lines).
    let lines_a = total_accepted_lines(&note_a);
    let lines_b = total_accepted_lines(&note_b);
    assert!(
        lines_a < lines_b,
        "REBASE NOTE CORRUPTION: commit A′ has accepted_lines={} and \
         commit B′ has accepted_lines={}. A′ came before B′ and introduced \
         fewer AI lines, so A′ should have a strictly smaller count.",
        lines_a,
        lines_b
    );

    // Verify line-level blame reflects the upstream header + AI appended lines.
    main_rs.assert_lines_and_blame(crate::lines![
        "// upstream",
        "fn original() {}",
        "fn ai_a1() {}".ai(),
        "fn ai_a2() {}".ai(),
        "fn ai_a3() {}".ai(),
        "fn ai_b1() {}".ai(),
        "fn ai_b2() {}".ai(),
        "fn ai_b3() {}".ai()
    ]);
}

// ---------------------------------------------------------------------------
// Test 6: AI lines newly added in commit K≥2 must not be attributed as human
// ---------------------------------------------------------------------------

/// The hunk-based path (used for all new commits after the first content-diff)
/// only carries EXISTING attributions forward by shifting line numbers. It does
/// NOT stamp newly-inserted lines with attribution. This means any AI line that
/// is "new" in commit B relative to commit A (i.e., was inserted in the A′→B′ diff)
/// will appear as human in B′'s blame, even though the original commit B had it
/// 100% AI.
///
/// Setup:
///   - Upstream prepends `// upstream` to shared.rs (forces slow path for all commits)
///   - Commit A: appends `fn ai_a()` to shared.rs (AI)
///   - Commit B: appends `fn ai_b()` to shared.rs (AI) — this line is "inserted" in A′→B′
///
/// After rebase:
///   - A′ is processed via content-diff (first commit, correct)
///   - B′ is processed via hunk-based path
///     → hunk-based path shifts fn_ai_a's attribution (line offset +0) ✓
///     → hunk-based path sees fn_ai_b as an "inserted" line → assigns NO attribution ✗
///   - `git ai diff B′` (blame) shows fn_ai_b as human even though it's 100% AI
#[test]
fn test_rebase_hunk_path_does_not_drop_ai_attribution_for_new_lines() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "shared.rs", "fn original() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends — forces slow path for all feature commits.
    write_raw_commit(
        &repo,
        "shared.rs",
        "// upstream\nfn original() {}",
        "Upstream: prepend to shared.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut shared = repo.filename("shared.rs");

    // Commit A: appends fn_ai_a (AI).
    shared.set_contents(crate::lines!["fn original() {}", "fn ai_a() {}".ai()]);
    repo.stage_all_and_commit("Commit A: fn ai_a").unwrap();

    // Commit B: appends fn_ai_b (AI).
    // After rebase, B′'s diff vs A′ shows fn_ai_b as an "inserted" line.
    // The hunk-based path has no way to stamp inserted lines as AI → bug.
    shared.set_contents(crate::lines![
        "fn original() {}",
        "fn ai_a() {}".ai(),
        "fn ai_b() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit B: fn ai_b").unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    // After rebase, shared.rs at HEAD (B′) should have:
    //   line 1: // upstream  ← human (added by upstream)
    //   line 2: fn original() {}  ← human
    //   line 3: fn ai_a() {}  ← AI (from commit A, correctly preserved)
    //   line 4: fn ai_b() {}  ← AI (from commit B, DROPPED by hunk-based path)
    //
    // The assert_lines_and_blame call tests the per-line blame attribution.
    // If fn ai_b is attributed as human, this will fail with the right message.
    shared.assert_lines_and_blame(crate::lines![
        "// upstream",
        "fn original() {}",
        "fn ai_a() {}".ai(),
        "fn ai_b() {}".ai() // BUG: hunk-based path drops this → shown as human
    ]);
}

// ---------------------------------------------------------------------------
// Test 7: attribution loss for the second-commit's per-note accepted_lines
// ---------------------------------------------------------------------------

/// Stronger variant: verify via note inspection that B′'s note attributes
/// fn_ai_b as AI. The blame test above checks line-level; this checks the
/// stored note directly. Both fail with the current buggy code.
#[test]
fn test_rebase_second_commit_note_attributes_its_own_ai_lines() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "work.rs", "fn base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Upstream prepends → slow path for all commits.
    write_raw_commit(
        &repo,
        "work.rs",
        "// header\nfn base() {}",
        "Upstream: prepend to work.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut work = repo.filename("work.rs");

    // Commit A: 3 AI lines.
    work.set_contents(crate::lines![
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn a3() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit A: 3 AI lines").unwrap();

    // Commit B: 3 more AI lines (different functions so there's no overlap with A).
    work.set_contents(crate::lines![
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn a3() {}".ai(),
        "fn b1() {}".ai(),
        "fn b2() {}".ai(),
        "fn b3() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit B: 3 more AI lines")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    let sha_b = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let sha_a = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();

    let note_a = repo
        .read_authorship_note(&sha_a)
        .expect("commit A′ should have a note");
    let note_b = repo
        .read_authorship_note(&sha_b)
        .expect("commit B′ should have a note");

    let lines_a = total_accepted_lines(&note_a);
    let lines_b = total_accepted_lines(&note_b);

    // A′ introduced 3 AI lines; after content-diff, its note should report ~3.
    // B′ introduced 3 more AI lines; its note should report ~6 (cumulative up to B).
    // With the hunk-path bug: B′'s note only has A's 3 lines in it (b1..b3 are
    // "inserted" and get no attribution) → lines_b ≈ lines_a, not lines_a + 3.
    assert!(
        lines_b > lines_a,
        "REBASE ATTRIBUTION LOSS (hunk-path drops inserted AI lines): \
         commit A′ accepted_lines={}, commit B′ accepted_lines={}. \
         B introduced 3 new AI lines (fn b1..fn b3) on top of A's 3. \
         B′ should report more accepted_lines than A′, but the hunk-based path \
         treats the newly-inserted fn b1..fn b3 as unattributed inserts, \
         so B′'s note ends up with the same (or fewer) accepted_lines as A′.",
        lines_a,
        lines_b
    );

    // Per-commit-delta: B′ reports only its own 3 new lines (not cumulative A+B=6).
    // The key invariant is that B′ > A′ (verified above), meaning B's new lines
    // were not silently dropped.  Accept any value >= 2 as a reasonable delta.
    assert!(
        lines_b >= 2,
        "REBASE ATTRIBUTION LOSS: commit B′ should have some accepted_lines \
         (it introduced 3 new AI lines), but got {}.",
        lines_b
    );
}

// ---------------------------------------------------------------------------
// Test 8: three-commit chain — attribution loss compounds across commits
// ---------------------------------------------------------------------------

/// Three-commit feature chain (A, B, C) each appending to the same file.
/// Upstream prepends (forces slow path). Only commit A′ is processed via
/// content-diff; B′ and C′ use the hunk-based path.
///
/// Expected: each commit's note includes ONLY its own newly-added AI lines.
/// Broken: B′ and C′ notes don't include their own new AI lines at all
/// (they only retain A's lines shifted by offset).
#[test]
fn test_rebase_attribution_loss_compounds_across_three_commits() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "lib.rs", "fn base() {}", "Initial commit");
    let default_branch = repo.current_branch();

    write_raw_commit(
        &repo,
        "lib.rs",
        "// upstream\nfn base() {}",
        "Upstream: prepend to lib.rs",
    );

    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    let mut lib = repo.filename("lib.rs");

    // Commit A: adds fn_a (2 AI lines).
    lib.set_contents(crate::lines![
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit A: 2 AI lines").unwrap();

    // Commit B: adds fn_b (2 AI lines on top of A).
    lib.set_contents(crate::lines![
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn b1() {}".ai(),
        "fn b2() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit B: 2 more AI lines")
        .unwrap();

    // Commit C: adds fn_c (2 AI lines on top of B).
    lib.set_contents(crate::lines![
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn b1() {}".ai(),
        "fn b2() {}".ai(),
        "fn c1() {}".ai(),
        "fn c2() {}".ai()
    ]);
    repo.stage_all_and_commit("Commit C: 2 more AI lines")
        .unwrap();

    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed without conflicts");

    // Line-level blame at HEAD (C′) — all 6 AI lines should be attributed as AI.
    lib.assert_lines_and_blame(crate::lines![
        "// upstream",
        "fn base() {}",
        "fn a1() {}".ai(),
        "fn a2() {}".ai(),
        "fn b1() {}".ai(), // BUG: hunk path drops this in B′ processing
        "fn b2() {}".ai(), // BUG: hunk path drops this in B′ processing
        "fn c1() {}".ai(), // BUG: hunk path drops this in C′ processing
        "fn c2() {}".ai()  // BUG: hunk path drops this in C′ processing
    ]);
}

crate::reuse_tests_in_worktree!(
    test_rebase_future_file_does_not_leak_into_earlier_commit_note,
    test_rebase_intermediate_commit_accepted_lines_not_inflated,
    test_rebase_three_commits_no_future_file_leakage,
    test_rebase_deleted_file_does_not_persist_in_later_notes,
    test_rebase_slow_path_line_attribution_is_correct,
    test_rebase_hunk_path_does_not_drop_ai_attribution_for_new_lines,
    test_rebase_second_commit_note_attributes_its_own_ai_lines,
    test_rebase_attribution_loss_compounds_across_three_commits,
);
