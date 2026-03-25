//! Integration tests for AI provenance tracking via bash tool pre/post snapshots.
//!
//! Each test simulates what happens when an AI coding agent executes a bash
//! command: the system takes a pre-snapshot of filesystem metadata, the bash
//! command runs, and then a post-snapshot detects which files changed. This
//! validates that the stat-diff mechanism correctly identifies created,
//! modified, and deleted files across a wide variety of real-world shell
//! commands.

use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::bash_tool::{
    BashCheckpointAction, HookEvent, diff, handle_bash_tool, snapshot,
};
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a file into the test repo, creating parent directories as needed.
fn write_file(repo: &TestRepo, rel_path: &str, contents: &str) {
    let abs = repo.path().join(rel_path);
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).expect("parent directory should be creatable");
    }
    fs::write(&abs, contents).expect("file write should succeed");
}

/// Stage and commit a file so it appears in `git ls-files` (tracked).
fn add_and_commit(repo: &TestRepo, rel_path: &str, contents: &str, message: &str) {
    write_file(repo, rel_path, contents);
    repo.git_og(&["add", rel_path])
        .expect("git add should succeed");
    repo.git_og(&["commit", "-m", message])
        .expect("git commit should succeed");
}

/// Canonical repo root path (resolves /tmp -> /private/tmp on macOS).
fn repo_root(repo: &TestRepo) -> std::path::PathBuf {
    repo.canonical_path()
}

/// Run a bash command in the repo and assert it succeeds.
fn run_bash(repo: &TestRepo, program: &str, args: &[&str]) -> std::process::Output {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo.path())
        .output()
        .unwrap_or_else(|e| panic!("{} {:?} failed to start: {}", program, args, e));
    assert!(
        output.status.success(),
        "{} {:?} failed: {}",
        program,
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Assert that a BashCheckpointAction::Checkpoint contains the expected path.
fn assert_checkpoint_contains(action: &BashCheckpointAction, expected_path: &str) {
    match action {
        BashCheckpointAction::Checkpoint(paths) => {
            assert!(
                paths.iter().any(|p| p.contains(expected_path)),
                "Expected checkpoint to contain '{}'; got {:?}",
                expected_path,
                paths
            );
        }
        BashCheckpointAction::NoChanges => {
            panic!(
                "Expected Checkpoint containing '{}', got NoChanges",
                expected_path
            );
        }
        BashCheckpointAction::TakePreSnapshot => {
            panic!("Expected Checkpoint, got TakePreSnapshot");
        }
        BashCheckpointAction::Fallback => {
            panic!("Expected Checkpoint, got Fallback");
        }
    }
}

/// Assert that a BashCheckpointAction::Checkpoint does NOT contain a path.
fn assert_checkpoint_excludes(action: &BashCheckpointAction, excluded_path: &str) {
    if let BashCheckpointAction::Checkpoint(paths) = action {
        assert!(
            !paths.iter().any(|p| p.contains(excluded_path)),
            "Expected checkpoint NOT to contain '{}'; got {:?}",
            excluded_path,
            paths
        );
    }
}

/// Assert that a BashCheckpointAction is NoChanges.
fn assert_no_changes(action: &BashCheckpointAction) {
    assert!(
        matches!(action, BashCheckpointAction::NoChanges),
        "Expected NoChanges, got {:?}",
        match action {
            BashCheckpointAction::TakePreSnapshot => "TakePreSnapshot",
            BashCheckpointAction::Checkpoint(p) => {
                panic!("Expected NoChanges, got Checkpoint({:?})", p);
            }
            BashCheckpointAction::NoChanges => "NoChanges",
            BashCheckpointAction::Fallback => "Fallback",
        }
    );
}

/// Get the checkpoint paths from an action, panicking if not a Checkpoint.
fn checkpoint_paths(action: &BashCheckpointAction) -> &[String] {
    match action {
        BashCheckpointAction::Checkpoint(paths) => paths,
        other => panic!(
            "Expected Checkpoint, got {:?}",
            match other {
                BashCheckpointAction::TakePreSnapshot => "TakePreSnapshot",
                BashCheckpointAction::NoChanges => "NoChanges",
                BashCheckpointAction::Fallback => "Fallback",
                BashCheckpointAction::Checkpoint(_) => unreachable!(),
            }
        ),
    }
}

// ===========================================================================
// Category 1: File creation commands
// ===========================================================================

#[test]
fn test_bash_provenance_echo_redirect_creates_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    let pre_action = handle_bash_tool(HookEvent::PreToolUse, &root, "echo-sess", "echo-t1")
        .expect("PreToolUse should succeed");
    assert!(matches!(pre_action, BashCheckpointAction::TakePreSnapshot));

    run_bash(&repo, "sh", &["-c", "echo 'hello world' > created.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "echo-sess", "echo-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "created.txt");
}

#[test]
fn test_bash_provenance_printf_redirect_creates_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "printf-sess", "printf-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &["-c", "printf 'formatted content' > printf_out.txt"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "printf-sess", "printf-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "printf_out.txt");
}

