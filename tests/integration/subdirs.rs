use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{TestRepo, get_binary_path};
use std::fs;
use std::process::Command;

/// Run git-ai from a specific working directory with proper HOME isolation.
fn run_git_ai_in_with_home(
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

/// Helper to check if a blame line indicates AI authorship.
fn blame_line_is_ai(line: &str) -> bool {
    let ai_names = [
        "mock_ai", "claude", "gpt", "copilot", "cursor", "codex", "gemini",
    ];
    let lower = line.to_lowercase();
    ai_names.iter().any(|name| lower.contains(name))
}

#[test]
fn test_checkpoint_from_subdirectory() {
    // Run checkpoint from a subdirectory (not repo root), verify attribution works.
    let repo = TestRepo::new();

    // Initial commit
    let mut seed = repo.filename("README.md");
    seed.set_contents(crate::lines!["# Project".human()]);
    repo.stage_all_and_commit("initial commit").unwrap();

    // Create subdirectory structure
    let subdir = repo.path().join("src").join("lib");
    fs::create_dir_all(&subdir).unwrap();

    // Write a file in the subdirectory
    let file_path = subdir.join("module.rs");
    fs::write(&file_path, "fn code() {}\nfn more() {}\n").unwrap();

    // Run checkpoint from within the subdirectory using a relative file path.
    // The checkpoint should resolve the repo root and attribute correctly.
    let home = repo.test_home_path();
    run_git_ai_in_with_home(&subdir, &home, &["checkpoint", "mock_ai", "module.rs"])
        .expect("checkpoint from subdirectory should succeed");

    // Stage and commit
    repo.git(&["add", "src/lib/module.rs"]).unwrap();
    repo.commit("add module").unwrap();

    // Verify AI attribution via blame
    let mut file = repo.filename("src/lib/module.rs");
    file.assert_lines_and_blame(crate::lines!["fn code() {}".ai(), "fn more() {}".ai()]);
}

#[test]
fn test_blame_from_subdirectory() {
    // Run blame from a subdirectory.
    let repo = TestRepo::new();

    // Create a file with AI content in a subdirectory
    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let mut file = repo.filename("src/app.rs");
    file.set_contents(crate::lines![
        "fn main() {".human(),
        "    println!(\"hello\");".ai(),
        "}".human()
    ]);
    repo.stage_all_and_commit("add app").unwrap();

    // Run blame from within the subdirectory using a relative path
    let home = repo.test_home_path();
    let blame_output = run_git_ai_in_with_home(&subdir, &home, &["blame", "app.rs"])
        .expect("blame from subdirectory should succeed");

    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        blame_lines.len(),
        3,
        "Expected 3 blame lines, got:\n{}",
        blame_output
    );

    // Line 1: human
    assert!(
        !blame_line_is_ai(blame_lines[0]),
        "Line 1 should be human: {}",
        blame_lines[0]
    );
    // Line 2: AI
    assert!(
        blame_line_is_ai(blame_lines[1]),
        "Line 2 should be AI: {}",
        blame_lines[1]
    );
    // Line 3: human
    assert!(
        !blame_line_is_ai(blame_lines[2]),
        "Line 3 should be human: {}",
        blame_lines[2]
    );
}

#[test]
fn test_commit_from_subdirectory() {
    // AI edits committed from subdirectory preserve attribution.
    let repo = TestRepo::new();

    // Initial commit
    let mut seed = repo.filename("README.md");
    seed.set_contents(crate::lines!["# Project".human()]);
    repo.stage_all_and_commit("initial commit").unwrap();

    // Create subdirectory
    let subdir = repo.path().join("src").join("components");
    fs::create_dir_all(&subdir).unwrap();

    // Write file in subdirectory
    let file_path = subdir.join("widget.rs");
    fs::write(
        &file_path,
        "pub struct Widget;\nimpl Widget {\n    pub fn render(&self) {}\n}\n",
    )
    .unwrap();

    // Checkpoint from subdirectory
    let home = repo.test_home_path();
    run_git_ai_in_with_home(&subdir, &home, &["checkpoint", "mock_ai", "widget.rs"])
        .expect("checkpoint from subdirectory should succeed");

    // Stage from subdirectory
    run_git_in(&subdir, &["add", "widget.rs"]).unwrap();

    // Commit from the subdirectory (git should find the repo root)
    run_git_in(&subdir, &["commit", "-m", "add widget"]).unwrap();

    // Run post-commit from subdirectory
    run_git_ai_in_with_home(&subdir, &home, &["post-commit"])
        .expect("post-commit from subdir should succeed");

    // Verify attribution
    let blame_output = run_git_ai_in_with_home(&subdir, &home, &["blame", "widget.rs"])
        .expect("blame from subdirectory should succeed");

    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        blame_lines.len(),
        4,
        "Expected 4 blame lines, got:\n{}",
        blame_output
    );

    // All lines should be AI-attributed
    for (i, line) in blame_lines.iter().enumerate() {
        assert!(
            blame_line_is_ai(line),
            "Line {} should be AI-attributed: {}",
            i + 1,
            line
        );
    }
}

