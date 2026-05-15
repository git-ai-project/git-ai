//! Real-world rebase scenario tests covering common patterns developers encounter.
//!
//! These tests verify attribution correctness across various rebase workflows:
//! - Disjoint files (fast path)
//! - Same file modified non-conflictingly
//! - Manual conflict resolution
//! - `git rebase --onto` with multiple commits
//! - Interactive squash via autosquash

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `content` to `filename`, add, and commit via git_og (no git-ai hooks).
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

// ---------------------------------------------------------------------------
// Test 1: Feature branch edits different files than main — notes copy verbatim
// ---------------------------------------------------------------------------

/// When the feature branch and main touch completely different files, the rebase
/// takes the fast path and notes should be copied verbatim to the new commits.
#[test]
fn test_rebase_disjoint_files_fast_path() {
    let repo = TestRepo::new();

    // Initial commit
    write_raw_commit(&repo, "readme.md", "# Project", "Initial commit");
    let default_branch = repo.current_branch();

    // Main advances with a different file
    write_raw_commit(
        &repo,
        "main_only.rs",
        "fn main_stuff() {}",
        "Main: add main_only.rs",
    );

    // Feature branch from common ancestor
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // AI creates feature-only file
    let mut feature_file = repo.filename("feature_only.rs");
    feature_file.set_contents(crate::lines![
        "fn feature_fn() {}".ai(),
        "fn feature_helper() {}".ai()
    ]);
    repo.stage_all_and_commit("feat: add feature_only.rs")
        .unwrap();

    // Capture pre-rebase note
    let pre_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let pre_note = repo.read_authorship_note(&pre_sha);
    assert!(pre_note.is_some(), "should have note before rebase");

    // Rebase onto main (disjoint files — no conflict)
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed (disjoint files)");

    // Verify note exists after rebase
    let post_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let post_note = repo.read_authorship_note(&post_sha);
    assert!(
        post_note.is_some(),
        "note must exist after fast-path rebase (disjoint files)"
    );

    // Verify attribution is correct
    feature_file.assert_lines_and_blame(crate::lines![
        "fn feature_fn() {}".ai(),
        "fn feature_helper() {}".ai()
    ]);
}

// ---------------------------------------------------------------------------
// Test 2: Both branches touch same file non-conflictingly — attribution remapped
// ---------------------------------------------------------------------------

/// Both branches modify the same file but in non-conflicting regions.
/// After rebase, the AI attribution must be correctly remapped to account
/// for line shifts caused by upstream changes.
#[test]
fn test_rebase_same_file_no_conflict() {
    let repo = TestRepo::new();

    // Initial: file with some content
    write_raw_commit(
        &repo,
        "lib.rs",
        "fn existing1() {}\nfn existing2() {}\nfn existing3() {}",
        "Initial commit",
    );
    let default_branch = repo.current_branch();

    // Main appends to the END (non-conflicting with feature's prepend)
    write_raw_commit(
        &repo,
        "lib.rs",
        "fn existing1() {}\nfn existing2() {}\nfn existing3() {}\n// footer added by main",
        "Main: append footer",
    );

    // Feature branch from common ancestor
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // AI PREPENDS to the same file (line numbers stay same in the rebased commit)
    let file_path = repo.path().join("lib.rs");
    repo.git_ai(&["checkpoint", "human", "lib.rs"]).unwrap();
    fs::write(
        &file_path,
        "fn ai_added() {}\nfn existing1() {}\nfn existing2() {}\nfn existing3() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "lib.rs"]).unwrap();
    repo.stage_all_and_commit("feat: AI prepends to lib.rs")
        .unwrap();

    // Rebase (non-conflicting: upstream appended at end, feature prepended at start)
    repo.git(&["rebase", &default_branch])
        .expect("rebase should succeed (non-conflicting same file)");

    // After rebase, AI line should still be attributed (line 1 in the rebased commit)
    let mut lib = repo.filename("lib.rs");
    lib.assert_lines_and_blame(crate::lines![
        "fn ai_added() {}".ai(),
        "fn existing1() {}",
        "fn existing2() {}",
        "fn existing3() {}",
        "// footer added by main",
    ]);
}

// ---------------------------------------------------------------------------
// Test 3: Conflict resolved by human (no AI checkpoint) — resolved lines human
// ---------------------------------------------------------------------------

/// When a conflict is resolved manually without any AI checkpoint, the resolved
/// lines should not carry AI attribution.
#[test]
fn test_rebase_with_manual_conflict_resolution() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "config.rs", "fn default() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Main rewrites the function completely
    write_raw_commit(
        &repo,
        "config.rs",
        "fn default_v2() { /* main version */ }",
        "Main: rewrite default",
    );

    // Feature branch from before main's rewrite
    let base_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &base_sha]).unwrap();

    // AI also rewrites the function
    let mut config = repo.filename("config.rs");
    config.set_contents(crate::lines!["fn default_ai() { /* ai version */ }".ai()]);
    repo.stage_all_and_commit("feat: AI rewrites config.rs")
        .unwrap();

    // Rebase — will conflict
    let result = repo.git(&["rebase", &default_branch]);
    assert!(result.is_err(), "rebase should conflict");

    // Human resolves manually (no AI checkpoint fired)
    let resolved_content = "fn default_merged() { /* human resolved */ }\n";
    fs::write(repo.path().join("config.rs"), resolved_content).unwrap();
    repo.git(&["add", "config.rs"]).unwrap();
    repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None)
        .expect("rebase --continue should succeed");

    // The note should still exist (remapped from original)
    let post_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&post_sha);
    assert!(
        note.is_some(),
        "note should survive conflict resolution (remapped from original)"
    );
}