#[test]
fn test_bash_provenance_heredoc_creates_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "heredoc-sess", "heredoc-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "cat > heredoc.txt <<'EOF'\nheredoc content\nline two\nEOF",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "heredoc-sess", "heredoc-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "heredoc.txt");
}

#[test]
fn test_bash_provenance_touch_creates_empty_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "touch-sess", "touch-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "touch", &["newfile.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "touch-sess", "touch-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "newfile.txt");
}

#[test]
fn test_bash_provenance_cp_creates_copy() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "existing.txt", "original content", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "cp-sess", "cp-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "cp", &["existing.txt", "copy.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "cp-sess", "cp-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "copy.txt");
    assert_checkpoint_excludes(&post_action, "existing.txt");
}

#[test]
fn test_bash_provenance_tee_creates_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "tee-sess", "tee-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &["-c", "echo content | tee output.txt > /dev/null"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "tee-sess", "tee-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "output.txt");
}

#[test]
fn test_bash_provenance_nested_directory_creation() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "nested-sess", "nested-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "mkdir -p src/deep/nested && touch src/deep/nested/mod.rs",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "nested-sess", "nested-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "mod.rs");
}

// ===========================================================================
// Category 2: File modification commands
// ===========================================================================

#[test]
fn test_bash_provenance_sed_in_place_edit() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "target.txt", "old value here", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "sed-sess", "sed-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "sed -i.bak 's/old/new/g' target.txt && rm -f target.txt.bak",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "sed-sess", "sed-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "target.txt");
}

#[test]
fn test_bash_provenance_append_with_redirect() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "log.txt", "line one\n", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "append-sess", "append-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    run_bash(&repo, "sh", &["-c", "echo 'appended line' >> log.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "append-sess", "append-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "log.txt");
}

#[test]
fn test_bash_provenance_truncate_to_zero() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "data.txt",
        "lots of data here that will be erased",
        "initial commit",
    );

    handle_bash_tool(HookEvent::PreToolUse, &root, "trunc-sess", "trunc-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    run_bash(&repo, "sh", &["-c", ": > data.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "trunc-sess", "trunc-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "data.txt");
}

#[cfg(unix)]
#[test]
fn test_bash_provenance_chmod_permission_change() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "script.sh", "#!/bin/bash\necho hi", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "chmod-sess", "chmod-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "chmod", &["+x", "script.sh"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "chmod-sess", "chmod-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "script.sh");
}

#[test]
fn test_bash_provenance_mv_rename() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "old_name.txt", "rename me", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "mv-sess", "mv-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "mv", &["old_name.txt", "new_name.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "mv-sess", "mv-t1")
        .expect("PostToolUse should succeed");

    // Rename shows as delete of old + create of new
    let paths = checkpoint_paths(&post_action);
    assert!(
        paths.iter().any(|p| p.contains("old_name.txt")),
        "old_name.txt should appear in checkpoint paths (as deleted); got {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("new_name.txt")),
        "new_name.txt should appear in checkpoint paths (as created); got {:?}",
        paths
    );
}

// ===========================================================================
// Category 3: File deletion commands
// ===========================================================================

#[test]
fn test_bash_provenance_rm_simple() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "doomed.txt", "bye bye", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "rm-sess", "rm-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "rm", &["doomed.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "rm-sess", "rm-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "doomed.txt");
}

