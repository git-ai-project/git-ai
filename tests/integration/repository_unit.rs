use git_ai::git::repository::{
    args_with_disabled_hooks_if_needed, args_with_internal_git_profile, disable_internal_git_hooks,
    find_repository_in_path, parse_diff_added_lines_with_insertions, parse_git_version,
    resolve_command_base_dir, worktree_storage_ai_dir, InternalGitProfile,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::repos::test_repo::TestRepo;

fn run_git(cwd: &Path, args: &[&str]) {
    git_ai::git::test_utils::init_test_git_config();
    let output = Command::new(git_ai::config::Config::get().git_cmd())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn test_parse_git_version_standard() {
    // Standard git version format
    assert_eq!(parse_git_version("git version 2.39.3"), Some((2, 39, 3)));
    assert_eq!(parse_git_version("git version 2.23.0"), Some((2, 23, 0)));
    assert_eq!(parse_git_version("git version 1.8.5"), Some((1, 8, 5)));
}

#[test]
fn test_parse_git_version_apple_git() {
    // macOS Apple Git format
    assert_eq!(
        parse_git_version("git version 2.39.3 (Apple Git-146)"),
        Some((2, 39, 3))
    );
}

#[test]
fn test_parse_git_version_windows() {
    // Windows git format
    assert_eq!(
        parse_git_version("git version 2.42.0.windows.2"),
        Some((2, 42, 0))
    );
}

#[test]
fn test_parse_git_version_no_patch() {
    // Version without patch number
    assert_eq!(parse_git_version("git version 2.39"), Some((2, 39, 0)));
}

#[test]
fn test_parse_git_version_with_newline() {
    // Version string with trailing newline
    assert_eq!(parse_git_version("git version 2.39.3\n"), Some((2, 39, 3)));
}

#[test]
fn test_parse_git_version_invalid() {
    // Invalid formats should return None
    assert_eq!(parse_git_version(""), None);
    assert_eq!(parse_git_version("not a version"), None);
    assert_eq!(parse_git_version("git version"), None);
    assert_eq!(parse_git_version("git version x.y.z"), None);
}

#[test]
fn disable_internal_git_hooks_guard_applies_to_spawned_threads() {
    let args = vec!["status".to_string()];
    let _guard = disable_internal_git_hooks();

    let spawned_args = args.clone();
    let forwarded =
        std::thread::spawn(move || args_with_disabled_hooks_if_needed(&spawned_args))
            .join()
            .expect("thread should join");

    assert_eq!(forwarded[0], "-c");
    assert!(forwarded[1].starts_with("core.hooksPath="));
}

#[test]
fn patch_profile_applies_canonical_machine_parse_flags() {
    let args = vec!["diff".to_string(), "HEAD^".to_string(), "HEAD".to_string()];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::PatchParse);

    assert!(rewritten.iter().any(|arg| arg == "--no-ext-diff"));
    assert!(rewritten.iter().any(|arg| arg == "--no-textconv"));
    assert!(rewritten.iter().any(|arg| arg == "--src-prefix=a/"));
    assert!(rewritten.iter().any(|arg| arg == "--dst-prefix=b/"));
    assert!(rewritten.iter().any(|arg| arg == "--no-relative"));
    assert!(rewritten.iter().any(|arg| arg == "--no-color"));
    assert!(
        rewritten
            .iter()
            .any(|arg| arg == "--diff-algorithm=default")
    );
    assert!(rewritten.iter().any(|arg| arg == "--indent-heuristic"));
    assert!(rewritten.iter().any(|arg| arg == "--inter-hunk-context=0"));
}

#[test]
fn numstat_profile_disables_renames_and_external_renderers() {
    let args = vec![
        "diff".to_string(),
        "--numstat".to_string(),
        "HEAD^".to_string(),
        "HEAD".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::NumstatParse);
    assert!(rewritten.iter().any(|arg| arg == "--no-ext-diff"));
    assert!(rewritten.iter().any(|arg| arg == "--no-textconv"));
    assert!(rewritten.iter().any(|arg| arg == "--no-color"));
    assert!(rewritten.iter().any(|arg| arg == "--no-relative"));
    assert!(rewritten.iter().any(|arg| arg == "--no-renames"));
}

#[test]
fn numstat_profile_strips_short_rename_and_copy_flags() {
    let args = vec![
        "diff".to_string(),
        "--numstat".to_string(),
        "-M90%".to_string(),
        "-C".to_string(),
        "-C75%".to_string(),
        "HEAD^".to_string(),
        "HEAD".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::NumstatParse);
    assert!(!rewritten.iter().any(|arg| arg == "-C"));
    assert!(!rewritten.iter().any(|arg| arg.starts_with("-M")));
    assert!(!rewritten.iter().any(|arg| arg.starts_with("-C")));
    assert!(rewritten.iter().any(|arg| arg == "--no-renames"));
}

#[test]
fn general_profile_is_noop() {
    let args = vec!["status".to_string(), "--porcelain=v2".to_string()];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::General);
    assert_eq!(rewritten, args);
}

