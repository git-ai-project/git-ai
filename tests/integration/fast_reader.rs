use crate::repos::test_repo::TestRepo;
use std::path::Path;

/// Verify that FastRefReader::try_read_head() matches `git symbolic-ref HEAD`
#[test]
fn test_fast_head_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastRefReader::new(&git_dir, &git_dir);

    let fast_result = reader.try_read_head().expect("Should read HEAD");
    let git_result = repo.git_og(&["symbolic-ref", "HEAD"]).unwrap();
    let git_result = git_result.trim();

    match fast_result {
        git_ai::git::fast_reader::HeadKind::Symbolic(refname) => {
            assert_eq!(refname, git_result);
        }
        git_ai::git::fast_reader::HeadKind::Detached(_) => {
            panic!("Expected symbolic HEAD, got detached");
        }
    }
}

/// Verify that FastRefReader::try_resolve_ref() matches `git rev-parse` for loose refs
#[test]
fn test_fast_resolve_ref_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastRefReader::new(&git_dir, &git_dir);

    let git_result = repo.git_og(&["rev-parse", "refs/heads/main"]).unwrap();
    let git_result = git_result.trim();

    let fast_result = reader
        .try_resolve_ref("refs/heads/main")
        .expect("Should resolve main");

    assert_eq!(fast_result, git_result);
}

/// Verify detached HEAD is handled correctly
#[test]
fn test_fast_detached_head_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let sha = sha.trim().to_string();
    repo.git_og(&["checkout", "--detach", "HEAD"]).unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastRefReader::new(&git_dir, &git_dir);

    let fast_head = reader.try_read_head().expect("Should read HEAD");
    match fast_head {
        git_ai::git::fast_reader::HeadKind::Detached(oid) => {
            assert_eq!(oid, sha);
        }
        git_ai::git::fast_reader::HeadKind::Symbolic(_) => {
            panic!("Expected detached HEAD");
        }
    }

    let resolved = reader.try_resolve_ref("HEAD").expect("Should resolve HEAD");
    assert_eq!(resolved, sha);
}

/// Verify fast reader handles packed refs correctly
#[test]
fn test_fast_packed_refs_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    repo.git_og(&["branch", "feature-branch"]).unwrap();
    repo.git_og(&["pack-refs", "--all"]).unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastRefReader::new(&git_dir, &git_dir);

    let git_result = repo
        .git_og(&["rev-parse", "refs/heads/feature-branch"])
        .unwrap();
    let git_result = git_result.trim();

    let fast_result = reader
        .try_resolve_ref("refs/heads/feature-branch")
        .expect("Should resolve packed ref");

    assert_eq!(fast_result, git_result);
}

/// Verify that complex rev-parse syntax returns None (needs CLI fallback)
#[test]
fn test_fast_impl_fallback_complex_syntax() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("init.txt"), "init\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastRefReader::new(&git_dir, &git_dir);

    assert_eq!(reader.try_resolve_ref("HEAD~1"), None);
    assert_eq!(reader.try_resolve_ref("HEAD^2"), None);
    assert_eq!(reader.try_resolve_ref("@{yesterday}"), None);
    assert_eq!(reader.try_resolve_ref("main..HEAD"), None);
}

/// Verify FastObjectReader reads blob content matching `git cat-file blob`
#[test]
fn test_fast_read_blob_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("hello.txt"), "Hello, World!\n").unwrap();
    repo.stage_all_and_commit("add hello").unwrap();

    let tree_sha = repo.git_og(&["rev-parse", "HEAD^{tree}"]).unwrap();
    let tree_sha = tree_sha.trim();
    let ls_tree_output = repo
        .git_og(&["ls-tree", tree_sha, "--", "hello.txt"])
        .unwrap();
    let blob_oid = ls_tree_output.split_whitespace().nth(2).unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastObjectReader::new(&git_dir);

    let fast_content = reader
        .try_read_blob(blob_oid)
        .expect("Should read loose blob");

    let git_content = repo.git_og(&["cat-file", "blob", blob_oid]).unwrap();
    assert_eq!(fast_content, git_content.as_bytes());
}