#[test]
fn test_bash_provenance_rm_rf_directory() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "subdir/a.txt", "file a", "add a");
    add_and_commit(&repo, "subdir/b.txt", "file b", "add b");
    add_and_commit(&repo, "subdir/deep/c.txt", "file c", "add c");

    handle_bash_tool(HookEvent::PreToolUse, &root, "rmrf-sess", "rmrf-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "rm", &["-rf", "subdir"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "rmrf-sess", "rmrf-t1")
        .expect("PostToolUse should succeed");
    let paths = checkpoint_paths(&post_action);
    assert!(
        paths.iter().any(|p| p.contains("a.txt")),
        "subdir/a.txt should appear in checkpoint after rm -rf; got {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("b.txt")),
        "subdir/b.txt should appear in checkpoint after rm -rf; got {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("c.txt")),
        "subdir/deep/c.txt should appear in checkpoint after rm -rf; got {:?}",
        paths
    );
}

// ===========================================================================
// Category 4: Build/compile tool simulations
// ===========================================================================

#[test]
fn test_bash_provenance_simulated_cargo_init() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "cargo-sess", "cargo-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "mkdir -p myproject/src && echo 'fn main() {}' > myproject/src/main.rs && printf '[package]\\nname=\"myproject\"' > myproject/Cargo.toml",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "cargo-sess", "cargo-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "main.rs");
    // On macOS, paths are case-normalized to lowercase, so check for lowercase.
    let paths = checkpoint_paths(&post_action);
    assert!(
        paths
            .iter()
            .any(|p| p.to_lowercase().contains("cargo.toml")),
        "Cargo.toml (case-insensitive) should appear in checkpoint; got {:?}",
        paths
    );
}

#[test]
fn test_bash_provenance_simulated_npm_init() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "npm-sess", "npm-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            r#"echo '{"name":"test","version":"1.0.0"}' > package.json"#,
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "npm-sess", "npm-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "package.json");
}

// ===========================================================================
// Category 5: Git commands (that modify working tree)
// ===========================================================================

#[test]
fn test_bash_provenance_git_checkout_restore() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "restorable.txt",
        "original content",
        "initial commit",
    );

    // Modify the file so git checkout -- will revert it
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "restorable.txt", "modified content");

    handle_bash_tool(HookEvent::PreToolUse, &root, "checkout-sess", "checkout-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    // Use git_og to bypass hooks, simulating what a bash command would do
    repo.git_og(&["checkout", "--", "restorable.txt"])
        .expect("git checkout should succeed");

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "checkout-sess",
        "checkout-t1",
    )
    .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "restorable.txt");
}

#[test]
fn test_bash_provenance_git_stash_pop() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "stashed.txt", "original", "initial commit");

    // Modify and stash
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "stashed.txt", "modified for stash");
    repo.git_og(&["add", "stashed.txt"])
        .expect("git add should succeed");
    repo.git_og(&["stash", "push", "-m", "test stash"])
        .expect("git stash should succeed");

    handle_bash_tool(HookEvent::PreToolUse, &root, "stash-sess", "stash-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    repo.git_og(&["stash", "pop"])
        .expect("git stash pop should succeed");

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "stash-sess", "stash-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "stashed.txt");
}

#[test]
fn test_bash_provenance_git_apply_patch() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "patchme.txt",
        "line one\nline two\nline three\n",
        "initial",
    );

    // Create a patch file
    let patch_content = "\
--- a/patchme.txt
+++ b/patchme.txt
@@ -1,3 +1,3 @@
 line one
-line two
+line TWO PATCHED
 line three
";
    write_file(&repo, "fix.patch", patch_content);

    handle_bash_tool(HookEvent::PreToolUse, &root, "patch-sess", "patch-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    repo.git_og(&["apply", "fix.patch"])
        .expect("git apply should succeed");

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "patch-sess", "patch-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "patchme.txt");
}

// ===========================================================================
// Category 6: Multi-command pipelines
// ===========================================================================

#[test]
fn test_bash_provenance_find_and_delete_bak_files() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "keep.txt", "keep this", "initial");
    add_and_commit(&repo, "one.bak", "backup one", "add bak 1");
    add_and_commit(&repo, "two.bak", "backup two", "add bak 2");
    add_and_commit(&repo, "sub/three.bak", "backup three", "add bak 3");

    handle_bash_tool(HookEvent::PreToolUse, &root, "finddel-sess", "finddel-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &["-c", "find . -name '*.bak' -not -path './.git/*' -delete"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "finddel-sess", "finddel-t1")
        .expect("PostToolUse should succeed");
    let paths = checkpoint_paths(&post_action);
    assert!(
        paths.iter().any(|p| p.contains("one.bak")),
        "one.bak should appear in checkpoint; got {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("two.bak")),
        "two.bak should appear in checkpoint; got {:?}",
        paths
    );
    assert!(
        paths.iter().any(|p| p.contains("three.bak")),
        "three.bak should appear in checkpoint; got {:?}",
        paths
    );
    assert_checkpoint_excludes(&post_action, "keep.txt");
}

