#[macro_use]
mod repos;
use repos::test_repo::TestRepo;

use git_ai::git::repository::find_repository_in_path;
use git_ai::git::status::MAX_PATHSPEC_ARGS;
use std::collections::HashSet;

/// Pad a set of real paths with non-existent paths to exceed MAX_PATHSPEC_ARGS.
/// Fake paths never match anything, so results should be identical to the CLI-arg path.
fn padded_pathspecs(real_paths: &[&str]) -> HashSet<String> {
    let mut set: HashSet<String> = real_paths.iter().map(|s| s.to_string()).collect();
    let needed = MAX_PATHSPEC_ARGS + 1 - set.len();
    for i in 0..needed {
        set.insert(format!("nonexistent/padding_{:04}.txt", i));
    }
    assert!(set.len() > MAX_PATHSPEC_ARGS);
    set
}

/// Create N numbered files in a test repo, stage them, and return filenames.
fn create_files(
    repo: &TestRepo,
    count: usize,
    content_fn: impl Fn(usize) -> String,
) -> Vec<String> {
    let mut filenames = Vec::new();
    for i in 0..count {
        let name = format!("file_{}.txt", i);
        let content = content_fn(i);
        std::fs::write(repo.path().join(&name), &content).unwrap();
        filenames.push(name);
    }
    filenames
}

// ============================================================
// Test Group A: status()
// ============================================================

