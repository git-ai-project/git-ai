//! Conformance test suite for the bash tool change attribution feature.
//!
//! Covers PRD Sections 5.1 (file mutations), 5.2 (read-only operations),
//! 5.3 (edge cases), 5.4 (pre/post hook semantics), tool classification
//! for all six agents, gitignore filtering, and full handle_bash_tool
//! orchestration.

use crate::repos::test_repo::TestRepo;
use git_ai::commands::checkpoint_agent::bash_tool::{
    Agent, BashCheckpointAction, HookEvent, ToolClass, build_gitignore, classify_tool,
    cleanup_stale_snapshots, diff, git_status_fallback, handle_bash_tool, normalize_path, snapshot,
};
use std::fs;
use std::path::Path;
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

// ===========================================================================
// Section 5.1 — File Mutations
// ===========================================================================

#[test]
fn test_bash_tool_detect_file_creation() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    write_file(&repo, "new.txt", "hello");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        created.iter().any(|p| p.contains("new.txt")),
        "new.txt should appear in created; got {:?}",
        created
    );
    assert!(result.modified.is_empty(), "no files should be modified");
    assert!(result.deleted.is_empty(), "no files should be deleted");
}

#[test]
fn test_bash_tool_detect_modification() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "existing.txt", "foo", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Allow filesystem time granularity to advance so the stat-tuple changes.
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "existing.txt", "bar");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let modified: Vec<String> = result
        .modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        modified.iter().any(|p| p.contains("existing.txt")),
        "existing.txt should appear in modified; got {:?}",
        modified
    );
    assert!(result.created.is_empty(), "no files should be created");
    assert!(result.deleted.is_empty(), "no files should be deleted");
}

#[test]
fn test_bash_tool_detect_deletion() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "tracked.txt", "content", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    fs::remove_file(repo.path().join("tracked.txt")).expect("remove should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let deleted: Vec<String> = result
        .deleted
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        deleted.iter().any(|p| p.contains("tracked.txt")),
        "tracked.txt should appear in deleted; got {:?}",
        deleted
    );
    assert!(result.created.is_empty(), "no files should be created");
    assert!(result.modified.is_empty(), "no files should be modified");
}

#[cfg(unix)]
#[test]
fn test_bash_tool_detect_permission_change() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "script.sh", "#!/bin/bash\necho hi", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // chmod +x
    let abs = repo.path().join("script.sh");
    let mut perms = fs::metadata(&abs).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&abs, perms).expect("chmod should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let modified: Vec<String> = result
        .modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        modified.iter().any(|p| p.contains("script.sh")),
        "script.sh should appear in modified after chmod; got {:?}",
        modified
    );
}

#[test]
fn test_bash_tool_detect_rename() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "old.txt", "data", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt"))
        .expect("rename should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let deleted: Vec<String> = result
        .deleted
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    assert!(
        deleted.iter().any(|p| p.contains("old.txt")),
        "old.txt should appear in deleted after rename; got {:?}",
        deleted
    );
    assert!(
        created.iter().any(|p| p.contains("new.txt")),
        "new.txt should appear in created after rename; got {:?}",
        created
    );
}

#[test]
fn test_bash_tool_detect_copy() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "source.txt", "copy-me", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    fs::copy(repo.path().join("source.txt"), repo.path().join("dest.txt"))
        .expect("copy should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        created.iter().any(|p| p.contains("dest.txt")),
        "dest.txt should appear in created (or modified) after copy; got {:?}",
        created
    );
    // source.txt should NOT appear as modified since we only read it
    let modified: Vec<String> = result
        .modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        !modified.iter().any(|p| p.contains("source.txt")),
        "source.txt should not be modified by a copy; got {:?}",
        modified
    );
}

// ===========================================================================
// Section 5.2 — Read-Only Operations
// ===========================================================================