#[test]
fn test_bash_provenance_loop_creating_multiple_files() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "loop-sess", "loop-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "for f in a.txt b.txt c.txt; do echo 'content' > $f; done",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "loop-sess", "loop-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "a.txt");
    assert_checkpoint_contains(&post_action, "b.txt");
    assert_checkpoint_contains(&post_action, "c.txt");
}

#[test]
fn test_bash_provenance_grep_sed_pipeline() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "file1.txt", "old pattern here", "add file1");
    add_and_commit(&repo, "file2.txt", "old pattern there", "add file2");
    add_and_commit(&repo, "file3.txt", "no match", "add file3");

    handle_bash_tool(HookEvent::PreToolUse, &root, "pipeline-sess", "pipeline-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "grep -rl 'old' --include='*.txt' . | xargs sed -i.bak 's/old/new/g' && find . -name '*.bak' -delete",
        ],
    );

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "pipeline-sess",
        "pipeline-t1",
    )
    .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "file1.txt");
    assert_checkpoint_contains(&post_action, "file2.txt");
    assert_checkpoint_excludes(&post_action, "file3.txt");
}

// ===========================================================================
// Category 7: Read-only commands (should produce NoChanges)
// ===========================================================================

#[test]
fn test_bash_provenance_cat_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "readable.txt", "read me", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "cat-sess", "cat-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "cat", &["readable.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "cat-sess", "cat-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_ls_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "visible.txt", "content", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "ls-sess", "ls-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "ls", &["-la"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "ls-sess", "ls-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_find_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "src/main.rs", "fn main() {}", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "find-sess", "find-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "find", &[".", "-name", "*.rs"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "find-sess", "find-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_grep_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "searchable.txt",
        "pattern match here",
        "initial commit",
    );

    handle_bash_tool(HookEvent::PreToolUse, &root, "grep-sess", "grep-t1")
        .expect("PreToolUse should succeed");

    // grep may exit non-zero if no match, so use sh -c with || true
    run_bash(
        &repo,
        "sh",
        &["-c", "grep 'pattern' searchable.txt || true"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "grep-sess", "grep-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_wc_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "countme.txt", "one\ntwo\nthree\n", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "wc-sess", "wc-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "wc", &["-l", "countme.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "wc-sess", "wc-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_head_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "longfile.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\n",
        "initial commit",
    );

    handle_bash_tool(HookEvent::PreToolUse, &root, "head-sess", "head-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "head", &["-5", "longfile.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "head-sess", "head-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_diff_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "file1.txt", "alpha\nbeta\n", "add file1");
    add_and_commit(&repo, "file2.txt", "alpha\ngamma\n", "add file2");

    handle_bash_tool(HookEvent::PreToolUse, &root, "diff-sess", "diff-t1")
        .expect("PreToolUse should succeed");

    // diff returns non-zero when files differ, so use || true
    run_bash(&repo, "sh", &["-c", "diff file1.txt file2.txt || true"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "diff-sess", "diff-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_git_log_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "gitlog-sess", "gitlog-t1")
        .expect("PreToolUse should succeed");

    repo.git_og(&["log", "--oneline"])
        .expect("git log should succeed");

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "gitlog-sess", "gitlog-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_git_diff_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "gitdiff-sess", "gitdiff-t1")
        .expect("PreToolUse should succeed");

    repo.git_og(&["diff"]).expect("git diff should succeed");

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "gitdiff-sess", "gitdiff-t1")
        .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_git_status_is_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(
        HookEvent::PreToolUse,
        &root,
        "gitstatus-sess",
        "gitstatus-t1",
    )
    .expect("PreToolUse should succeed");

    repo.git_og(&["status"]).expect("git status should succeed");

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "gitstatus-sess",
        "gitstatus-t1",
    )
    .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

#[test]
fn test_bash_provenance_compound_readonly() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "compound-sess", "compound-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "sh", &["-c", "pwd && ls"]);

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "compound-sess",
        "compound-t1",
    )
    .expect("PostToolUse should succeed");
    assert_no_changes(&post_action);
}

// ===========================================================================
// Category 8: Symlink operations (unix only)
// ===========================================================================

#[cfg(unix)]
#[test]
fn test_bash_provenance_symlink_creation() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "target.txt", "symlink target", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "symlink-sess", "symlink-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "ln", &["-s", "target.txt", "link.txt"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "symlink-sess", "symlink-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "link.txt");
}