#[test]
fn patch_profile_strips_conflicting_ext_diff_and_color_flags() {
    let args = vec![
        "diff".to_string(),
        "--ext-diff".to_string(),
        "--color=always".to_string(),
        "HEAD".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::PatchParse);

    assert!(rewritten.iter().any(|arg| arg == "--no-ext-diff"));
    assert!(!rewritten.iter().any(|arg| arg == "--ext-diff"));
    assert!(!rewritten.iter().any(|arg| arg.starts_with("--color")));
    assert!(rewritten.iter().any(|arg| arg == "--no-color"));
}

#[test]
fn patch_profile_strips_split_prefix_args() {
    let args = vec![
        "diff".to_string(),
        "--src-prefix".to_string(),
        "SRC/".to_string(),
        "--dst-prefix".to_string(),
        "DST/".to_string(),
        "HEAD^".to_string(),
        "HEAD".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::PatchParse);

    assert!(!rewritten.iter().any(|arg| arg == "--src-prefix"));
    assert!(!rewritten.iter().any(|arg| arg == "--dst-prefix"));
    assert!(!rewritten.iter().any(|arg| arg == "SRC/"));
    assert!(!rewritten.iter().any(|arg| arg == "DST/"));
    assert!(rewritten.iter().any(|arg| arg == "--src-prefix=a/"));
    assert!(rewritten.iter().any(|arg| arg == "--dst-prefix=b/"));
}

#[test]
fn profile_rewrite_does_not_strip_pathspec_tokens_after_double_dash() {
    let args = vec![
        "diff".to_string(),
        "--color=always".to_string(),
        "HEAD^".to_string(),
        "HEAD".to_string(),
        "--".to_string(),
        "--color".to_string(),
        "--relative".to_string(),
        "file.txt".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::PatchParse);
    let separator = rewritten
        .iter()
        .position(|arg| arg == "--")
        .expect("rewritten args should keep pathspec separator");
    assert_eq!(
        rewritten[separator + 1..],
        [
            "--color".to_string(),
            "--relative".to_string(),
            "file.txt".to_string()
        ]
    );
}

#[test]
fn raw_diff_profile_keeps_rename_flags_untouched() {
    let args = vec![
        "diff".to_string(),
        "--raw".to_string(),
        "-z".to_string(),
        "-M".to_string(),
        "HEAD^".to_string(),
        "HEAD".to_string(),
    ];
    let rewritten = args_with_internal_git_profile(&args, InternalGitProfile::RawDiffParse);
    assert!(rewritten.iter().any(|arg| arg == "-M"));
    assert!(rewritten.iter().any(|arg| arg == "--no-ext-diff"));
    assert!(rewritten.iter().any(|arg| arg == "--no-textconv"));
    assert!(rewritten.iter().any(|arg| arg == "--no-color"));
    assert!(rewritten.iter().any(|arg| arg == "--no-relative"));
}

#[test]
fn test_list_commit_files_with_utf8_filename() {
    // Create a test repo with a UTF-8 filename
    let repo = TestRepo::new();

    // Write a file with Chinese characters in its name
    let chinese_filename = "中文文件.txt";
    std::fs::write(repo.path().join(chinese_filename), "Hello, 世界!\n")
        .expect("write Chinese file");

    // Stage and commit
    repo.stage_all_and_commit("Add Chinese file")
        .expect("commit Chinese file");

    // Get the head SHA
    let sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Get a gitai Repository
    let repo_handle =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("find repository");

    // Test list_commit_files
    let files = repo_handle
        .list_commit_files(&sha, None)
        .expect("list commit files");

    // Debug: print what we got
    println!("Files in commit: {:?}", files);

    // The file should be in the list with its UTF-8 name
    assert!(
        files.contains(chinese_filename),
        "Should contain the Chinese filename '{}', but got: {:?}",
        chinese_filename,
        files
    );
}

#[test]
fn test_parse_diff_added_lines_with_insertions_standard_prefix() {
    // Test diff with standard b/ prefix (commit-to-commit diff)
    let diff = r#"diff --git a/test.txt b/test.txt
index 0000000..abc1234 100644
--- a/test.txt
+++ b/test.txt
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("test.txt"), Some(&vec![1, 2]));
    assert_eq!(insertion_lines.get("test.txt"), Some(&vec![1, 2]));
}