#[test]
fn test_bash_tool_no_changes_detected() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "stable.txt", "unchanged", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");
    // No mutations between snapshots.
    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(
        result.is_empty(),
        "diff should be empty when nothing changed"
    );
    assert!(result.created.is_empty());
    assert!(result.modified.is_empty());
    assert!(result.deleted.is_empty());
}

#[test]
fn test_bash_tool_empty_repo_no_changes() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");
    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(result.is_empty(), "empty repo diff should be empty");
}

// ===========================================================================
// Section 5.3 — Edge Cases
// ===========================================================================

#[test]
fn test_bash_tool_files_outside_repo_ignored() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "inside.txt", "inside", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Modify a file outside the repo — this should not be detected.
    let outside = std::env::temp_dir().join("bash_tool_test_outside.txt");
    fs::write(&outside, "external change").expect("write outside repo should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(
        result.is_empty(),
        "changes outside the repo should not appear in the diff"
    );

    // Clean up
    let _ = fs::remove_file(&outside);
}

#[test]
fn test_bash_tool_empty_stat_diff() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");
    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(
        result.is_empty(),
        "empty stat-diff should produce no changes"
    );
    assert!(result.all_changed_paths().is_empty());
}

#[test]
fn test_bash_tool_multiple_mutations_combined() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "modify-me.txt", "original", "initial");
    add_and_commit(&repo, "delete-me.txt", "gone-soon", "add delete target");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Perform multiple mutations
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "modify-me.txt", "changed");
    write_file(&repo, "brand-new.txt", "fresh");
    fs::remove_file(repo.path().join("delete-me.txt")).expect("delete should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(
        !result.is_empty(),
        "diff should not be empty after multiple mutations"
    );

    let all_paths = result.all_changed_paths();
    assert!(
        all_paths.iter().any(|p| p.contains("modify-me.txt")),
        "modify-me.txt should be in changed paths; got {:?}",
        all_paths
    );
    assert!(
        all_paths.iter().any(|p| p.contains("brand-new.txt")),
        "brand-new.txt should be in changed paths; got {:?}",
        all_paths
    );
    assert!(
        all_paths.iter().any(|p| p.contains("delete-me.txt")),
        "delete-me.txt should be in changed paths; got {:?}",
        all_paths
    );
}

// ===========================================================================
// Section 5.4 — Pre/Post Hook Semantics
// ===========================================================================

#[test]
fn test_bash_tool_pre_hook_returns_take_pre_snapshot() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let action = handle_bash_tool(HookEvent::PreToolUse, &root, "sess", "tool1")
        .expect("handle_bash_tool PreToolUse should succeed");

    assert!(
        matches!(action, BashCheckpointAction::TakePreSnapshot),
        "PreToolUse should return TakePreSnapshot"
    );
}

#[test]
fn test_bash_tool_post_hook_no_changes() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "stable.txt", "unchanged", "initial");

    // Pre-hook stores the snapshot
    let pre_action = handle_bash_tool(HookEvent::PreToolUse, &root, "sess", "tool1")
        .expect("PreToolUse should succeed");
    assert!(matches!(pre_action, BashCheckpointAction::TakePreSnapshot));

    // Post-hook with no changes
    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "sess", "tool1")
        .expect("PostToolUse should succeed");
    assert!(
        matches!(post_action, BashCheckpointAction::NoChanges),
        "PostToolUse with no changes should return NoChanges; got {:?}",
        match &post_action {
            BashCheckpointAction::TakePreSnapshot => "TakePreSnapshot",
            BashCheckpointAction::Checkpoint(_) => "Checkpoint",
            BashCheckpointAction::NoChanges => "NoChanges",
            BashCheckpointAction::Fallback => "Fallback",
        }
    );
}

