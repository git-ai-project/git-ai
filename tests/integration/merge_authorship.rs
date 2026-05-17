use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Test 1: Fast-forward merge preserves AI authorship from feature branch.
///
/// When main has no new commits since the feature branch diverged, git performs
/// a fast-forward merge. The original commits (and their authorship notes) should
/// remain intact and blame should correctly attribute AI lines.
#[test]
fn test_ff_merge_preserves_ai_authorship() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("code.py");

    // Initial commit on main with human-written content
    let initial = "def main():\n    pass\n";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "code.py"])
        .unwrap();
    repo.stage_all_and_commit("initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature-ff"]).unwrap();

    // AI adds new lines on feature branch - use pre/post checkpoint flow
    repo.git_ai(&["checkpoint", "human", "code.py"]).unwrap();
    let ai_edit = "def main():\n    pass\n\ndef helper():\n    return 42\n";
    fs::write(&file_path, ai_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "code.py"]).unwrap();
    let feature_commit = repo
        .stage_all_and_commit("AI adds helper function")
        .unwrap();

    // Verify the feature commit has an authorship note
    assert!(
        repo.read_authorship_note(&feature_commit.commit_sha)
            .is_some(),
        "Feature commit should have an authorship note"
    );

    // Switch to main (no new commits, so merge will fast-forward)
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "feature-ff"]).unwrap();

    // After FF merge, HEAD should be the same commit as the feature branch
    let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        head_sha, feature_commit.commit_sha,
        "FF merge should just move HEAD to the feature commit"
    );

    // Authorship note should still be present on the commit
    assert!(
        repo.read_authorship_note(&head_sha).is_some(),
        "Authorship note should survive fast-forward merge"
    );

    // Verify blame attribution - the AI added lines 3-5 (empty line, helper function)
    let mut file = repo.filename("code.py");
    file.assert_committed_lines(crate::lines![
        "def main():".human(),
        "    pass".human(),
        "".ai(),
        "def helper():".ai(),
        "    return 42".ai(),
    ]);
}

/// Test 2: Three-way merge preserves AI authorship from both branches.
///
/// When both branches have diverged, git creates a merge commit. The authorship
/// notes on the individual commits from both branches should remain accessible,
/// and blame should correctly attribute lines from each branch.
#[test]
fn test_three_way_merge_preserves_authorship_from_both_branches() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("app.rs");

    // Initial commit
    let initial = "fn main() {\n    println!(\"hello\");\n}\n";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "app.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let default_branch = repo.current_branch();

    // Feature branch: AI adds a function at the end
    repo.git(&["checkout", "-b", "feature-3way"]).unwrap();
    let feature_edit =
        "fn main() {\n    println!(\"hello\");\n}\n\nfn ai_feature() {\n    todo!()\n}\n";
    fs::write(&file_path, feature_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "app.rs"]).unwrap();
    let feature_commit = repo
        .stage_all_and_commit("AI adds feature function")
        .unwrap();

    // Main branch: human adds a line at the top (non-conflicting)
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_edit = "// Main module\nfn main() {\n    println!(\"hello\");\n}\n";
    fs::write(&file_path, main_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "app.rs"])
        .unwrap();
    let main_commit = repo.stage_all_and_commit("human adds comment").unwrap();

    // Verify both commits have authorship notes before merge
    assert!(
        repo.read_authorship_note(&feature_commit.commit_sha)
            .is_some(),
        "Feature commit should have authorship note"
    );
    assert!(
        repo.read_authorship_note(&main_commit.commit_sha).is_some(),
        "Main commit should have authorship note"
    );

    // Perform three-way merge
    repo.git(&["merge", "feature-3way", "-m", "Merge feature into main"])
        .unwrap();

    // Both original commits should still have their notes
    assert!(
        repo.read_authorship_note(&feature_commit.commit_sha)
            .is_some(),
        "Feature commit note should survive merge"
    );
    assert!(
        repo.read_authorship_note(&main_commit.commit_sha).is_some(),
        "Main commit note should survive merge"
    );

    // Verify blame correctly attributes lines from both branches
    let mut file = repo.filename("app.rs");
    file.assert_committed_lines(crate::lines![
        "// Main module".human(),
        "fn main() {".human(),
        "    println!(\"hello\");".human(),
        "}".human(),
        "".ai(),
        "fn ai_feature() {".ai(),
        "    todo!()".ai(),
        "}".ai(),
    ]);
}