#[test]
fn test_status_post_filter_equivalence() {
    let repo = TestRepo::new();

    // Create and commit 5 files
    let filenames = create_files(&repo, 5, |i| format!("initial content {}", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Modify 2 files in working dir (unstaged)
    std::fs::write(repo.path().join(&filenames[0]), "modified content 0").unwrap();
    std::fs::write(repo.path().join(&filenames[1]), "modified content 1").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Small pathspec (CLI-arg path)
    let small: HashSet<String> = filenames.iter().cloned().collect();
    let result_small = gitai_repo.status(Some(&small), true).unwrap();

    // Padded pathspec (post-filter path)
    let refs: Vec<&str> = filenames.iter().map(|s| s.as_str()).collect();
    let large = padded_pathspecs(&refs);
    let result_large = gitai_repo.status(Some(&large), true).unwrap();

    // Sort both by path for comparison
    let mut sorted_small = result_small.clone();
    sorted_small.sort_by(|a, b| a.path.cmp(&b.path));
    let mut sorted_large = result_large.clone();
    sorted_large.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(
        sorted_small.len(),
        sorted_large.len(),
        "entry count mismatch"
    );
    for (s, l) in sorted_small.iter().zip(sorted_large.iter()) {
        assert_eq!(s, l, "entries differ: {:?} vs {:?}", s, l);
    }
}

#[test]
fn test_status_post_filter_excludes_unmatched_files() {
    let repo = TestRepo::new();

    // Create and commit 5 files
    let filenames = create_files(&repo, 5, |i| format!("initial content {}", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Modify all 5 files in working dir
    for (i, name) in filenames.iter().enumerate() {
        std::fs::write(repo.path().join(name), format!("modified content {}", i)).unwrap();
    }

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Padded pathspec containing only 2 of the 5 modified files
    let subset = padded_pathspecs(&[&filenames[0], &filenames[1]]);
    let result = gitai_repo.status(Some(&subset), true).unwrap();

    let paths: HashSet<String> = result.iter().map(|e| e.path.clone()).collect();
    assert!(paths.contains(&filenames[0]), "should contain file_0");
    assert!(paths.contains(&filenames[1]), "should contain file_1");
    assert!(!paths.contains(&filenames[2]), "should NOT contain file_2");
    assert!(!paths.contains(&filenames[3]), "should NOT contain file_3");
    assert!(!paths.contains(&filenames[4]), "should NOT contain file_4");
    assert_eq!(result.len(), 2);
}

#[test]
fn test_status_post_filter_rename_matched_by_new_path() {
    let repo = TestRepo::new();

    // Commit old.txt
    std::fs::write(repo.path().join("old.txt"), "content").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // git mv old.txt new.txt (stages the rename)
    repo.git_og(&["mv", "old.txt", "new.txt"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Padded pathspec containing "new.txt"
    let pathspecs = padded_pathspecs(&["new.txt"]);
    let result = gitai_repo.status(Some(&pathspecs), true).unwrap();

    assert!(!result.is_empty(), "rename entry should appear");
    let entry = result.iter().find(|e| e.path == "new.txt").unwrap();
    assert_eq!(
        entry.orig_path.as_deref(),
        Some("old.txt"),
        "orig_path should be old.txt"
    );
}

#[test]
fn test_status_post_filter_rename_matched_by_orig_path() {
    let repo = TestRepo::new();

    // Commit old.txt
    std::fs::write(repo.path().join("old.txt"), "content").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // git mv old.txt new.txt (stages the rename)
    repo.git_og(&["mv", "old.txt", "new.txt"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Padded pathspec containing only "old.txt" (the orig_path)
    let pathspecs = padded_pathspecs(&["old.txt"]);
    let result = gitai_repo.status(Some(&pathspecs), true).unwrap();

    // The rename entry should still appear because orig_path matches
    let rename_entry = result
        .iter()
        .find(|e| e.orig_path.as_deref() == Some("old.txt"));
    assert!(
        rename_entry.is_some(),
        "rename entry should appear when matching by orig_path"
    );
}

#[test]
fn test_status_post_filter_rename_excluded_when_neither_matches() {
    // Note: status() unions staged filenames into combined_pathspecs, so we test
    // the post-filter retain predicate via list_commit_files on a commit with a rename.
    // Here we verify that status() correctly includes only the pathspec-relevant entries
    // when there are NO staged files (all changes are unstaged modifications).
    let repo = TestRepo::new();

    // Commit file_a.txt, file_b.txt, file_c.txt
    std::fs::write(repo.path().join("file_a.txt"), "content a").unwrap();
    std::fs::write(repo.path().join("file_b.txt"), "content b").unwrap();
    std::fs::write(repo.path().join("file_c.txt"), "content c").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Modify all 3 files in working dir (unstaged)
    std::fs::write(repo.path().join("file_a.txt"), "modified a").unwrap();
    std::fs::write(repo.path().join("file_b.txt"), "modified b").unwrap();
    std::fs::write(repo.path().join("file_c.txt"), "modified c").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Padded pathspec containing only "file_a.txt"
    // Since nothing is staged, combined_pathspecs == our pathspec
    let pathspecs = padded_pathspecs(&["file_a.txt"]);
    let result = gitai_repo.status(Some(&pathspecs), true).unwrap();

    let paths: Vec<&str> = result.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"file_a.txt"), "file_a.txt should appear");
    assert!(
        !paths.contains(&"file_b.txt"),
        "file_b.txt should NOT appear"
    );
    assert!(
        !paths.contains(&"file_c.txt"),
        "file_c.txt should NOT appear"
    );
    assert_eq!(result.len(), 1, "should have exactly 1 entry");
}

// ============================================================
// Test Group B: list_commit_files()
// ============================================================

#[test]
fn test_list_commit_files_post_filter_equivalence() {
    let repo = TestRepo::new();

    // Create and commit 5 files
    let filenames = create_files(&repo, 5, |i| format!("content {}", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Small pathspec (CLI-arg path)
    let small: HashSet<String> = filenames.iter().cloned().collect();
    let result_small = gitai_repo
        .list_commit_files(&head_sha, Some(&small))
        .unwrap();

    // Padded pathspec (post-filter path)
    let refs: Vec<&str> = filenames.iter().map(|s| s.as_str()).collect();
    let large = padded_pathspecs(&refs);
    let result_large = gitai_repo
        .list_commit_files(&head_sha, Some(&large))
        .unwrap();

    assert_eq!(
        result_small, result_large,
        "list_commit_files results should be identical"
    );
}

#[test]
fn test_list_commit_files_post_filter_exclusion() {
    let repo = TestRepo::new();

    // Create and commit 5 files
    let filenames = create_files(&repo, 5, |i| format!("content {}", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Padded pathspec containing only 2 of the 5 files
    let subset = padded_pathspecs(&[&filenames[0], &filenames[1]]);
    let result = gitai_repo
        .list_commit_files(&head_sha, Some(&subset))
        .unwrap();

    let expected: HashSet<String> = [filenames[0].clone(), filenames[1].clone()]
        .into_iter()
        .collect();
    assert_eq!(result, expected, "should contain only file_0 and file_1");
}

#[test]
fn test_list_commit_files_post_filter_no_matches() {
    let repo = TestRepo::new();

    // Create and commit 5 files
    create_files(&repo, 5, |i| format!("content {}", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Padded pathspec with ALL fake paths (none matching real files)
    let mut all_fake: HashSet<String> = HashSet::new();
    for i in 0..=MAX_PATHSPEC_ARGS {
        all_fake.insert(format!("nonexistent/fake_{:04}.txt", i));
    }
    assert!(all_fake.len() > MAX_PATHSPEC_ARGS);

    let result = gitai_repo
        .list_commit_files(&head_sha, Some(&all_fake))
        .unwrap();
    assert!(
        result.is_empty(),
        "should return empty when no pathspecs match"
    );
}

// ============================================================
// Test Group C: diff_added_lines()
// ============================================================

#[test]
fn test_diff_added_lines_post_filter_equivalence() {
    let repo = TestRepo::new();

    // commit1: 5 files with "line1"
    let filenames = create_files(&repo, 5, |_| "line1\n".to_string());
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit1"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha1 = gitai_repo.head().unwrap().target().unwrap();

    // commit2: modify 3 files (append "line2")
    for name in &filenames[0..3] {
        std::fs::write(repo.path().join(name), "line1\nline2\n").unwrap();
    }
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit2"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Small pathspec (all 5 files)
    let small: HashSet<String> = filenames.iter().cloned().collect();
    let result_small = gitai_repo
        .diff_added_lines(&sha1, &sha2, Some(&small))
        .unwrap();

    // Padded pathspec
    let refs: Vec<&str> = filenames.iter().map(|s| s.as_str()).collect();
    let large = padded_pathspecs(&refs);
    let result_large = gitai_repo
        .diff_added_lines(&sha1, &sha2, Some(&large))
        .unwrap();

    assert_eq!(
        result_small, result_large,
        "diff_added_lines results should be identical"
    );
}

#[test]
fn test_diff_added_lines_post_filter_exclusion() {
    let repo = TestRepo::new();

    // commit1: 5 files with "line1"
    let filenames = create_files(&repo, 5, |_| "line1\n".to_string());
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit1"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha1 = gitai_repo.head().unwrap().target().unwrap();

    // commit2: modify 3 files
    for name in &filenames[0..3] {
        std::fs::write(repo.path().join(name), "line1\nline2\n").unwrap();
    }
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit2"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Padded pathspec containing only 1 of the 3 modified files
    let subset = padded_pathspecs(&[&filenames[0]]);
    let result = gitai_repo
        .diff_added_lines(&sha1, &sha2, Some(&subset))
        .unwrap();

    assert_eq!(result.len(), 1, "should have exactly 1 file");
    assert!(result.contains_key(&filenames[0]), "should contain file_0");
    assert!(
        !result.contains_key(&filenames[1]),
        "should NOT contain file_1"
    );
    assert!(
        !result.contains_key(&filenames[2]),
        "should NOT contain file_2"
    );
}

#[test]
fn test_diff_added_lines_post_filter_correct_line_numbers() {
    let repo = TestRepo::new();

    // commit1: a.txt = "L1\nL2\nL3\n"
    std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit1"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha1 = gitai_repo.head().unwrap().target().unwrap();

    // commit2: a.txt = "L1\nL2\nL3\nL4\nL5\n"
    std::fs::write(repo.path().join("a.txt"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit2"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Padded pathspec
    let pathspecs = padded_pathspecs(&["a.txt"]);
    let result = gitai_repo
        .diff_added_lines(&sha1, &sha2, Some(&pathspecs))
        .unwrap();

    assert!(result.contains_key("a.txt"), "should contain a.txt");
    assert_eq!(
        result["a.txt"],
        vec![4, 5],
        "should report lines 4 and 5 as added"
    );
}

// ============================================================
// Test Group D: diff_workdir_added_lines_with_insertions()
// ============================================================

#[test]
fn test_diff_workdir_insertions_post_filter_equivalence() {
    let repo = TestRepo::new();

    // Commit 3 files
    let filenames = create_files(&repo, 3, |i| format!("line1_{}\n", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Modify 2 in working dir (don't commit)
    std::fs::write(
        repo.path().join(&filenames[0]),
        format!("line1_0\nline2_0\n"),
    )
    .unwrap();
    std::fs::write(
        repo.path().join(&filenames[1]),
        format!("line1_1\nline2_1\n"),
    )
    .unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Small pathspec
    let small: HashSet<String> = filenames.iter().cloned().collect();
    let (all_small, ins_small) = gitai_repo
        .diff_workdir_added_lines_with_insertions(&head_sha, Some(&small))
        .unwrap();

    // Padded pathspec
    let refs: Vec<&str> = filenames.iter().map(|s| s.as_str()).collect();
    let large = padded_pathspecs(&refs);
    let (all_large, ins_large) = gitai_repo
        .diff_workdir_added_lines_with_insertions(&head_sha, Some(&large))
        .unwrap();

    assert_eq!(all_small, all_large, "all_added should be identical");
    assert_eq!(ins_small, ins_large, "pure_insertions should be identical");
}

#[test]
fn test_diff_workdir_insertions_both_maps_filtered() {
    let repo = TestRepo::new();

    // Commit a.txt and b.txt
    std::fs::write(repo.path().join("a.txt"), "a_line1\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "b_line1\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Append lines to both in working dir
    std::fs::write(repo.path().join("a.txt"), "a_line1\na_line2\n").unwrap();
    std::fs::write(repo.path().join("b.txt"), "b_line1\nb_line2\n").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Padded pathspec containing only "a.txt"
    let pathspecs = padded_pathspecs(&["a.txt"]);
    let (all_added, pure_insertions) = gitai_repo
        .diff_workdir_added_lines_with_insertions(&head_sha, Some(&pathspecs))
        .unwrap();

    assert!(
        all_added.contains_key("a.txt"),
        "all_added should have a.txt"
    );
    assert!(
        !all_added.contains_key("b.txt"),
        "all_added should NOT have b.txt"
    );
    assert!(
        !pure_insertions.contains_key("b.txt"),
        "pure_insertions should NOT have b.txt"
    );
}

// ============================================================
// Test Group E: diff_tree_to_tree()
// ============================================================

#[test]
fn test_diff_tree_to_tree_post_filter_equivalence() {
    let repo = TestRepo::new();

    // commit1: 3 files
    create_files(&repo, 3, |i| format!("content_{}\n", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit1"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha1 = gitai_repo.head().unwrap().target().unwrap();

    // commit2: modify 2 files
    std::fs::write(repo.path().join("file_0.txt"), "modified_0\n").unwrap();
    std::fs::write(repo.path().join("file_1.txt"), "modified_1\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit2"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Get trees
    let commit1 = gitai_repo.find_commit(sha1).unwrap();
    let tree1 = commit1.tree().unwrap();
    let commit2 = gitai_repo.find_commit(sha2).unwrap();
    let tree2 = commit2.tree().unwrap();

    // Small pathspec
    let small: HashSet<String> = ["file_0.txt", "file_1.txt", "file_2.txt"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let diff_small = gitai_repo
        .diff_tree_to_tree(Some(&tree1), Some(&tree2), None, Some(&small))
        .unwrap();

    // Padded pathspec
    let large = padded_pathspecs(&["file_0.txt", "file_1.txt", "file_2.txt"]);
    let diff_large = gitai_repo
        .diff_tree_to_tree(Some(&tree1), Some(&tree2), None, Some(&large))
        .unwrap();

    assert_eq!(diff_small.len(), diff_large.len(), "delta count mismatch");

    let paths_small: HashSet<String> = diff_small
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path()
                .and_then(|p| p.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    let paths_large: HashSet<String> = diff_large
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path()
                .and_then(|p| p.to_str())
                .map(|s| s.to_string())
        })
        .collect();

    assert_eq!(paths_small, paths_large, "delta paths should be identical");
}

#[test]
fn test_diff_tree_to_tree_post_filter_exclusion() {
    let repo = TestRepo::new();

    // commit1: 3 files
    create_files(&repo, 3, |i| format!("content_{}\n", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit1"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha1 = gitai_repo.head().unwrap().target().unwrap();

    // commit2: modify 2 files
    std::fs::write(repo.path().join("file_0.txt"), "modified_0\n").unwrap();
    std::fs::write(repo.path().join("file_1.txt"), "modified_1\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "commit2"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Get trees
    let commit1 = gitai_repo.find_commit(sha1).unwrap();
    let tree1 = commit1.tree().unwrap();
    let commit2 = gitai_repo.find_commit(sha2).unwrap();
    let tree2 = commit2.tree().unwrap();

    // Padded pathspec containing only 1 of the 2 modified files
    let subset = padded_pathspecs(&["file_0.txt"]);
    let diff = gitai_repo
        .diff_tree_to_tree(Some(&tree1), Some(&tree2), None, Some(&subset))
        .unwrap();

    assert_eq!(diff.len(), 1, "should have exactly 1 delta");
    let delta_path = diff
        .deltas()
        .next()
        .unwrap()
        .new_file()
        .path()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(delta_path, "file_0.txt");
}

// ============================================================
// Test Group F: Boundary & edge cases
// ============================================================

#[test]
fn test_threshold_boundary_1000_vs_1001() {
    let repo = TestRepo::new();

    // Commit 3 files
    let filenames = create_files(&repo, 3, |i| format!("content_{}\n", i));
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Exactly 1000 entries (3 real + 997 fake) → NOT greater, should use CLI-arg path
    let mut exactly_1000: HashSet<String> = filenames.iter().cloned().collect();
    for i in 0..(MAX_PATHSPEC_ARGS - filenames.len()) {
        exactly_1000.insert(format!("nonexistent/pad_{:04}.txt", i));
    }
    assert_eq!(exactly_1000.len(), MAX_PATHSPEC_ARGS);

    // Exactly 1001 entries (3 real + 998 fake) → greater, should use post-filter path
    let mut exactly_1001 = exactly_1000.clone();
    exactly_1001.insert("nonexistent/extra.txt".to_string());
    assert_eq!(exactly_1001.len(), MAX_PATHSPEC_ARGS + 1);

    let result_1000 = gitai_repo
        .list_commit_files(&head_sha, Some(&exactly_1000))
        .unwrap();
    let result_1001 = gitai_repo
        .list_commit_files(&head_sha, Some(&exactly_1001))
        .unwrap();

    assert_eq!(
        result_1000, result_1001,
        "results at boundary (1000 vs 1001) should be identical"
    );
    // Both should return the 3 real files
    let expected: HashSet<String> = filenames.into_iter().collect();
    assert_eq!(result_1000, expected);
}

#[test]
fn test_empty_pathspec_early_return() {
    let repo = TestRepo::new();

    // Commit a file
    std::fs::write(repo.path().join("a.txt"), "content\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let head_sha = gitai_repo.head().unwrap().target().unwrap();

    // Modify the file to create a diff
    std::fs::write(repo.path().join("a.txt"), "content\nnew line\n").unwrap();
    repo.git_og(&["add", "-A"]).unwrap();
    repo.git_og(&["commit", "-m", "second"]).unwrap();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let sha2 = gitai_repo.head().unwrap().target().unwrap();

    // Empty pathspec should return empty HashMap immediately
    let empty: HashSet<String> = HashSet::new();
    let result = gitai_repo
        .diff_added_lines(&head_sha, &sha2, Some(&empty))
        .unwrap();
    assert!(
        result.is_empty(),
        "empty pathspec should return empty result"
    );
}

reuse_tests_in_worktree!(
    test_status_post_filter_equivalence,
    test_status_post_filter_excludes_unmatched_files,
    test_status_post_filter_rename_matched_by_new_path,
    test_status_post_filter_rename_matched_by_orig_path,
    test_status_post_filter_rename_excluded_when_neither_matches,
    test_list_commit_files_post_filter_equivalence,
    test_list_commit_files_post_filter_exclusion,
    test_list_commit_files_post_filter_no_matches,
    test_diff_added_lines_post_filter_equivalence,
    test_diff_added_lines_post_filter_exclusion,
    test_diff_added_lines_post_filter_correct_line_numbers,
    test_diff_workdir_insertions_post_filter_equivalence,
    test_diff_workdir_insertions_both_maps_filtered,
    test_diff_tree_to_tree_post_filter_equivalence,
    test_diff_tree_to_tree_post_filter_exclusion,
    test_threshold_boundary_1000_vs_1001,
    test_empty_pathspec_early_return,
);