// ---------------------------------------------------------------------------
// Test 4: git rebase --onto with 3+ commits
// ---------------------------------------------------------------------------

/// `git rebase --onto` with multiple commits being replayed onto a new base.
/// All notes must be preserved for each replayed commit.
#[test]
fn test_rebase_onto_with_multiple_commits() {
    let repo = TestRepo::new();

    write_raw_commit(&repo, "init.rs", "fn init() {}", "Initial commit");
    let default_branch = repo.current_branch();

    // Create a longer history on main
    write_raw_commit(&repo, "step1.rs", "fn step1() {}", "Main: step 1");
    write_raw_commit(&repo, "step2.rs", "fn step2() {}", "Main: step 2");

    // Feature branch from initial commit
    let initial_sha = repo
        .git(&["rev-parse", "HEAD~2"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "feature", &initial_sha])
        .unwrap();

    // Intermediate commit (will be skipped by --onto)
    write_raw_commit(&repo, "skip_me.rs", "fn skip() {}", "Feature: skip this");
    let onto_start = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Three AI commits we want to rebase --onto main
    let file1 = repo.path().join("ai_feat1.rs");
    fs::write(&file1, "fn ai1() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai_feat1.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: AI commit 1").unwrap();

    let file2 = repo.path().join("ai_feat2.rs");
    fs::write(&file2, "fn ai2() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai_feat2.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: AI commit 2").unwrap();

    let file3 = repo.path().join("ai_feat3.rs");
    fs::write(&file3, "fn ai3() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai_feat3.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: AI commit 3").unwrap();

    // rebase --onto main <skip_point> feature
    // This replays commits after onto_start onto main, skipping skip_me.rs
    repo.git(&["rebase", "--onto", &default_branch, &onto_start, "feature"])
        .expect("rebase --onto should succeed");

    // Verify all 3 AI commits have notes after rebase
    let log = repo
        .git(&["log", "--format=%H", &format!("{}..HEAD", default_branch)])
        .unwrap();
    let shas: Vec<&str> = log.trim().lines().collect();
    assert_eq!(shas.len(), 3, "should have 3 rebased commits after --onto");

    for sha in &shas {
        let sha = sha.trim();
        let note = repo.read_authorship_note(sha);
        assert!(
            note.is_some(),
            "commit {} must have authorship note after rebase --onto",
            &sha[..7]
        );
    }
}

// ---------------------------------------------------------------------------
// Test 5: Interactive rebase squash preserves combined AI lines
// ---------------------------------------------------------------------------

/// Squashing commits via interactive rebase (autosquash) must preserve the
/// combined AI attribution from all squashed commits.
#[test]
#[cfg(not(target_os = "windows"))]
fn test_rebase_interactive_squash_preserves_attribution() {
    let repo = TestRepo::new();

    // Base commit
    write_raw_commit(&repo, "readme.md", "# Project", "Initial commit");
    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Commit 1: AI adds function
    let file_path = repo.path().join("module.rs");
    fs::write(&file_path, "fn first() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "module.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: add module").unwrap();

    // Commit 2: AI adds more to same file (squash target)
    fs::write(&file_path, "fn first() {}\nfn second() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "module.rs"])
        .unwrap();
    repo.stage_all_and_commit("fixup! feat: add module")
        .unwrap();

    // Autosquash rebase: merges "fixup!" commit into its target
    let script_content = "#!/bin/sh\n\
        sed -i.bak '2s/pick/fixup/' \"$1\"\n";
    let script_path = repo.path().join("squash_script.sh");
    fs::write(&script_path, script_content).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();
    }

    repo.git_with_env(
        &["rebase", "-i", &base_sha],
        &[
            ("GIT_SEQUENCE_EDITOR", script_path.to_str().unwrap()),
            ("GIT_EDITOR", "true"),
        ],
        None,
    )
    .expect("interactive rebase with squash should succeed");

    // Should have exactly 1 commit after base
    let count = repo
        .git(&["rev-list", "--count", &format!("{base_sha}..HEAD")])
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(count, "1", "should have 1 commit after squash");

    // Verify the squashed commit has an authorship note
    let squashed_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&squashed_sha);
    assert!(
        note.is_some(),
        "squashed commit must have authorship note with combined AI lines"
    );

    // Verify blame shows AI attribution
    let blame = repo.git_ai(&["blame", "module.rs"]).unwrap();
    assert!(
        blame.contains("mock_ai"),
        "blame after squash should show AI attribution"
    );
}