/// Test 3: Merge commit itself has no authorship note.
///
/// A pure merge commit (one that only combines two branches without introducing
/// new content) should NOT have an authorship note attached to it, since no new
/// lines were authored in the merge commit itself.
#[test]
fn test_merge_commit_has_no_authorship_note() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("lib.rs");

    // Initial commit
    let initial = "pub fn init() {}\n";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let default_branch = repo.current_branch();

    // Feature branch: AI adds a new file (no conflict possible)
    repo.git(&["checkout", "-b", "feature-no-note"]).unwrap();
    let feature_file_path = repo.path().join("feature.rs");
    fs::write(&feature_file_path, "pub fn feature() { todo!() }\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds feature.rs").unwrap();

    // Main branch: human adds a different file (no conflict)
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_file_path = repo.path().join("utils.rs");
    fs::write(&main_file_path, "pub fn util() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "utils.rs"])
        .unwrap();
    repo.stage_all_and_commit("human adds utils.rs").unwrap();

    // Perform merge (no conflicts, creates a merge commit)
    repo.git(&[
        "merge",
        "--no-ff",
        "feature-no-note",
        "-m",
        "Merge feature-no-note",
    ])
    .unwrap();

    // Get the merge commit SHA
    let merge_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify the merge commit is indeed a merge (has 2 parents)
    let parents = repo
        .git(&["rev-list", "--parents", "-1", &merge_sha])
        .unwrap();
    let parent_count = parents.split_whitespace().count() - 1; // subtract the commit itself
    assert_eq!(
        parent_count, 2,
        "Merge commit should have exactly 2 parents"
    );

    // The merge commit itself should NOT have an authorship note
    assert!(
        repo.read_authorship_note(&merge_sha).is_none(),
        "Pure merge commit should not have an authorship note, but got: {}",
        repo.read_authorship_note(&merge_sha).unwrap_or_default()
    );

    // But blame should still correctly attribute lines in files from both branches
    let mut feature_file = repo.filename("feature.rs");
    feature_file.assert_committed_lines(crate::lines!["pub fn feature() { todo!() }".ai(),]);

    let mut utils_file = repo.filename("utils.rs");
    utils_file.assert_committed_lines(crate::lines!["pub fn util() {}".human(),]);
}

/// Test 4: Merge with conflicts - human resolution lines are unattributed,
/// AI lines from both sides are preserved.
///
/// When a merge produces conflicts and a human resolves them manually (without
/// an AI checkpoint), the resolution lines should be unattributed. Lines that
/// came from either branch and were not part of the conflict should retain their
/// original AI/human attribution.
#[test]
fn test_merge_conflict_resolution_preserves_non_conflicting_ai_lines() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("config.toml");

    // Initial commit with a few lines
    let initial = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "config.toml"])
        .unwrap();
    repo.stage_all_and_commit("initial config").unwrap();

    let default_branch = repo.current_branch();

    // Feature branch: AI modifies version AND adds a new section at end
    repo.git(&["checkout", "-b", "feature-conflict"]).unwrap();
    let feature_edit =
        "[package]\nname = \"app\"\nversion = \"2.0.0\"\n\n[dependencies]\nserde = \"1.0\"\n";
    fs::write(&file_path, feature_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "config.toml"])
        .unwrap();
    repo.stage_all_and_commit("AI updates version and adds deps")
        .unwrap();

    // Main branch: human modifies the same version line (conflict!) but different end
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_edit = "[package]\nname = \"app\"\nversion = \"1.5.0\"\n";
    fs::write(&file_path, main_edit).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "config.toml"])
        .unwrap();
    repo.stage_all_and_commit("human updates version").unwrap();

    // Attempt merge - should conflict on the version line
    let merge_result = repo.git(&["merge", "feature-conflict", "-m", "merge feature"]);
    assert!(
        merge_result.is_err(),
        "merge should conflict on the version line"
    );

    // Human resolves conflict manually: picks a compromise version,
    // keeps the AI-added dependencies section
    let resolved =
        "[package]\nname = \"app\"\nversion = \"2.0.0-rc1\"\n\n[dependencies]\nserde = \"1.0\"\n";
    fs::write(&file_path, resolved).unwrap();

    // Stage the resolution and commit (no AI checkpoint - purely human resolution)
    repo.git(&["add", "config.toml"]).unwrap();
    repo.git(&["commit", "-m", "resolve merge conflict"])
        .unwrap();

    // Wait for daemon sync
    let _merge_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Blame should show:
    // - "[package]" and "name = ..." are from the original commit (human)
    // - "version = ..." is the human-resolved conflict line (human - since no AI checkpoint)
    // - The [dependencies] section came from the feature branch (AI)
    let mut file = repo.filename("config.toml");
    file.assert_committed_lines(crate::lines![
        "[package]".human(),
        "name = \"app\"".human(),
        "version = \"2.0.0-rc1\"".human(),
        "".ai(),
        "[dependencies]".ai(),
        "serde = \"1.0\"".ai(),
    ]);
}