#[cfg(unix)]
#[test]
fn test_bash_provenance_symlink_target_change() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "target_a.txt", "target a", "add target a");
    add_and_commit(&repo, "target_b.txt", "target b", "add target b");

    // Create the symlink pointing to target_a
    run_bash(&repo, "ln", &["-s", "target_a.txt", "mylink.txt"]);
    // Commit the symlink so it is tracked
    repo.git_og(&["add", "mylink.txt"])
        .expect("git add symlink should succeed");
    repo.git_og(&["commit", "-m", "add symlink"])
        .expect("git commit symlink should succeed");

    handle_bash_tool(HookEvent::PreToolUse, &root, "symtgt-sess", "symtgt-t1")
        .expect("PreToolUse should succeed");

    // Re-point the symlink to target_b
    run_bash(
        &repo,
        "sh",
        &["-c", "rm mylink.txt && ln -s target_b.txt mylink.txt"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "symtgt-sess", "symtgt-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "mylink.txt");
}

// ===========================================================================
// Category 9: Large/batch operations
// ===========================================================================

#[test]
fn test_bash_provenance_create_50_files() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "batch50-sess", "batch50-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "for i in $(seq 1 50); do echo \"file $i\" > \"batch_$i.txt\"; done",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "batch50-sess", "batch50-t1")
        .expect("PostToolUse should succeed");
    let paths = checkpoint_paths(&post_action);
    assert!(
        paths.len() >= 50,
        "Expected at least 50 created files in checkpoint; got {} paths: {:?}",
        paths.len(),
        paths
    );
    // Spot-check a few
    assert_checkpoint_contains(&post_action, "batch_1.txt");
    assert_checkpoint_contains(&post_action, "batch_25.txt");
    assert_checkpoint_contains(&post_action, "batch_50.txt");
}

#[test]
fn test_bash_provenance_modify_20_of_50_tracked() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Create and commit 50 files
    for i in 1..=50 {
        let name = format!("tracked_{}.txt", i);
        add_and_commit(
            &repo,
            &name,
            &format!("original {}", i),
            &format!("add {}", name),
        );
    }

    handle_bash_tool(HookEvent::PreToolUse, &root, "mod20-sess", "mod20-t1")
        .expect("PreToolUse should succeed");

    thread::sleep(Duration::from_millis(50));
    // Modify only files 1-20
    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "for i in $(seq 1 20); do echo 'modified' > \"tracked_$i.txt\"; done",
        ],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "mod20-sess", "mod20-t1")
        .expect("PostToolUse should succeed");
    let paths = checkpoint_paths(&post_action);

    // Exactly 20 files should be modified
    assert_eq!(
        paths.len(),
        20,
        "Expected exactly 20 modified files; got {} paths: {:?}",
        paths.len(),
        paths
    );

    // Verify modified files are present
    assert_checkpoint_contains(&post_action, "tracked_1.txt");
    assert_checkpoint_contains(&post_action, "tracked_20.txt");

    // Verify unmodified files are NOT present
    assert_checkpoint_excludes(&post_action, "tracked_21.txt");
    assert_checkpoint_excludes(&post_action, "tracked_50.txt");
}

// ===========================================================================
// Category 10: Edge cases
// ===========================================================================

#[test]
fn test_bash_provenance_failed_command_with_partial_output() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "fail-sess", "fail-t1")
        .expect("PreToolUse should succeed");

    // Command that creates a file then fails. We use || true so run_bash
    // does not panic, but the file is still created.
    run_bash(
        &repo,
        "sh",
        &["-c", "echo 'partial' > partial.txt && false || true"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "fail-sess", "fail-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "partial.txt");
}

#[test]
fn test_bash_provenance_file_with_spaces_in_name() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "spaces-sess", "spaces-t1")
        .expect("PreToolUse should succeed");

    run_bash(&repo, "sh", &["-c", "echo 'x' > 'file with spaces.txt'"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "spaces-sess", "spaces-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "file with spaces.txt");
}

#[test]
fn test_bash_provenance_file_with_special_characters() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "special-sess", "special-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &["-c", "echo 'x' > 'file-with-dashes_and_underscores.txt'"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "special-sess", "special-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "file-with-dashes_and_underscores.txt");
}

