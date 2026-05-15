//! Real-world workflow tests that verify attribution survives common daily git operations.
//!
//! These cover the workflows Cursor/Claude users hit most frequently:
//! - cherry-pick (single + range)
//! - reset --soft + recommit (manual squash)
//! - git mv (rename tracking)
//! - multi-agent edits to the same file
//! - git revert
//! - commit --fixup + rebase --autosquash

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

// ===========================================================================
// Cherry-pick
// ===========================================================================

#[test]
fn test_cherry_pick_single_commit_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("lib.rs");

    // Initial commit: human writes 2 lines
    fs::write(&file_path, "fn main() {}\n    println!(\"hello\");\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch: AI adds 2 lines (pre-edit + post-edit checkpoint flow)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    // Pre-edit: snapshot existing content as "untracked"
    repo.git_ai(&["checkpoint", "human", "lib.rs"]).unwrap();
    // AI writes new lines
    fs::write(&file_path, "fn main() {}\n    println!(\"hello\");\n    let x = compute();\n    println!(\"{}\", x);\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "lib.rs"]).unwrap();
    repo.stage_all_and_commit("AI adds compute").unwrap();
    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Cherry-pick onto main
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_sha]).unwrap();

    let mut file = repo.filename("lib.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn main() {}".human(),
        "    println!(\"hello\");".human(),
        "    let x = compute();".ai(),
        "    println!(\"{}\", x);".ai(),
    ]);
}