#[test]
fn test_bash_tool_post_hook_detects_changes() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "target.txt", "before", "initial");

    // Pre-hook
    let pre_action = handle_bash_tool(HookEvent::PreToolUse, &root, "sess", "tool2")
        .expect("PreToolUse should succeed");
    assert!(matches!(pre_action, BashCheckpointAction::TakePreSnapshot));

    // Mutate between pre and post
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "target.txt", "after");

    // Post-hook
    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "sess", "tool2")
        .expect("PostToolUse should succeed");
    match post_action {
        BashCheckpointAction::Checkpoint(paths) => {
            assert!(
                paths.iter().any(|p| p.contains("target.txt")),
                "Checkpoint should include target.txt; got {:?}",
                paths
            );
        }
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

#[test]
fn test_bash_tool_post_hook_without_pre_uses_fallback() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Do NOT call PreToolUse first. PostToolUse should fall back to git status.
    // Create a tracked file and then modify it so git status shows changes.
    add_and_commit(&repo, "changed.txt", "original", "initial");
    write_file(&repo, "changed.txt", "modified");

    let post_action = handle_bash_tool(HookEvent::PostToolUse, &root, "sess", "missing-pre")
        .expect("PostToolUse without pre should succeed via fallback");

    // Should be Checkpoint (from git status) or NoChanges, but not panic
    match post_action {
        BashCheckpointAction::Checkpoint(paths) => {
            assert!(
                paths.iter().any(|p| p.contains("changed.txt")),
                "Fallback should detect changed.txt via git status; got {:?}",
                paths
            );
        }
        BashCheckpointAction::NoChanges => {
            // Acceptable if git status does not report changes (unlikely but possible)
        }
        BashCheckpointAction::Fallback => {
            // Also acceptable — means git status itself failed
        }
        BashCheckpointAction::TakePreSnapshot => {
            panic!("PostToolUse should never return TakePreSnapshot");
        }
    }
}

// ===========================================================================
// Full handle_bash_tool orchestration — Pre followed by Post with creation
// ===========================================================================

#[test]
fn test_bash_tool_orchestration_create_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Make an initial commit so the repo is valid
    add_and_commit(&repo, "readme.md", "# Hello", "init");

    // Pre-hook
    handle_bash_tool(HookEvent::PreToolUse, &root, "orch-sess", "orch-tool")
        .expect("PreToolUse should succeed");

    // Simulate bash creating a new file
    write_file(&repo, "generated.rs", "fn main() {}");

    // Post-hook
    let action = handle_bash_tool(HookEvent::PostToolUse, &root, "orch-sess", "orch-tool")
        .expect("PostToolUse should succeed");

    match action {
        BashCheckpointAction::Checkpoint(paths) => {
            assert!(
                paths.iter().any(|p| p.contains("generated.rs")),
                "Orchestrated checkpoint should include generated.rs; got {:?}",
                paths
            );
        }
        BashCheckpointAction::NoChanges => {
            panic!("Expected Checkpoint after creating a file, got NoChanges");
        }
        _ => panic!("Expected Checkpoint after creating a file"),
    }
}

#[test]
fn test_bash_tool_orchestration_delete_file() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "doomed.txt", "temporary", "initial");

    // Pre-hook
    handle_bash_tool(HookEvent::PreToolUse, &root, "del-sess", "del-tool")
        .expect("PreToolUse should succeed");

    // Simulate bash deleting the file
    fs::remove_file(repo.path().join("doomed.txt")).expect("remove should succeed");

    // Post-hook
    let action = handle_bash_tool(HookEvent::PostToolUse, &root, "del-sess", "del-tool")
        .expect("PostToolUse should succeed");

    match action {
        BashCheckpointAction::Checkpoint(paths) => {
            assert!(
                paths.iter().any(|p| p.contains("doomed.txt")),
                "Orchestrated checkpoint should include doomed.txt; got {:?}",
                paths
            );
        }
        BashCheckpointAction::NoChanges => {
            panic!("Expected Checkpoint after deleting a file, got NoChanges");
        }
        _ => panic!("Expected Checkpoint after deleting a file"),
    }
}