#[test]
fn test_nested_subdirectory_operations() {
    // Operations from deeply nested paths work correctly.
    let repo = TestRepo::new();

    // Initial commit
    let mut seed = repo.filename("README.md");
    seed.set_contents(crate::lines!["# Project".human()]);
    repo.stage_all_and_commit("initial commit").unwrap();

    // Create deeply nested directory
    let deep_dir = repo
        .path()
        .join("src")
        .join("features")
        .join("auth")
        .join("providers");
    fs::create_dir_all(&deep_dir).unwrap();

    // Write file in deeply nested directory
    let file_path = deep_dir.join("oauth.rs");
    fs::write(&file_path, "pub fn authenticate() -> bool {\n    true\n}\n").unwrap();

    // Checkpoint from deeply nested directory
    let home = repo.test_home_path();
    run_git_ai_in_with_home(&deep_dir, &home, &["checkpoint", "mock_ai", "oauth.rs"])
        .expect("checkpoint from deep subdir should succeed");

    // Stage from deep directory
    run_git_in(&deep_dir, &["add", "oauth.rs"]).unwrap();

    // Commit from deep directory
    run_git_in(&deep_dir, &["commit", "-m", "add oauth"]).unwrap();

    // Run post-commit from the deep directory
    run_git_ai_in_with_home(&deep_dir, &home, &["post-commit"])
        .expect("post-commit from deep subdir should succeed");

    // Verify blame from deep directory (relative path)
    let blame_output = run_git_ai_in_with_home(&deep_dir, &home, &["blame", "oauth.rs"])
        .expect("blame from deep subdir should succeed");

    let blame_lines: Vec<&str> = blame_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        blame_lines.len(),
        3,
        "Expected 3 blame lines, got:\n{}",
        blame_output
    );
    for (i, line) in blame_lines.iter().enumerate() {
        assert!(
            blame_line_is_ai(line),
            "Line {} should be AI-attributed: {}",
            i + 1,
            line
        );
    }

    // Also verify blame from repo root using the full relative path
    let blame_from_root = repo
        .git_ai(&["blame", "src/features/auth/providers/oauth.rs"])
        .expect("blame from root should succeed");

    let root_blame_lines: Vec<&str> = blame_from_root
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        root_blame_lines.len(),
        3,
        "Expected 3 blame lines from root, got:\n{}",
        blame_from_root
    );
    for (i, line) in root_blame_lines.iter().enumerate() {
        assert!(
            blame_line_is_ai(line),
            "Line {} from root blame should be AI-attributed: {}",
            i + 1,
            line
        );
    }

    // Verify blame from an intermediate directory (src/features/)
    let mid_dir = repo.path().join("src").join("features");
    let blame_from_mid =
        run_git_ai_in_with_home(&mid_dir, &home, &["blame", "auth/providers/oauth.rs"])
            .expect("blame from intermediate dir should succeed");

    let mid_blame_lines: Vec<&str> = blame_from_mid
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        mid_blame_lines.len(),
        3,
        "Expected 3 blame lines from intermediate dir, got:\n{}",
        blame_from_mid
    );
    for (i, line) in mid_blame_lines.iter().enumerate() {
        assert!(
            blame_line_is_ai(line),
            "Line {} from intermediate dir blame should be AI-attributed: {}",
            i + 1,
            line
        );
    }
}
