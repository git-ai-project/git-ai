use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Feature branch with AI commits, squash-merge into main, verify AI attribution preserved.
#[test]
fn test_squash_merge_preserves_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("main.txt");

    // Create initial content on default branch (human)
    std::fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    // Pre-edit snapshot
    repo.git_ai(&["checkpoint", "human", "main.txt"]).unwrap();
    // AI adds a line
    std::fs::write(&file_path, "line 1\nline 2\nline 3\n// AI added feature\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "main.txt"]).unwrap();
    repo.stage_all_and_commit("Add AI feature").unwrap();

    // Go back to main and squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed feature").unwrap();

    // Verify AI attribution is preserved through squash merge
    let mut file = repo.filename("main.txt");
    file.assert_lines_and_blame(crate::lines![
        "line 1".human(),
        "line 2".human(),
        "line 3".human(),
        "// AI added feature".ai(),
    ]);
}

/// Squash merge with AI edits across multiple files.
#[test]
fn test_squash_merge_multiple_files() {
    let repo = TestRepo::new();

    // Create initial content on default branch with multiple files
    let mut file_a = repo.filename("file_a.rs");
    file_a.set_contents(crate::lines!["fn a() {}", ""]);
    let mut file_b = repo.filename("file_b.rs");
    file_b.set_contents(crate::lines!["fn b() {}", ""]);
    let mut file_c = repo.filename("file_c.rs");
    file_c.set_contents(crate::lines!["fn c() {}", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with AI changes across multiple files
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI edits file_a
    file_a.insert_at(1, crate::lines!["    // AI logic in a".ai()]);
    repo.stage_all_and_commit("AI edits file_a").unwrap();

    // AI edits file_b
    file_b.insert_at(1, crate::lines!["    // AI logic in b".ai()]);
    repo.stage_all_and_commit("AI edits file_b").unwrap();

    // AI edits file_c
    file_c.insert_at(1, crate::lines!["    // AI logic in c".ai()]);
    repo.stage_all_and_commit("AI edits file_c").unwrap();

    // Go back to main and squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squash merge multi-file feature").unwrap();

    // Verify AI attribution in all three files
    file_a.assert_lines_and_blame(crate::lines![
        "fn a() {}".human(),
        "    // AI logic in a".ai(),
    ]);

    file_b.assert_lines_and_blame(crate::lines![
        "fn b() {}".human(),
        "    // AI logic in b".ai(),
    ]);

    file_c.assert_lines_and_blame(crate::lines![
        "fn c() {}".human(),
        "    // AI logic in c".ai(),
    ]);
}

/// Feature branch has both AI and human commits, squash preserves both attributions.
#[test]
fn test_squash_merge_mixed_ai_and_human() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("file.txt");

    // Create initial content on default branch (human)
    std::fs::write(&file_path, "header\nbody\nfooter\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "file.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    let default_branch = repo.current_branch();

    // Create feature branch with mixed AI and human commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // AI commit - inserts line after header
    repo.git_ai(&["checkpoint", "human", "file.txt"]).unwrap();
    std::fs::write(&file_path, "header\n// AI session 1\nbody\nfooter\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI session 1").unwrap();

    // Human commit - inserts line after body
    std::fs::write(
        &file_path,
        "header\n// AI session 1\nbody\n// Human addition\nfooter\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "file.txt"])
        .unwrap();
    repo.stage_all_and_commit("Human edit").unwrap();

    // Another AI commit - appends line after footer
    repo.git_ai(&["checkpoint", "human", "file.txt"]).unwrap();
    std::fs::write(
        &file_path,
        "header\n// AI session 1\nbody\n// Human addition\nfooter\n// AI session 2\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    repo.stage_all_and_commit("AI session 2").unwrap();

    // Squash merge into main
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("Squashed multiple sessions").unwrap();

    // Verify both AI and human attributions are preserved
    let mut file = repo.filename("file.txt");
    file.assert_lines_and_blame(crate::lines![
        "header".human(),
        "// AI session 1".ai(),
        "body".human(),
        "// Human addition".human(),
        "footer".human(),
        "// AI session 2".ai(),
    ]);
}

/// Multiple AI sessions (different agents/checkpoints) survive squash merge.
#[test]
fn test_squash_merge_multiple_sessions() {
    let repo = TestRepo::new();
    let mut file = repo.filename("module.py");

    // Create initial content
    file.set_contents(crate::lines!["# module"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Create feature branch with multiple AI sessions
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI session creates a class
    file.set_contents(crate::lines![
        "class Store:".ai(),
        "    def __init__(self):".ai(),
        "        self.data = {}".ai(),
        "    def get(self, k):".ai(),
        "        return self.data.get(k)".ai(),
    ]);
    repo.stage_all_and_commit("Session A: create Store class")
        .unwrap();

    // Second AI session adds interleaved lines (docstrings and a new method)
    file.set_contents(crate::lines![
        "class Store:".ai(),
        "    \"\"\"A data store.\"\"\"".ai(),
        "    def __init__(self):".ai(),
        "        self.data = {}".ai(),
        "        self.cache = {}".ai(),
        "    def get(self, k):".ai(),
        "        \"\"\"Get value.\"\"\"".ai(),
        "        return self.data.get(k)".ai(),
        "    def set(self, k, v):".ai(),
        "        self.data[k] = v".ai(),
    ]);
    repo.stage_all_and_commit("Session B: add docstrings and set method")
        .unwrap();

    // Squash merge into main
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    repo.commit("squash merge").unwrap();

    // ALL 10 lines should be AI-attributed (5 from session A, 5 from session B).
    file.assert_lines_and_blame(crate::lines![
        "class Store:".ai(),
        "    \"\"\"A data store.\"\"\"".ai(),
        "    def __init__(self):".ai(),
        "        self.data = {}".ai(),
        "        self.cache = {}".ai(),
        "    def get(self, k):".ai(),
        "        \"\"\"Get value.\"\"\"".ai(),
        "        return self.data.get(k)".ai(),
        "    def set(self, k, v):".ai(),
        "        self.data[k] = v".ai(),
    ]);
}

crate::reuse_tests_in_worktree!(
    test_squash_merge_preserves_ai_attribution,
    test_squash_merge_multiple_files,
    test_squash_merge_mixed_ai_and_human,
    test_squash_merge_multiple_sessions,
);