#[test]
fn test_bash_provenance_hidden_file_creation() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(HookEvent::PreToolUse, &root, "hidden-sess", "hidden-t1")
        .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &["-c", "echo 'secret config' > .hidden_config"],
    );

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "hidden-sess", "hidden-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, ".hidden_config");
}

#[test]
fn test_bash_provenance_touch_then_write_shows_created() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    handle_bash_tool(
        HookEvent::PreToolUse,
        &root,
        "touchwrite-sess",
        "touchwrite-t1",
    )
    .expect("PreToolUse should succeed");

    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "touch empty.txt && echo 'now has content' > empty.txt",
        ],
    );

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "touchwrite-sess",
        "touchwrite-t1",
    )
    .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "empty.txt");
}

#[test]
fn test_bash_provenance_overwrite_identical_content_detects_mtime_change() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "same.txt", "identical", "initial commit");

    handle_bash_tool(
        HookEvent::PreToolUse,
        &root,
        "identical-sess",
        "identical-t1",
    )
    .expect("PreToolUse should succeed");

    // Wait so mtime advances even though content is the same
    thread::sleep(Duration::from_millis(50));
    // Write exact same content but file metadata (mtime) will change
    run_bash(&repo, "sh", &["-c", "echo 'identical' > same.txt"]);

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "identical-sess",
        "identical-t1",
    )
    .expect("PostToolUse should succeed");
    // The stat tuple should differ because mtime changed, even if content is the same.
    // Note: echo adds a trailing newline, so content actually differs from "identical"
    // to "identical\n". Regardless, the stat-tuple approach detects this.
    assert_checkpoint_contains(&post_action, "same.txt");
}

#[test]
fn test_bash_provenance_sequential_tool_uses_same_session() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    // --- First cycle: create alpha.txt ---
    let pre1 = handle_bash_tool(HookEvent::PreToolUse, &root, "seq-sess", "seq-use1")
        .expect("PreToolUse 1 should succeed");
    assert!(
        matches!(pre1, BashCheckpointAction::TakePreSnapshot),
        "First PreToolUse should return TakePreSnapshot"
    );

    run_bash(&repo, "sh", &["-c", "echo 'alpha' > alpha.txt"]);

    let post1 = handle_bash_tool(HookEvent::PostToolUse, &root, "seq-sess", "seq-use1")
        .expect("PostToolUse 1 should succeed");
    assert_checkpoint_contains(&post1, "alpha.txt");
    assert_checkpoint_excludes(&post1, "beta.txt");

    // --- Second cycle: create beta.txt ---
    let pre2 = handle_bash_tool(HookEvent::PreToolUse, &root, "seq-sess", "seq-use2")
        .expect("PreToolUse 2 should succeed");
    assert!(
        matches!(pre2, BashCheckpointAction::TakePreSnapshot),
        "Second PreToolUse should return TakePreSnapshot"
    );

    run_bash(&repo, "sh", &["-c", "echo 'beta' > beta.txt"]);

    let post2 = handle_bash_tool(HookEvent::PostToolUse, &root, "seq-sess", "seq-use2")
        .expect("PostToolUse 2 should succeed");
    assert_checkpoint_contains(&post2, "beta.txt");
    // alpha.txt was created in the first cycle; it should NOT appear in the second
    // cycle since the second pre-snapshot includes it.
    assert_checkpoint_excludes(&post2, "alpha.txt");
}

// ===========================================================================
// Category 11: Tar/archive operations
// ===========================================================================

#[test]
fn test_bash_provenance_create_tarball() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "archive/one.txt", "one", "add one");
    add_and_commit(&repo, "archive/two.txt", "two", "add two");

    handle_bash_tool(
        HookEvent::PreToolUse,
        &root,
        "tar-create-sess",
        "tar-create-t1",
    )
    .expect("PreToolUse should succeed");

    run_bash(&repo, "tar", &["czf", "archive.tar.gz", "archive"]);

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "tar-create-sess",
        "tar-create-t1",
    )
    .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "archive.tar.gz");
}