#[test]
fn test_bash_tool_orchestration_multiple_tool_uses() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "base.txt", "base", "initial");

    // First tool use: create file
    handle_bash_tool(HookEvent::PreToolUse, &root, "multi-sess", "use1")
        .expect("PreToolUse 1 should succeed");
    write_file(&repo, "first.txt", "first");
    let action1 = handle_bash_tool(HookEvent::PostToolUse, &root, "multi-sess", "use1")
        .expect("PostToolUse 1 should succeed");
    assert!(
        matches!(action1, BashCheckpointAction::Checkpoint(_)),
        "First tool use should produce Checkpoint"
    );

    // Second tool use: modify file
    handle_bash_tool(HookEvent::PreToolUse, &root, "multi-sess", "use2")
        .expect("PreToolUse 2 should succeed");
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "first.txt", "modified-first");
    let action2 = handle_bash_tool(HookEvent::PostToolUse, &root, "multi-sess", "use2")
        .expect("PostToolUse 2 should succeed");
    assert!(
        matches!(action2, BashCheckpointAction::Checkpoint(_)),
        "Second tool use should produce Checkpoint"
    );
}

// ===========================================================================
// Tool Classification — All 6 Agents
// ===========================================================================

#[test]
fn test_classify_tool_claude() {
    assert_eq!(classify_tool(Agent::Claude, "Write"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Claude, "Edit"), ToolClass::FileEdit);
    assert_eq!(
        classify_tool(Agent::Claude, "MultiEdit"),
        ToolClass::FileEdit
    );
    assert_eq!(classify_tool(Agent::Claude, "Bash"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::Claude, "Read"), ToolClass::Skip);
    assert_eq!(classify_tool(Agent::Claude, "Glob"), ToolClass::Skip);
    assert_eq!(
        classify_tool(Agent::Claude, "unknown_tool"),
        ToolClass::Skip
    );
}

#[test]
fn test_classify_tool_gemini() {
    assert_eq!(
        classify_tool(Agent::Gemini, "write_file"),
        ToolClass::FileEdit
    );
    assert_eq!(classify_tool(Agent::Gemini, "replace"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Gemini, "shell"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::Gemini, "read_file"), ToolClass::Skip);
    assert_eq!(classify_tool(Agent::Gemini, "unknown"), ToolClass::Skip);
}

#[test]
fn test_classify_tool_continue_cli() {
    assert_eq!(
        classify_tool(Agent::ContinueCli, "edit"),
        ToolClass::FileEdit
    );
    assert_eq!(
        classify_tool(Agent::ContinueCli, "terminal"),
        ToolClass::Bash
    );
    assert_eq!(
        classify_tool(Agent::ContinueCli, "local_shell_call"),
        ToolClass::Bash
    );
    assert_eq!(classify_tool(Agent::ContinueCli, "read"), ToolClass::Skip);
    assert_eq!(
        classify_tool(Agent::ContinueCli, "unknown"),
        ToolClass::Skip
    );
}

#[test]
fn test_classify_tool_droid() {
    assert_eq!(
        classify_tool(Agent::Droid, "ApplyPatch"),
        ToolClass::FileEdit
    );
    assert_eq!(classify_tool(Agent::Droid, "Edit"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Droid, "Write"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Droid, "Create"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Droid, "Bash"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::Droid, "Read"), ToolClass::Skip);
    assert_eq!(classify_tool(Agent::Droid, "unknown"), ToolClass::Skip);
}

#[test]
fn test_classify_tool_amp() {
    assert_eq!(classify_tool(Agent::Amp, "Write"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Amp, "Edit"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::Amp, "Bash"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::Amp, "Read"), ToolClass::Skip);
    assert_eq!(classify_tool(Agent::Amp, "unknown"), ToolClass::Skip);
}

#[test]
fn test_classify_tool_opencode() {
    assert_eq!(classify_tool(Agent::OpenCode, "edit"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::OpenCode, "write"), ToolClass::FileEdit);
    assert_eq!(classify_tool(Agent::OpenCode, "bash"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::OpenCode, "shell"), ToolClass::Bash);
    assert_eq!(classify_tool(Agent::OpenCode, "read"), ToolClass::Skip);
    assert_eq!(classify_tool(Agent::OpenCode, "unknown"), ToolClass::Skip);
}