/// Test 5: Octopus merge (merging multiple branches) preserves all notes.
///
/// Git supports merging more than two branches at once (octopus merge). Each
/// source branch's authorship notes should be preserved and blame should
/// correctly attribute lines from all merged branches.
#[test]
fn test_octopus_merge_preserves_all_authorship_notes() {
    let repo = TestRepo::new();
    let base_path = repo.path().join("base.txt");

    // Initial commit
    fs::write(&base_path, "base content\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "base.txt"])
        .unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let default_branch = repo.current_branch();

    // Branch A: AI creates file_a.txt
    repo.git(&["checkout", "-b", "branch-a"]).unwrap();
    let file_a_path = repo.path().join("file_a.txt");
    fs::write(&file_a_path, "AI branch A line 1\nAI branch A line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file_a.txt"])
        .unwrap();
    let commit_a = repo.stage_all_and_commit("AI adds file_a").unwrap();

    // Branch B: AI creates file_b.txt (from main, not from branch-a)
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "branch-b"]).unwrap();
    let file_b_path = repo.path().join("file_b.txt");
    fs::write(&file_b_path, "AI branch B line 1\nAI branch B line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file_b.txt"])
        .unwrap();
    let commit_b = repo.stage_all_and_commit("AI adds file_b").unwrap();

    // Branch C: human creates file_c.txt (from main)
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["checkout", "-b", "branch-c"]).unwrap();
    let file_c_path = repo.path().join("file_c.txt");
    fs::write(&file_c_path, "Human branch C line 1\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "file_c.txt"])
        .unwrap();
    let commit_c = repo.stage_all_and_commit("human adds file_c").unwrap();

    // Switch to main and add a commit to prevent fast-forward
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_extra_path = repo.path().join("main_extra.txt");
    fs::write(&main_extra_path, "main diverged\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main_extra.txt"])
        .unwrap();
    repo.stage_all_and_commit("main diverges").unwrap();

    // Perform octopus merge of all three branches
    repo.git(&[
        "merge",
        "branch-a",
        "branch-b",
        "branch-c",
        "-m",
        "Octopus merge",
    ])
    .unwrap();

    // Verify all original commits still have their authorship notes
    assert!(
        repo.read_authorship_note(&commit_a.commit_sha).is_some(),
        "Branch A commit should retain authorship note after octopus merge"
    );
    assert!(
        repo.read_authorship_note(&commit_b.commit_sha).is_some(),
        "Branch B commit should retain authorship note after octopus merge"
    );
    assert!(
        repo.read_authorship_note(&commit_c.commit_sha).is_some(),
        "Branch C commit should retain authorship note after octopus merge"
    );

    // Verify the octopus merge commit itself has no authorship note
    let merge_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert!(
        repo.read_authorship_note(&merge_sha).is_none(),
        "Octopus merge commit should not have an authorship note, but got: {}",
        repo.read_authorship_note(&merge_sha).unwrap_or_default()
    );

    // Verify the merge commit has 4 parents (main + 3 branches)
    let parents = repo
        .git(&["rev-list", "--parents", "-1", &merge_sha])
        .unwrap();
    let parent_count = parents.split_whitespace().count() - 1;
    assert_eq!(
        parent_count, 4,
        "Octopus merge should have 4 parents (main + 3 branches), got {}",
        parent_count
    );

    // Verify blame attribution for each file
    let mut file_a = repo.filename("file_a.txt");
    file_a.assert_committed_lines(crate::lines![
        "AI branch A line 1".ai(),
        "AI branch A line 2".ai(),
    ]);

    let mut file_b = repo.filename("file_b.txt");
    file_b.assert_committed_lines(crate::lines![
        "AI branch B line 1".ai(),
        "AI branch B line 2".ai(),
    ]);

    let mut file_c = repo.filename("file_c.txt");
    file_c.assert_committed_lines(crate::lines!["Human branch C line 1".human(),]);

    let mut base_file = repo.filename("base.txt");
    base_file.assert_committed_lines(crate::lines!["base content".human(),]);
}

crate::reuse_tests_in_worktree!(
    test_ff_merge_preserves_ai_authorship,
    test_three_way_merge_preserves_authorship_from_both_branches,
    test_merge_commit_has_no_authorship_note,
    test_merge_conflict_resolution_preserves_non_conflicting_ai_lines,
    test_octopus_merge_preserves_all_authorship_notes,
);