#[test]
fn test_parse_diff_added_lines_with_insertions_workdir_prefix() {
    // Test diff with w/ prefix (commit-to-workdir diff)
    let diff = r#"diff --git c/test.txt w/test.txt
index a751413..8adaa6c 100644
--- c/test.txt
+++ w/test.txt
@@ -0,0 +1,2 @@
+// AI added line 1
+// AI added line 2"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("test.txt"), Some(&vec![1, 2]));
    assert_eq!(insertion_lines.get("test.txt"), Some(&vec![1, 2]));
}

#[test]
fn test_parse_diff_added_lines_with_insertions_quoted_paths() {
    // Test diff with quoted paths containing spaces
    let diff = r#"diff --git "a/my file.txt" "b/my file.txt"
index 0000000..abc1234 100644
--- "a/my file.txt"
+++ "b/my file.txt"
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("my file.txt"), Some(&vec![1, 2, 3]));
    assert_eq!(insertion_lines.get("my file.txt"), Some(&vec![1, 2, 3]));
}

#[test]
fn test_parse_diff_added_lines_with_insertions_quoted_workdir_paths() {
    // Test diff with quoted w/ paths
    let diff = r#"diff --git "c/my file.txt" "w/my file.txt"
index 0000000..abc1234 100644
--- "c/my file.txt"
+++ "w/my file.txt"
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("my file.txt"), Some(&vec![1, 2]));
    assert_eq!(insertion_lines.get("my file.txt"), Some(&vec![1, 2]));
}

#[test]
fn test_parse_diff_added_lines_with_insertions_no_prefix_paths() {
    let diff = r#"diff --git my-file.txt my-file.txt
index 0000000..abc1234 100644
--- my-file.txt
+++ my-file.txt
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("my-file.txt"), Some(&vec![1, 2]));
    assert_eq!(insertion_lines.get("my-file.txt"), Some(&vec![1, 2]));
}

#[test]
fn test_parse_diff_added_lines_with_insertions_custom_prefix_paths() {
    let diff = r#"diff --git SRC/my-file.txt DST/my-file.txt
index 0000000..abc1234 100644
--- SRC/my-file.txt
+++ DST/my-file.txt
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

    let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
    assert_eq!(added_lines.get("DST/my-file.txt"), Some(&vec![1, 2]));
    assert_eq!(insertion_lines.get("DST/my-file.txt"), Some(&vec![1, 2]));
}

#[test]
fn worktree_storage_ai_dir_keeps_full_relative_worktree_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let common_dir = temp.path().join("repo.git");
    let linked_git_dir = common_dir.join("worktrees").join("feature").join("nested");

    fs::create_dir_all(&linked_git_dir).expect("create linked git dir");

    let ai_dir = worktree_storage_ai_dir(&linked_git_dir, &common_dir);
    assert_eq!(
        ai_dir,
        common_dir
            .join("ai")
            .join("worktrees")
            .join("feature")
            .join("nested")
    );
}

#[test]
fn worktree_storage_ai_dir_fallback_uses_git_dir_leaf_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let common_dir = temp.path().join("repo.git");
    let detached_git_dir = temp.path().join("somewhere").join("linked-worktree");

    fs::create_dir_all(&common_dir).expect("create common dir");
    fs::create_dir_all(&detached_git_dir).expect("create detached git dir");

    let ai_dir = worktree_storage_ai_dir(&detached_git_dir, &common_dir);
    assert_eq!(
        ai_dir,
        common_dir
            .join("ai")
            .join("worktrees")
            .join("linked-worktree")
    );
}

#[test]
fn resolve_command_base_dir_applies_chained_c_arguments() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path().join("root");
    let args = vec![
        "-C".to_string(),
        base.to_string_lossy().to_string(),
        "-C".to_string(),
        "nested".to_string(),
        "-C".to_string(),
        "..".to_string(),
        "-C".to_string(),
        "repo".to_string(),
        "status".to_string(),
    ];

    let resolved = resolve_command_base_dir(&args).expect("resolve base dir");
    assert_eq!(resolved, base.join("nested").join("..").join("repo"));
}