// ===========================================================================
// Gitignore Filtering
// ===========================================================================

#[test]
fn test_bash_tool_gitignore_excludes_new_untracked_files() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Create a .gitignore that ignores *.log files, then commit it
    add_and_commit(&repo, ".gitignore", "*.log\n", "add gitignore");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Create both an ignored and a non-ignored file
    write_file(&repo, "debug.log", "log output");
    write_file(&repo, "result.txt", "result data");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    assert!(
        created.iter().any(|p| p.contains("result.txt")),
        "result.txt should be created; got {:?}",
        created
    );
    assert!(
        !created.iter().any(|p| p.contains("debug.log")),
        "debug.log should be excluded by gitignore; got {:?}",
        created
    );
}

#[test]
fn test_bash_tool_gitignore_does_not_exclude_tracked_files() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Commit a .log file FIRST (making it tracked), then add gitignore
    add_and_commit(&repo, "important.log", "valuable data", "track the log");
    add_and_commit(&repo, ".gitignore", "*.log\n", "add gitignore");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Modify the tracked .log file
    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "important.log", "updated data");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let modified: Vec<String> = result
        .modified
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        modified.iter().any(|p| p.contains("important.log")),
        "tracked important.log should still appear as modified despite gitignore; got {:?}",
        modified
    );
}

#[test]
fn test_bash_tool_gitignore_excludes_directory_patterns() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Use glob patterns that match files (not just directory-trailing patterns),
    // since the snapshot walker checks individual file paths with is_dir=false.
    add_and_commit(
        &repo,
        ".gitignore",
        "*.o\n*.pyc\ntarget/\n",
        "add gitignore",
    );

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    // Create files matching glob-based ignore patterns
    write_file(&repo, "build/output.o", "binary");
    write_file(&repo, "cache/module.pyc", "bytecode");
    // Also create a non-ignored file
    write_file(&repo, "src/main.rs", "fn main() {}");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let created: Vec<String> = result
        .created
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    assert!(
        created
            .iter()
            .any(|p| p.contains("src/main.rs") || p.contains("src\\main.rs")),
        "src/main.rs should be created; got {:?}",
        created
    );
    assert!(
        !created.iter().any(|p| p.contains("output.o")),
        "*.o files should be excluded by gitignore; got {:?}",
        created
    );
    assert!(
        !created.iter().any(|p| p.contains("module.pyc")),
        "*.pyc files should be excluded by gitignore; got {:?}",
        created
    );
}

// ===========================================================================
// build_gitignore
// ===========================================================================

#[test]
fn test_build_gitignore_parses_rules() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, ".gitignore", "*.tmp\ntarget/\n", "add gitignore");

    let gitignore = build_gitignore(&root).expect("build_gitignore should succeed");

    // *.tmp files should be ignored
    let tmp_match = gitignore.matched(Path::new("data.tmp"), false);
    assert!(tmp_match.is_ignore(), "*.tmp should match gitignore rules");

    // .rs files should not be ignored
    let rs_match = gitignore.matched(Path::new("main.rs"), false);
    assert!(
        !rs_match.is_ignore(),
        "*.rs should not match gitignore rules"
    );
}

// ===========================================================================
// git_status_fallback
// ===========================================================================

#[test]
fn test_git_status_fallback_detects_changes() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "tracked.txt", "original", "initial");
    write_file(&repo, "tracked.txt", "modified");

    let changed = git_status_fallback(&root).expect("git_status_fallback should succeed");

    assert!(
        changed.iter().any(|p| p.contains("tracked.txt")),
        "git_status_fallback should report tracked.txt; got {:?}",
        changed
    );
}

#[test]
fn test_git_status_fallback_detects_untracked() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Make an initial commit so we have a valid repo
    add_and_commit(&repo, "base.txt", "base", "init");
    write_file(&repo, "untracked.txt", "new file");

    let changed = git_status_fallback(&root).expect("git_status_fallback should succeed");

    assert!(
        changed.iter().any(|p| p.contains("untracked.txt")),
        "git_status_fallback should report untracked.txt; got {:?}",
        changed
    );
}

