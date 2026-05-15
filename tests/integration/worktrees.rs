use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, get_binary_path};
use std::fs;
use std::process::Command;

fn unique_worktree_path(repo: &TestRepo, suffix: &str) -> std::path::PathBuf {
    repo.test_home_path().join(format!("wt-{}", suffix))
}

/// Run git-ai from a specific working directory (worktree).
fn run_git_ai_in(
    cwd: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> Result<String, String> {
    let binary = get_binary_path();
    let output = Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .env("HOME", home)
        .output()
        .unwrap_or_else(|_| panic!("Failed to execute git-ai {:?}", args));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(format!("{}{}", stderr, stdout))
    }
}

/// Run real git from a specific working directory.
fn run_git_in(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap_or_else(|_| panic!("Failed to execute git {:?}", args));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(format!("{}{}", stderr, stdout))
    }
}

fn blame_line_is_ai(line: &str) -> bool {
    let ai_names = [
        "mock_ai", "claude", "gpt", "copilot", "cursor", "codex", "gemini",
    ];
    let lower = line.to_lowercase();
    ai_names.iter().any(|name| lower.contains(name))
}

#[test]
fn test_worktree_checkpoint_and_blame() {
    let repo = TestRepo::new();
    let home = repo.test_home_path();

    let mut seed = repo.filename("seed.txt");
    seed.set_contents(crate::lines!["seed line".human()]);
    repo.stage_all_and_commit("initial commit").unwrap();

    let worktree_path = unique_worktree_path(&repo, "blame");
    repo.git(&[
        "worktree",
        "add",
        worktree_path.to_str().unwrap(),
        "-b",
        "feature-blame",
    ])
    .unwrap();

    let wt_file = worktree_path.join("feature.rs");
    fs::write(&wt_file, "fn hello() {}\nfn world() {}\n").unwrap();

    run_git_ai_in(
        &worktree_path,
        &home,
        &["checkpoint", "mock_ai", "feature.rs"],
    )
    .expect("AI checkpoint in worktree should succeed");

    run_git_in(&worktree_path, &["add", "feature.rs"]).unwrap();
    run_git_in(&worktree_path, &["commit", "-m", "add feature"]).unwrap();
    run_git_ai_in(&worktree_path, &home, &["post-commit"])
        .expect("post-commit in worktree should succeed");

    let blame_output = run_git_ai_in(&worktree_path, &home, &["blame", "feature.rs"])
        .expect("blame should succeed");

    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        blame_lines.len(),
        2,
        "Expected 2 blame lines, got: {}",
        blame_output
    );
    assert!(
        blame_line_is_ai(blame_lines[0]),
        "Line 1 should be AI: {}",
        blame_lines[0]
    );
    assert!(
        blame_line_is_ai(blame_lines[1]),
        "Line 2 should be AI: {}",
        blame_lines[1]
    );
}