#[test]
fn find_repository_in_path_supports_bare_repositories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let bare = temp.path().join("repo.git");
    fs::create_dir_all(&source).expect("create source");

    run_git(&source, &["init"]);
    run_git(&source, &["config", "user.name", "Test User"]);
    run_git(&source, &["config", "user.email", "test@example.com"]);
    fs::write(source.join("README.md"), "# repo\n").expect("write readme");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-m", "initial"]);
    run_git(
        temp.path(),
        &[
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );

    let repo = find_repository_in_path(bare.to_str().unwrap()).expect("find bare repo");
    assert!(repo.is_bare_repository().expect("bare check"));
    assert_eq!(
        repo.path().canonicalize().expect("canonical bare"),
        bare.canonicalize().expect("canonical path")
    );

    let discovered = git_ai::git::repository::discover_repository_in_path_no_git_exec(&bare)
        .expect("discover bare repo");
    assert_eq!(
        discovered.path().canonicalize().expect("canonical bare"),
        bare.canonicalize().expect("canonical path")
    );
}

#[test]
fn find_repository_in_path_bare_repo_can_read_head_gitattributes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let bare = temp.path().join("repo.git");
    fs::create_dir_all(&source).expect("create source");

    run_git(&source, &["init"]);
    run_git(&source, &["config", "user.name", "Test User"]);
    run_git(&source, &["config", "user.email", "test@example.com"]);
    fs::write(
        source.join(".gitattributes"),
        "generated/** linguist-generated=true\n",
    )
    .expect("write attrs");
    fs::write(source.join("README.md"), "# repo\n").expect("write readme");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-m", "initial"]);
    run_git(
        temp.path(),
        &[
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );

    let repo = find_repository_in_path(bare.to_str().unwrap()).expect("find bare repo");
    let content = repo
        .get_file_content(".gitattributes", "HEAD")
        .expect("read attrs from HEAD");
    let content = String::from_utf8(content).expect("utf8 attrs");
    assert!(content.contains("generated/** linguist-generated=true"));
}

#[test]
fn find_repository_in_path_worktree_uses_common_dir_for_isolated_storage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let main_repo = temp.path().join("main");
    let worktree = temp.path().join("linked");

    fs::create_dir_all(&main_repo).expect("create main repo dir");
    run_git(&main_repo, &["init"]);
    run_git(&main_repo, &["config", "user.name", "Test User"]);
    run_git(&main_repo, &["config", "user.email", "test@example.com"]);
    run_git(&main_repo, &["worktree", "add", worktree.to_str().unwrap()]);

    let repo = find_repository_in_path(worktree.to_str().unwrap()).expect("find worktree repo");
    let common_dir = PathBuf::from(run_git_stdout(
        &worktree,
        &["rev-parse", "--git-common-dir"],
    ));

    assert_eq!(
        repo.common_dir()
            .canonicalize()
            .expect("canonical common dir"),
        common_dir
            .canonicalize()
            .expect("canonical expected common dir")
    );
    assert!(
        repo.storage
            .working_logs
            .starts_with(common_dir.join("ai").join("worktrees")),
        "worktree storage should be isolated under common-dir/ai/worktrees: {}",
        repo.storage.working_logs.display()
    );

    let discovered = git_ai::git::repository::discover_repository_in_path_no_git_exec(&worktree)
        .expect("discover worktree repo");
    assert_eq!(
        discovered
            .common_dir()
            .canonicalize()
            .expect("canonical discovered common dir"),
        common_dir
            .canonicalize()
            .expect("canonical expected common dir")
    );
    assert!(
        discovered
            .storage
            .working_logs
            .starts_with(common_dir.join("ai").join("worktrees")),
        "discovered worktree storage should be isolated under common-dir/ai/worktrees: {}",
        discovered.storage.working_logs.display()
    );
}

#[test]
fn path_is_in_workdir_returns_false_for_linked_worktree_file() {
    // Sibling worktree: the worktree lives OUTSIDE the main repo's working tree.
    // path_is_in_workdir returns false purely because the path doesn't
    // start_with(workdir) — no .git file inspection is needed.  This test
    // passes even without the is_linked_worktree_git_file fix.
    let temp = tempfile::tempdir().expect("tempdir");
    let main_repo = temp.path().join("main");
    let worktree = temp.path().join("linked");

    fs::create_dir_all(&main_repo).expect("create main repo dir");
    run_git(&main_repo, &["init"]);
    run_git(&main_repo, &["config", "user.name", "Test User"]);
    run_git(&main_repo, &["config", "user.email", "test@example.com"]);
    // Write a file so the sanity-check path exists on disk — path_is_in_workdir
    // calls path.canonicalize() which only resolves symlinks for existing paths
    // (on macOS /var/... is a symlink to /private/var/...; on Windows temp paths
    // may use short names that differ from the canonical workdir stored by git).
    fs::write(main_repo.join("README.md"), "# main\n").expect("write README");
    run_git(&main_repo, &["worktree", "add", worktree.to_str().unwrap()]);

    let dot_git = worktree.join(".git");
    assert!(
        dot_git.is_file(),
        ".git should be a file in a linked worktree"
    );

    let main = find_repository_in_path(main_repo.to_str().unwrap()).expect("find main repo");

    let wt_file = worktree.join("somefile.rs");
    assert!(
        !main.path_is_in_workdir(&wt_file),
        "sibling linked worktree file should not be in main repo workdir"
    );

    // Use an existing file so path.canonicalize() resolves symlinks correctly.
    let main_file = main_repo.join("README.md");
    assert!(
        main.path_is_in_workdir(&main_file),
        "main repo file should be in main repo workdir"
    );
}