/// Verify FastObjectReader::try_read_commit_tree_oid matches `git rev-parse HEAD^{tree}`
#[test]
fn test_fast_commit_tree_oid_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("file.txt"), "content\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let commit_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let commit_sha = commit_sha.trim();
    let expected_tree = repo.git_og(&["rev-parse", "HEAD^{tree}"]).unwrap();
    let expected_tree = expected_tree.trim();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastObjectReader::new(&git_dir);

    let fast_tree = reader
        .try_read_commit_tree_oid(commit_sha)
        .expect("Should read commit tree OID");
    assert_eq!(fast_tree, expected_tree);
}

/// Verify tree traversal for nested paths matches git ls-tree
#[test]
fn test_fast_tree_entry_for_path_matches_git_cli() {
    let repo = TestRepo::new();
    std::fs::create_dir_all(repo.path().join("src/utils")).unwrap();
    std::fs::write(repo.path().join("src/utils/helper.rs"), "fn helper() {}\n").unwrap();
    repo.stage_all_and_commit("add nested file").unwrap();

    let tree_sha = repo.git_og(&["rev-parse", "HEAD^{tree}"]).unwrap();
    let tree_sha = tree_sha.trim();

    let ls_tree_output = repo
        .git_og(&["ls-tree", "-r", tree_sha, "--", "src/utils/helper.rs"])
        .unwrap();
    let expected_blob_oid = ls_tree_output.split_whitespace().nth(2).unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastObjectReader::new(&git_dir);

    let fast_blob_oid = reader
        .try_tree_entry_for_path(tree_sha, Path::new("src/utils/helper.rs"))
        .expect("Should find nested path in tree");
    assert_eq!(fast_blob_oid, expected_blob_oid);
}

/// Verify that packed objects gracefully return None (triggering fallback)
#[test]
fn test_fast_read_packed_object_returns_none() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("file.txt"), "content\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let commit_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    let commit_sha = commit_sha.trim();

    repo.git_og(&["gc", "--aggressive"]).unwrap();

    let git_dir = repo.path().join(".git");
    let reader = git_ai::git::fast_reader::FastObjectReader::new(&git_dir);

    let result = reader.try_read_commit_tree_oid(commit_sha);
    assert_eq!(
        result, None,
        "Packed objects should return None for fallback"
    );
}

/// Verify worktree scenario: refs in common_dir, HEAD in git_dir
#[test]
fn test_fast_reader_worktree_refs_in_common_dir() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("file.txt"), "content\n").unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    let worktree_path = repo
        .path()
        .parent()
        .unwrap()
        .join("worktree-fast-reader-test");
    repo.git_og(&[
        "worktree",
        "add",
        worktree_path.to_str().unwrap(),
        "-b",
        "wt-branch",
    ])
    .unwrap();

    let wt_dot_git = worktree_path.join(".git");
    assert!(wt_dot_git.is_file(), ".git in worktree should be a file");
    let wt_git_dir_contents = std::fs::read_to_string(&wt_dot_git).unwrap();
    let wt_git_dir = wt_git_dir_contents.strip_prefix("gitdir: ").unwrap().trim();
    let wt_git_dir = if std::path::Path::new(wt_git_dir).is_absolute() {
        std::path::PathBuf::from(wt_git_dir)
    } else {
        worktree_path.join(wt_git_dir)
    };

    let common_dir = repo.path().join(".git");

    let reader = git_ai::git::fast_reader::FastRefReader::new(&wt_git_dir, &common_dir);

    let head = reader.try_read_head().expect("Should read worktree HEAD");
    match head {
        git_ai::git::fast_reader::HeadKind::Symbolic(refname) => {
            assert_eq!(refname, "refs/heads/wt-branch");
        }
        _ => panic!("Expected symbolic HEAD in worktree"),
    }

    let resolved = reader
        .try_resolve_ref("refs/heads/wt-branch")
        .expect("Should resolve worktree branch via common_dir");

    let expected = repo.git_og(&["rev-parse", "refs/heads/wt-branch"]).unwrap();
    assert_eq!(resolved, expected.trim());

    repo.git_og(&[
        "worktree",
        "remove",
        "--force",
        worktree_path.to_str().unwrap(),
    ])
    .unwrap();
}