#[test]
fn test_bash_provenance_extract_tarball() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "pkg/alpha.txt", "alpha", "add alpha");
    add_and_commit(&repo, "pkg/beta.txt", "beta", "add beta");

    // Create the tarball first
    run_bash(&repo, "tar", &["czf", "pkg.tar.gz", "pkg"]);
    repo.git_og(&["add", "pkg.tar.gz"])
        .expect("git add tarball should succeed");
    repo.git_og(&["commit", "-m", "add tarball"])
        .expect("git commit tarball should succeed");

    // Remove original directory
    run_bash(&repo, "rm", &["-rf", "pkg"]);
    repo.git_og(&["add", "-A"])
        .expect("git add removal should succeed");
    repo.git_og(&["commit", "-m", "remove pkg dir"])
        .expect("git commit removal should succeed");

    handle_bash_tool(
        HookEvent::PreToolUse,
        &root,
        "tar-extract-sess",
        "tar-extract-t1",
    )
    .expect("PreToolUse should succeed");

    run_bash(&repo, "tar", &["xzf", "pkg.tar.gz"]);

    let post_action = handle_bash_tool(
        HookEvent::PostToolUse,
        &root,
        "tar-extract-sess",
        "tar-extract-t1",
    )
    .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "alpha.txt");
    assert_checkpoint_contains(&post_action, "beta.txt");
}

// ===========================================================================
// Category 12: Compiler/tool output simulation
// ===========================================================================

#[test]
fn test_bash_provenance_simulated_compile() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(
        &repo,
        "hello.c",
        "#include <stdio.h>\nint main() { printf(\"hello\\n\"); return 0; }\n",
        "initial commit",
    );

    handle_bash_tool(HookEvent::PreToolUse, &root, "compile-sess", "compile-t1")
        .expect("PreToolUse should succeed");

    // Simulate compilation by creating an output binary
    run_bash(&repo, "sh", &["-c", "echo 'compiled binary' > hello"]);

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "compile-sess", "compile-t1")
        .expect("PostToolUse should succeed");
    assert_checkpoint_contains(&post_action, "hello");
}

// ===========================================================================
// Additional: Direct snapshot/diff API tests with real commands
// ===========================================================================

#[test]
fn test_bash_provenance_snapshot_diff_echo_redirect() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "init.txt", "seed", "initial commit");

    let pre = snapshot(&root, "snap-echo", "t1").expect("pre-snapshot should succeed");

    run_bash(&repo, "sh", &["-c", "echo 'snap test' > snap_created.txt"]);

    let post = snapshot(&root, "snap-echo", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        created.iter().any(|p| p.contains("snap_created.txt")),
        "snap_created.txt should appear in created via direct snapshot/diff; got {:?}",
        created
    );
    assert!(
        result.modified.is_empty(),
        "no files should be modified; got {:?}",
        result.modified
    );
    assert!(
        result.deleted.is_empty(),
        "no files should be deleted; got {:?}",
        result.deleted
    );
}

#[test]
fn test_bash_provenance_snapshot_diff_sed_modification() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "editable.txt", "old text old text", "initial commit");

    let pre = snapshot(&root, "snap-sed", "t1").expect("pre-snapshot should succeed");

    thread::sleep(Duration::from_millis(50));
    run_bash(
        &repo,
        "sh",
        &[
            "-c",
            "sed -i.bak 's/old/new/g' editable.txt && rm -f editable.txt.bak",
        ],
    );

    let post = snapshot(&root, "snap-sed", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let modified: Vec<String> = result
        .modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        modified.iter().any(|p| p.contains("editable.txt")),
        "editable.txt should appear in modified via direct snapshot/diff; got {:?}",
        modified
    );
}

#[test]
fn test_bash_provenance_snapshot_diff_rm_deletion() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);
    add_and_commit(&repo, "removable.txt", "remove me", "initial commit");

    let pre = snapshot(&root, "snap-rm", "t1").expect("pre-snapshot should succeed");

    run_bash(&repo, "rm", &["removable.txt"]);

    let post = snapshot(&root, "snap-rm", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let deleted: Vec<String> = result
        .deleted
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        deleted.iter().any(|p| p.contains("removable.txt")),
        "removable.txt should appear in deleted via direct snapshot/diff; got {:?}",
        deleted
    );
}