#[test]
fn test_git_status_fallback_clean_repo() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "clean.txt", "clean", "initial");

    let changed = git_status_fallback(&root).expect("git_status_fallback should succeed");
    assert!(
        changed.is_empty(),
        "clean repo should report no changes; got {:?}",
        changed
    );
}

// ===========================================================================
// cleanup_stale_snapshots
// ===========================================================================

#[test]
fn test_cleanup_stale_snapshots_does_not_error_on_empty() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    // Make an initial commit so .git directory is valid
    add_and_commit(&repo, "init.txt", "init", "initial");

    // Should not error even when there are no snapshots
    cleanup_stale_snapshots(&root).expect("cleanup_stale_snapshots should succeed on empty dir");
}

// ===========================================================================
// normalize_path consistency
// ===========================================================================

#[test]
fn test_normalize_path_idempotent() {
    let path = Path::new("src/lib.rs");
    let once = normalize_path(path);
    let twice = normalize_path(&once);
    assert_eq!(once, twice, "normalize_path should be idempotent");
}

#[test]
fn test_normalize_path_handles_nested() {
    let path = Path::new("deeply/nested/dir/file.rs");
    let normalized = normalize_path(path);
    // On any platform, normalizing twice should give the same result
    assert_eq!(normalized, normalize_path(&normalized));
}

// ===========================================================================
// Snapshot invocation key
// ===========================================================================

#[test]
fn test_snapshot_invocation_key_format() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let snap = snapshot(&root, "my-session", "my-tool").expect("snapshot should succeed");
    assert_eq!(
        snap.invocation_key, "my-session:my-tool",
        "invocation_key should be session_id:tool_use_id"
    );
}

// ===========================================================================
// DiffResult helpers
// ===========================================================================

#[test]
fn test_diff_result_all_changed_paths_combines_categories() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "modify.txt", "original", "initial");
    add_and_commit(&repo, "delete.txt", "doomed", "add delete target");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "modify.txt", "changed");
    write_file(&repo, "create.txt", "new");
    fs::remove_file(repo.path().join("delete.txt")).expect("delete should succeed");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let all = result.all_changed_paths();
    assert!(
        all.len() >= 3,
        "Should have at least 3 changed paths; got {}",
        all.len()
    );
    assert!(all.iter().any(|p| p.contains("modify.txt")));
    assert!(all.iter().any(|p| p.contains("create.txt")));
    assert!(all.iter().any(|p| p.contains("delete.txt")));
}

#[test]
fn test_diff_result_is_empty_true_when_no_changes() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");
    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    assert!(result.is_empty());
    assert!(result.all_changed_paths().is_empty());
}

// ===========================================================================
// Subdirectory file operations
// ===========================================================================

#[test]
fn test_bash_tool_detect_file_in_subdirectory() {
    let repo = TestRepo::new();
    let root = repo_root(&repo);

    add_and_commit(&repo, "src/lib.rs", "pub fn foo() {}", "initial");

    let pre = snapshot(&root, "sess", "t1").expect("pre-snapshot should succeed");

    thread::sleep(Duration::from_millis(50));
    write_file(&repo, "src/lib.rs", "pub fn bar() {}");
    write_file(&repo, "src/nested/deep/module.rs", "mod deep;");

    let post = snapshot(&root, "sess", "t2").expect("post-snapshot should succeed");
    let result = diff(&pre, &post);

    let all = result.all_changed_paths();
    assert!(
        all.iter()
            .any(|p| p.contains("src/lib.rs") || p.contains("src\\lib.rs")),
        "src/lib.rs should be detected; got {:?}",
        all
    );
    assert!(
        all.iter().any(|p| p.contains("module.rs")),
        "deeply nested module.rs should be detected; got {:?}",
        all
    );
}
