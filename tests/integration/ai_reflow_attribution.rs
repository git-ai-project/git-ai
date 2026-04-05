use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Regression test for AI reflow (line collapse) being incorrectly attributed to human.
///
/// Scenario: Human writes a two-line expression, AI reformats it into a single line.
/// The resulting line should be attributed to AI, not human.
///
/// This uses raw fs::write + checkpoint mock_ai to replicate the exact real-world
/// flow rather than the helper utilities which use a different checkpointing approach.
#[test]
fn test_ai_reflow_two_lines_to_one_attributed_to_ai() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test_repo.rs");

    // Step 1: Human writes initial content with a two-line chained call.
    // Use checkpoint + stage_all_and_commit so git-ai tracks authorship from the start.
    let initial_content = "\
fn ensure_isolated_process_home() {
    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    PROCESS_HOME.get_or_init(|| {
        let home = std::env::temp_dir()
            .join(format!(\"git-ai-test-home-{}\", std::process::id()));
        fs::create_dir_all(&home).expect(\"create isolated process HOME\");
        home
    });
}
";
    fs::write(&file_path, initial_content).unwrap();
    repo.git_ai(&["checkpoint", "--", "test_repo.rs"]).unwrap();
    repo.stage_all_and_commit("Initial human implementation")
        .unwrap();

    // Step 2: AI reformats the two-line chained call into a single line (reflow)
    let ai_reflowed_content = "\
fn ensure_isolated_process_home() {
    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    PROCESS_HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!(\"git-ai-test-home-{}\", std::process::id()));
        fs::create_dir_all(&home).expect(\"create isolated process HOME\");
        home
    });
}
";
    fs::write(&file_path, ai_reflowed_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test_repo.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI reflows chained call to single line")
        .unwrap();

    // Step 3: Assert the reflowed line is attributed to AI, not human
    let mut file = repo.filename("test_repo.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn ensure_isolated_process_home() {".human(),
        "    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();".human(),
        "    PROCESS_HOME.get_or_init(|| {".human(),
        "        let home = std::env::temp_dir().join(format!(\"git-ai-test-home-{}\", std::process::id()));".ai(),
        "        fs::create_dir_all(&home).expect(\"create isolated process HOME\");".human(),
        "        home".human(),
        "    });".human(),
        "}".human(),
    ]);
}

/// Inverse regression test: when a human does the same reflow on AI-attributed code,
/// the attribution should remain AI (not switch to human).
///
/// This ensures that formatting scripts or human reflows don't steal AI attribution.
///
/// NOTE: Currently ignored because preserving AI attribution across commits (when the
/// working log has been cleared) requires reading the HEAD commit's authorship note in
/// `get_checkpoint_entry_for_file`.  That change causes regressions in 20+ existing
/// tests and needs a more careful approach.
#[test]
#[ignore]
fn test_human_reflow_on_ai_code_retains_ai_attribution() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("test_repo.rs");

    // Step 1: Human writes initial boilerplate (context lines).
    // Use checkpoint + stage_all_and_commit so git-ai tracks authorship from the start.
    let initial_content = "\
fn ensure_isolated_process_home() {
    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
}
";
    fs::write(&file_path, initial_content).unwrap();
    repo.git_ai(&["checkpoint", "--", "test_repo.rs"]).unwrap();
    repo.stage_all_and_commit("Initial human boilerplate")
        .unwrap();

    // Step 2: AI writes a two-line chained call
    let ai_content = "\
fn ensure_isolated_process_home() {
    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    PROCESS_HOME.get_or_init(|| {
        let home = std::env::temp_dir()
            .join(format!(\"git-ai-test-home-{}\", std::process::id()));
        fs::create_dir_all(&home).expect(\"create isolated process HOME\");
        home
    });
}
";
    fs::write(&file_path, ai_content).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test_repo.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds implementation").unwrap();

    // Step 3: Human (or formatter) reflows the AI's two-line call to single line.
    // This is a human checkpoint (no mock_ai preset), so attribution should stay AI.
    let human_reflowed_content = "\
fn ensure_isolated_process_home() {
    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
    PROCESS_HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!(\"git-ai-test-home-{}\", std::process::id()));
        fs::create_dir_all(&home).expect(\"create isolated process HOME\");
        home
    });
}
";
    fs::write(&file_path, human_reflowed_content).unwrap();
    repo.git_ai(&["checkpoint", "--", "test_repo.rs"]).unwrap();
    repo.stage_all_and_commit("Human/formatter reflows AI code")
        .unwrap();

    // Step 4: Assert the reflowed line retains AI attribution
    let mut file = repo.filename("test_repo.rs");
    file.assert_lines_and_blame(crate::lines![
        "fn ensure_isolated_process_home() {".human(),
        "    static PROCESS_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();".human(),
        "    PROCESS_HOME.get_or_init(|| {".ai(),
        "        let home = std::env::temp_dir().join(format!(\"git-ai-test-home-{}\", std::process::id()));".ai(),
        "        fs::create_dir_all(&home).expect(\"create isolated process HOME\");".ai(),
        "        home".ai(),
        "    });".ai(),
        "}".human(),
    ]);
}