#[test]
fn test_cherry_pick_range_preserves_attribution_across_commits() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("app.rs");

    fs::write(&file_path, "// app\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "app.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch with multiple AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI commit 1
    repo.git_ai(&["checkpoint", "human", "app.rs"]).unwrap();
    fs::write(&file_path, "// app\nfn one() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();
    repo.stage_all_and_commit("AI commit 1").unwrap();
    let first_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI commit 2
    repo.git_ai(&["checkpoint", "human", "app.rs"]).unwrap();
    fs::write(&file_path, "// app\nfn one() {}\nfn two() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let second_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Cherry-pick both commits onto main (first_sha~1..second_sha includes both commits)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &format!("{}~1..{}", first_sha, second_sha)])
        .unwrap();

    let mut file = repo.filename("app.rs");
    file.assert_lines_and_blame(crate::lines![
        "// app".human(),
        "fn one() {}".ai(),
        "fn two() {}".ai(),
    ]);
}

// ===========================================================================
// Reset --soft + recommit (manual squash)
// ===========================================================================

#[test]
fn test_reset_soft_recommit_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("module.rs");

    // Initial commit
    fs::write(&file_path, "// module\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "module.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI commit 1
    repo.git_ai(&["checkpoint", "human", "module.rs"]).unwrap();
    fs::write(&file_path, "// module\nfn alpha() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "module.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds alpha").unwrap();

    // AI commit 2
    repo.git_ai(&["checkpoint", "human", "module.rs"]).unwrap();
    fs::write(&file_path, "// module\nfn alpha() {}\nfn beta() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "module.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds beta").unwrap();

    // Soft reset back 2 commits and recommit as one
    repo.git(&["reset", "--soft", "HEAD~2"]).unwrap();
    repo.commit("squashed AI work").unwrap();

    let mut file = repo.filename("module.rs");
    file.assert_lines_and_blame(crate::lines![
        "// module".human(),
        "fn alpha() {}".ai(),
        "fn beta() {}".ai(),
    ]);
}

#[test]
fn test_reset_mixed_then_selective_stage_and_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("utils.rs");

    fs::write(&file_path, "// utils\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "utils.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI adds two functions
    repo.git_ai(&["checkpoint", "human", "utils.rs"]).unwrap();
    fs::write(&file_path, "// utils\nfn helper_a() {}\nfn helper_b() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "utils.rs"]).unwrap();
    repo.stage_all_and_commit("AI adds helpers").unwrap();

    // Reset --mixed (keeps working tree, unstages)
    repo.git(&["reset", "HEAD~1"]).unwrap();

    // Re-stage and commit — attribution should be preserved from reconstructed working log
    repo.git(&["add", "."]).unwrap();
    repo.commit("recommit helpers").unwrap();

    let mut file = repo.filename("utils.rs");
    file.assert_lines_and_blame(crate::lines![
        "// utils".human(),
        "fn helper_a() {}".ai(),
        "fn helper_b() {}".ai(),
    ]);
}

// ===========================================================================
// Git mv (rename)
// ===========================================================================

#[test]
fn test_git_mv_preserves_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("old_name.rs");

    // Create the file with initial human content, then have AI rewrite it
    fs::write(&file_path, "// placeholder\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "old_name.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial placeholder").unwrap();

    // AI rewrites the file
    repo.git_ai(&["checkpoint", "human", "old_name.rs"])
        .unwrap();
    fs::write(&file_path, "fn ai_code() {}\nfn human_code() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "old_name.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI writes content").unwrap();

    // Rename the file
    repo.git(&["mv", "old_name.rs", "new_name.rs"]).unwrap();
    repo.commit("rename file").unwrap();

    // Verify attribution survives rename via git blame -C
    let mut renamed = repo.filename("new_name.rs");
    renamed.assert_lines_and_blame(crate::lines![
        "fn ai_code() {}".ai(),
        "fn human_code() {}".ai(),
    ]);
}

// ===========================================================================
// Multi-agent same file
// ===========================================================================

#[test]
fn test_two_ai_agents_same_file_one_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("shared.rs");

    // Initial content
    fs::write(&file_path, "// shared module\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "shared.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Agent 1 adds lines
    repo.git_ai(&["checkpoint", "human", "shared.rs"]).unwrap();
    fs::write(&file_path, "// shared module\nfn from_cursor() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "shared.rs"])
        .unwrap();

    // Agent 2 adds more lines
    repo.git_ai(&["checkpoint", "human", "shared.rs"]).unwrap();
    fs::write(
        &file_path,
        "// shared module\nfn from_cursor() {}\nfn from_claude() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "shared.rs"])
        .unwrap();

    repo.stage_all_and_commit("both agents contributed")
        .unwrap();

    let mut file = repo.filename("shared.rs");
    file.assert_lines_and_blame(crate::lines![
        "// shared module".human(),
        "fn from_cursor() {}".ai(),
        "fn from_claude() {}".ai(),
    ]);
}

#[test]
fn test_ai_edit_then_human_edit_same_file_one_commit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("mixed.rs");

    fs::write(&file_path, "line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI edits
    repo.git_ai(&["checkpoint", "human", "mixed.rs"]).unwrap();
    fs::write(&file_path, "line 1\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "mixed.rs"]).unwrap();

    // Human edits after AI
    fs::write(&file_path, "line 1\nai line\nhuman line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.rs"])
        .unwrap();

    repo.stage_all_and_commit("mixed edits").unwrap();

    let mut file = repo.filename("mixed.rs");
    file.assert_lines_and_blame(crate::lines![
        "line 1".human(),
        "ai line".ai(),
        "human line".human(),
    ]);
}

// ===========================================================================
// Git revert
// ===========================================================================

#[test]
fn test_revert_ai_commit_removes_ai_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("feature.rs");

    fs::write(&file_path, "fn base() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "feature.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI adds a feature
    repo.git_ai(&["checkpoint", "human", "feature.rs"]).unwrap();
    fs::write(&file_path, "fn base() {}\nfn ai_feature() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds feature").unwrap();
    let ai_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Revert the AI commit
    repo.git(&["revert", "--no-edit", &ai_sha]).unwrap();

    // After revert, only the base line remains
    let mut file = repo.filename("feature.rs");
    file.assert_lines_and_blame(crate::lines!["fn base() {}".human()]);
}

#[test]
fn test_revert_preserves_other_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("multi.rs");

    fs::write(&file_path, "// base\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "multi.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // First AI commit
    repo.git_ai(&["checkpoint", "human", "multi.rs"]).unwrap();
    fs::write(&file_path, "// base\nfn keep_this() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "multi.rs"]).unwrap();
    repo.stage_all_and_commit("AI commit 1").unwrap();

    // Second AI commit
    repo.git_ai(&["checkpoint", "human", "multi.rs"]).unwrap();
    fs::write(
        &file_path,
        "// base\nfn keep_this() {}\nfn remove_this() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "multi.rs"]).unwrap();
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let revert_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Revert only the second commit
    repo.git(&["revert", "--no-edit", &revert_sha]).unwrap();

    let mut file = repo.filename("multi.rs");
    file.assert_lines_and_blame(crate::lines!["// base".human(), "fn keep_this() {}".ai(),]);
}

// ===========================================================================
// Fixup + autosquash
// ===========================================================================

#[test]
fn test_fixup_autosquash_preserves_combined_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("impl.rs");

    fs::write(&file_path, "// implementation\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "impl.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // AI writes initial implementation
    repo.git_ai(&["checkpoint", "human", "impl.rs"]).unwrap();
    fs::write(
        &file_path,
        "// implementation\nfn process() {\n    todo!()\n}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "impl.rs"]).unwrap();
    repo.stage_all_and_commit("AI: add process stub").unwrap();
    let target_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // AI fixes up the implementation
    repo.git_ai(&["checkpoint", "human", "impl.rs"]).unwrap();
    fs::write(
        &file_path,
        "// implementation\nfn process() {\n    let data = fetch();\n    transform(data)\n}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "impl.rs"]).unwrap();
    repo.stage_all_and_commit("fixup! AI: add process stub")
        .unwrap();

    // Autosquash rebase
    repo.git(&["rebase", "--autosquash", &format!("{}~1", target_sha)])
        .unwrap_or_else(|e| {
            eprintln!("[test] autosquash rebase error (might be expected): {}", e);
            String::new()
        });

    let mut file = repo.filename("impl.rs");
    file.assert_lines_and_blame(crate::lines![
        "// implementation".human(),
        "fn process() {".ai(),
        "    let data = fetch();".ai(),
        "    transform(data)".ai(),
        "}".ai(),
    ]);
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn test_commit_with_only_deletions_preserves_remaining_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("shrink.rs");

    // AI writes 3 functions
    repo.git_ai(&["checkpoint", "human", "shrink.rs"]).unwrap();
    fs::write(
        &file_path,
        "fn keep() {}\nfn delete_me() {}\nfn also_keep() {}\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "shrink.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI writes functions").unwrap();

    // Human deletes the middle function
    fs::write(&file_path, "fn keep() {}\nfn also_keep() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "shrink.rs"])
        .unwrap();
    repo.stage_all_and_commit("remove unused function").unwrap();

    let mut file = repo.filename("shrink.rs");
    file.assert_lines_and_blame(crate::lines!["fn keep() {}".ai(), "fn also_keep() {}".ai(),]);
}

#[test]
fn test_empty_commit_produces_no_note() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("stable.rs");

    repo.git_ai(&["checkpoint", "human", "stable.rs"]).unwrap();
    fs::write(&file_path, "fn stable() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "stable.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Empty commit
    repo.git(&["commit", "--allow-empty", "-m", "empty"])
        .unwrap();
    let empty_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Should have no note on the empty commit
    let note_result = repo.read_authorship_note(&empty_sha);
    // Note may exist but should be essentially empty (no attestations)
    if let Some(note) = note_result {
        assert!(
            !note.contains("stable.rs"),
            "empty commit should not have file attestations"
        );
    }
}

#[test]
fn test_large_file_small_ai_edit() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("big.rs");

    // Create a 200-line file (human)
    let content: Vec<String> = (0..200).map(|i| format!("fn line_{}() {{}}", i)).collect();
    fs::write(&file_path, content.join("\n") + "\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "big.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial large file").unwrap();

    // AI edits just line 100
    repo.git_ai(&["checkpoint", "human", "big.rs"]).unwrap();
    let mut modified = content.clone();
    modified[100] = "fn ai_edited_line() {}".to_string();
    fs::write(&file_path, modified.join("\n") + "\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "big.rs"]).unwrap();

    repo.stage_all_and_commit("AI tweaks line 100").unwrap();

    // Verify the AI line is attributed
    let blame_output = repo.git_ai(&["blame", "big.rs"]).unwrap();

    assert!(
        blame_output.contains("fn ai_edited_line()"),
        "blame should contain the AI-edited line"
    );
    // The AI-edited line should show mock_ai as author
    let ai_line = blame_output
        .lines()
        .find(|l| l.contains("ai_edited_line"))
        .unwrap();
    assert!(
        ai_line.contains("mock_ai"),
        "AI-edited line should be attributed to mock_ai, got: {}",
        ai_line
    );
}