#[test]
fn path_is_in_workdir_returns_false_for_nested_linked_worktree_file() {
    // Nested worktree: the worktree lives INSIDE the main repo's working tree
    // (e.g. main_repo/.worktrees/feature).  This is the exact Bug-A / Bug-B
    // scenario: path starts_with(workdir) so the starts_with check passes,
    // and only is_linked_worktree_git_file makes path_is_in_workdir return
    // false.  This test FAILS without the fix.
    let temp = tempfile::tempdir().expect("tempdir");
    let main_repo = temp.path().join("main");
    let worktree = main_repo.join(".worktrees").join("feature");

    fs::create_dir_all(&main_repo).expect("create main repo dir");
    run_git(&main_repo, &["init"]);
    run_git(&main_repo, &["config", "user.name", "Test User"]);
    run_git(&main_repo, &["config", "user.email", "test@example.com"]);
    // git worktree add requires at least one commit
    fs::write(main_repo.join("README.md"), "# test\n").expect("write README");
    run_git(&main_repo, &["add", "."]);
    run_git(&main_repo, &["commit", "-m", "initial"]);
    run_git(
        &main_repo,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()],
    );

    let dot_git = worktree.join(".git");
    assert!(
        dot_git.is_file(),
        ".git should be a file in a nested worktree"
    );
    let gitfile_content = fs::read_to_string(&dot_git).expect("read .git file");
    assert!(
        gitfile_content.contains("/worktrees/"),
        ".git file should reference /worktrees/: {}",
        gitfile_content.trim()
    );

    let main = find_repository_in_path(main_repo.to_str().unwrap()).expect("find main repo");

    // The nested worktree file is physically under main_repo/ but must NOT
    // be reported as part of the main repo's working tree.
    let wt_file = worktree.join("somefile.rs");
    assert!(
        !main.path_is_in_workdir(&wt_file),
        "nested linked worktree file should not be in main repo workdir \
         (path starts_with workdir, but .git file marks a repo boundary)"
    );

    // Sanity: file is in the worktree's own workdir.
    let wt_repo =
        find_repository_in_path(worktree.to_str().unwrap()).expect("find nested worktree");
    assert!(
        wt_repo.path_is_in_workdir(&wt_file),
        "nested worktree file should be in the worktree's own workdir"
    );

    // Sanity: a normal file in the main repo is still in the main workdir.
    // Use README.md which already exists so path.canonicalize() resolves
    // symlinks correctly (macOS /var/... → /private/var/...; Windows short names).
    let main_file = main_repo.join("README.md");
    assert!(
        main.path_is_in_workdir(&main_file),
        "main repo file should be in main repo workdir"
    );
}

#[test]
fn get_all_staged_file_blob_oids_reads_stage_zero_entries_without_git2() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path().join("repo");
    fs::create_dir_all(&repo_dir).expect("create repo dir");

    run_git(&repo_dir, &["init"]);
    run_git(&repo_dir, &["config", "user.name", "Test User"]);
    run_git(&repo_dir, &["config", "user.email", "test@example.com"]);

    fs::write(repo_dir.join("a.txt"), "alpha\n").expect("write a.txt");
    fs::create_dir_all(repo_dir.join("dir")).expect("create dir");
    fs::write(repo_dir.join("dir").join("b.txt"), "beta\n").expect("write b.txt");

    run_git(&repo_dir, &["add", "."]);

    let repo = find_repository_in_path(repo_dir.to_str().expect("repo path")).expect("repo");
    let staged = repo
        .get_all_staged_file_blob_oids()
        .expect("read staged blobs");

    assert_eq!(
        staged.get("a.txt"),
        Some(&run_git_stdout(&repo_dir, &["rev-parse", ":0:a.txt"]))
    );
    assert_eq!(
        staged.get("dir/b.txt"),
        Some(&run_git_stdout(&repo_dir, &["rev-parse", ":0:dir/b.txt"]))
    );
}