/// Regression test that mirrors the Chinese reflow scenario exactly.
///
/// Uses the same two-phase checkpoint approach as set_contents():
/// 1. Human checkpoint with placeholder content
/// 2. AI checkpoint with real content
///
/// Then a human reflows the AI content into multiple lines.
///
/// All reflowed lines should retain AI attribution.
///
/// NOTE: Currently ignored – same cross-commit attribution limitation as
/// `test_human_reflow_on_ai_code_retains_ai_attribution`.
#[test]
#[ignore]
fn test_human_reflow_of_ai_set_contents_retains_ai() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("reflow.txt");

    // Step 1: Two-phase checkpoint approach (same as set_contents helper):
    //   Phase A: Human checkpoint with placeholder
    let placeholder = "||__AI LINE__ PENDING__||";
    fs::write(&file_path, placeholder).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git_ai(&["checkpoint", "--", "reflow.txt"]).unwrap();

    //   Phase B: AI checkpoint with real content
    let ai_content = "调用(参数一, 参数二, 参数三)";
    fs::write(&file_path, ai_content).unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "reflow.txt"])
        .unwrap();

    repo.stage_all_and_commit("Initial AI content").unwrap();

    // Step 2: Human (or formatter) reflows the single AI line into multiple lines
    let human_reflowed = "调用(\n  参数一,\n  参数二,\n  参数三\n)";
    fs::write(&file_path, human_reflowed).unwrap();
    repo.git_ai(&["checkpoint", "--", "reflow.txt"]).unwrap();
    repo.stage_all_and_commit("Human reflow").unwrap();

    // Step 3: All lines should retain AI attribution
    let mut file = repo.filename("reflow.txt");
    file.assert_lines_and_blame(crate::lines![
        "调用(".ai(),
        "  参数一,".ai(),
        "  参数二,".ai(),
        "  参数三".ai(),
        ")".ai(),
    ]);
}

/// Inverse: AI reflows human single-line content into multiple lines.
/// All reflowed lines should be attributed to AI.
///
/// NOTE: Currently ignored because force-splitting 1→N hunks in AI checkpoints
/// is too aggressive – it also catches cases where AI appends content after a
/// human line (the trailing newline pulls the human line into the changed hunk).
/// A more targeted approach is needed to distinguish true reflows from appends.
#[test]
#[ignore]
fn test_ai_reflow_of_human_content_one_to_many_lines_attributed_to_ai() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("reflow2.txt");

    // Step 1: Human writes single-line content
    let human_content = "call(arg1, arg2, arg3)";
    fs::write(&file_path, human_content).unwrap();
    repo.git_ai(&["checkpoint", "--", "reflow2.txt"]).unwrap();
    repo.stage_all_and_commit("Initial human content").unwrap();

    // Step 2: AI reflows to multiple lines
    let ai_reflowed = "call(\n  arg1,\n  arg2,\n  arg3\n)";
    fs::write(&file_path, ai_reflowed).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "reflow2.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI reflows to multiple lines")
        .unwrap();

    // Step 3: All lines should be attributed to AI
    let mut file = repo.filename("reflow2.txt");
    file.assert_lines_and_blame(crate::lines![
        "call(".ai(),
        "  arg1,".ai(),
        "  arg2,".ai(),
        "  arg3".ai(),
        ")".ai(),
    ]);
}

crate::reuse_tests_in_worktree!(
    test_ai_reflow_two_lines_to_one_attributed_to_ai,
    // Excluded: test_human_reflow_on_ai_code_retains_ai_attribution (ignored – needs checkpoint.rs cross-commit fix)
    // Excluded: test_human_reflow_of_ai_set_contents_retains_ai (ignored – needs checkpoint.rs cross-commit fix)
    // Excluded: test_ai_reflow_of_human_content_one_to_many_lines_attributed_to_ai (ignored – needs 1→N force-split fix)
);