#[test]
fn test_worktree_isolation() {
    let repo = TestRepo::new();
    let home = repo.test_home_path();

    let mut seed = repo.filename("seed.txt");
    seed.set_contents(crate::lines!["seed".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    let wt1_path = unique_worktree_path(&repo, "iso-1");
    let wt2_path = unique_worktree_path(&repo, "iso-2");

    repo.git(&[
        "worktree",
        "add",
        wt1_path.to_str().unwrap(),
        "-b",
        "feature-1",
    ])
    .unwrap();
    repo.git(&[
        "worktree",
        "add",
        wt2_path.to_str().unwrap(),
        "-b",
        "feature-2",
    ])
    .unwrap();

    let wt1_file = wt1_path.join("wt1_only.rs");
    fs::write(&wt1_file, "fn wt1() {}\n").unwrap();
    run_git_ai_in(&wt1_path, &home, &["checkpoint", "mock_ai", "wt1_only.rs"])
        .expect("checkpoint in wt1 should succeed");

    // Verify worktree2's working log does NOT contain wt1's AI file
    let common_dir = run_git_in(&wt1_path, &["rev-parse", "--git-common-dir"])
        .unwrap()
        .trim()
        .to_string();
    let common_dir_path = if std::path::Path::new(&common_dir).is_relative() {
        wt1_path.join(&common_dir)
    } else {
        std::path::PathBuf::from(&common_dir)
    };

    let wt2_head = run_git_in(&wt2_path, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let wt2_working_log_dir = common_dir_path
        .join("ai")
        .join("working_logs")
        .join(&wt2_head);

    if wt2_working_log_dir.exists() {
        let checkpoints_file = wt2_working_log_dir.join("checkpoints.jsonl");
        if checkpoints_file.exists() {
            let content = fs::read_to_string(&checkpoints_file).unwrap_or_default();
            assert!(
                !content.contains("wt1_only.rs"),
                "worktree2 should NOT contain checkpoints for wt1's files"
            );
        }
    }
}

#[test]
fn test_worktree_rebase_preserves_attribution() {
    let repo = TestRepo::new();
    let home = repo.test_home_path();

    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["base line".human()]);
    repo.stage_all_and_commit("base commit").unwrap();

    let wt_path = unique_worktree_path(&repo, "rebase");
    repo.git(&[
        "worktree",
        "add",
        wt_path.to_str().unwrap(),
        "-b",
        "feature-rebase",
    ])
    .unwrap();

    let wt_file = wt_path.join("feature.rs");
    fs::write(&wt_file, "fn feature() {}\n").unwrap();
    run_git_ai_in(&wt_path, &home, &["checkpoint", "mock_ai", "feature.rs"]).unwrap();
    run_git_in(&wt_path, &["add", "feature.rs"]).unwrap();
    run_git_in(&wt_path, &["commit", "-m", "AI feature"]).unwrap();
    run_git_ai_in(&wt_path, &home, &["post-commit"]).unwrap();

    let old_sha = run_git_in(&wt_path, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Advance main with non-conflicting commit
    let mut main_file = repo.filename("main_only.txt");
    main_file.set_contents(crate::lines!["main content".human()]);
    repo.stage_all_and_commit("main advance").unwrap();

    // Rebase worktree branch onto main
    run_git_in(&wt_path, &["rebase", "main"]).unwrap();

    let new_sha = run_git_in(&wt_path, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(old_sha, new_sha, "Rebase should create a new commit");

    // Transfer authorship note
    run_git_ai_in(&wt_path, &home, &["post-rewrite", &old_sha, &new_sha]).unwrap();

    let blame_output =
        run_git_ai_in(&wt_path, &home, &["blame", "feature.rs"]).expect("blame should succeed");
    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(blame_lines.len(), 1, "Expected 1 blame line");
    assert!(
        blame_line_is_ai(blame_lines[0]),
        "AI attribution should survive rebase: {}",
        blame_lines[0]
    );
}

#[test]
fn test_worktree_stash_preserves_attribution() {
    let repo = TestRepo::new();
    let home = repo.test_home_path();

    let mut seed = repo.filename("seed.txt");
    seed.set_contents(crate::lines!["seed".human()]);
    repo.stage_all_and_commit("initial").unwrap();

    let wt_path = unique_worktree_path(&repo, "stash");
    repo.git(&[
        "worktree",
        "add",
        wt_path.to_str().unwrap(),
        "-b",
        "feature-stash",
    ])
    .unwrap();

    let wt_file = wt_path.join("stash_test.txt");
    fs::write(&wt_file, "base line\n").unwrap();
    run_git_in(&wt_path, &["add", "stash_test.txt"]).unwrap();
    run_git_in(&wt_path, &["commit", "-m", "base in worktree"]).unwrap();
    run_git_ai_in(&wt_path, &home, &["post-commit"]).unwrap();

    // AI edit (uncommitted)
    fs::write(&wt_file, "base line\nai stash line\n").unwrap();
    run_git_ai_in(
        &wt_path,
        &home,
        &["checkpoint", "mock_ai", "stash_test.txt"],
    )
    .unwrap();

    // Save stash attributions
    let _ = run_git_ai_in(&wt_path, &home, &["stash-save"]);

    // Stash
    run_git_in(&wt_path, &["stash", "push", "-m", "wip"]).unwrap();
    let content_after_stash = fs::read_to_string(&wt_file).unwrap();
    assert!(!content_after_stash.contains("ai stash line"));

    // Pop
    run_git_in(&wt_path, &["stash", "pop"]).unwrap();
    let _ = run_git_ai_in(&wt_path, &home, &["stash-restore"]);

    let content_after_pop = fs::read_to_string(&wt_file).unwrap();
    assert!(content_after_pop.contains("ai stash line"));

    // Commit and verify
    run_git_in(&wt_path, &["add", "stash_test.txt"]).unwrap();
    run_git_in(&wt_path, &["commit", "-m", "commit after stash pop"]).unwrap();
    run_git_ai_in(&wt_path, &home, &["post-commit"]).unwrap();

    let blame_output =
        run_git_ai_in(&wt_path, &home, &["blame", "stash_test.txt"]).expect("blame should succeed");
    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        blame_lines.len(),
        2,
        "Expected 2 blame lines, got:\n{}",
        blame_output
    );
    assert!(
        blame_line_is_ai(blame_lines[1]),
        "AI attribution should survive stash/pop: {}",
        blame_lines[1]
    );
}
