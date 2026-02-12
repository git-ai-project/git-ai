#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_blame_from_subdirectory_with_relative_path() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let file_path = subdir.join("main.rs");
    fs::write(&file_path, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

    repo.git(&["add", "src/main.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/main.rs"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let output = repo
        .git_ai_from_working_dir(&subdir, &["blame", "main.rs"])
        .expect("blame from subdirectory with relative path should succeed");

    assert!(
        output.contains("fn main()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_nested_subdirectory_with_relative_path() {
    let repo = TestRepo::new();

    let nested_dir = repo.path().join("src").join("lib").join("utils");
    fs::create_dir_all(&nested_dir).unwrap();

    let file_path = nested_dir.join("helper.rs");
    fs::write(&file_path, "pub fn help() {}\n").unwrap();

    repo.git(&["add", "src/lib/utils/helper.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/lib/utils/helper.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add helper").unwrap();

    let output = repo
        .git_ai_from_working_dir(&nested_dir, &["blame", "helper.rs"])
        .expect("blame from deeply nested subdirectory should succeed");

    assert!(
        output.contains("pub fn help()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_subdirectory_with_subpath() {
    let repo = TestRepo::new();

    let src_dir = repo.path().join("src");
    let lib_dir = src_dir.join("lib");
    fs::create_dir_all(&lib_dir).unwrap();

    let file_path = lib_dir.join("mod.rs");
    fs::write(&file_path, "pub mod utils;\n").unwrap();

    repo.git(&["add", "src/lib/mod.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/lib/mod.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add mod").unwrap();

    let output = repo
        .git_ai_from_working_dir(&src_dir, &["blame", "lib/mod.rs"])
        .expect("blame from parent subdirectory with sub-path should succeed");

    assert!(
        output.contains("pub mod utils"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_repo_root_still_works() {
    let repo = TestRepo::new();

    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1", "Line 2".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let output = repo
        .git_ai(&["blame", "test.txt"])
        .expect("blame from repo root should still work");

    assert!(
        output.contains("Line 1"),
        "blame output should contain file content, got: {}",
        output
    );
    assert!(
        output.contains("Line 2"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_repo_root_with_subdir_path() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let file_path = subdir.join("app.rs");
    fs::write(&file_path, "fn app() {}\n").unwrap();

    repo.git(&["add", "src/app.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/app.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add app").unwrap();

    let output = repo
        .git_ai(&["blame", "src/app.rs"])
        .expect("blame from repo root with subdirectory path should work");

    assert!(
        output.contains("fn app()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_subdirectory_preserves_ai_authorship() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let mut file = repo.filename("src/code.rs");
    file.set_contents(lines!["fn human_code() {}".human(), "fn ai_code() {}".ai()]);
    repo.stage_all_and_commit("Mixed commit").unwrap();

    let root_output = repo
        .git_ai(&["blame", "src/code.rs"])
        .expect("blame from root should work");

    let subdir_output = repo
        .git_ai_from_working_dir(&subdir, &["blame", "code.rs"])
        .expect("blame from subdirectory should work");

    assert_eq!(
        root_output, subdir_output,
        "blame output from root and subdirectory should be identical"
    );
}

#[test]
fn test_blame_from_subdirectory_nonexistent_file_errors() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let mut file = repo.filename("src/exists.rs");
    file.set_contents(lines!["content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let result = repo.git_ai_from_working_dir(&subdir, &["blame", "nonexistent.rs"]);
    assert!(result.is_err(), "blame for nonexistent file should fail");
}

#[test]
fn test_blame_from_subdirectory_with_line_range() {
    let repo = TestRepo::new();

    let subdir = repo.path().join("src");
    fs::create_dir_all(&subdir).unwrap();

    let file_path = subdir.join("multi.rs");
    fs::write(&file_path, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    repo.git(&["add", "src/multi.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/multi.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add multi-line file").unwrap();

    let output = repo
        .git_ai_from_working_dir(&subdir, &["blame", "-L", "2,4", "multi.rs"])
        .expect("blame with line range from subdirectory should succeed");

    assert!(
        output.contains("line2"),
        "blame output should contain line2, got: {}",
        output
    );
    assert!(
        output.contains("line4"),
        "blame output should contain line4, got: {}",
        output
    );
    assert!(
        !output.contains("line1"),
        "blame output should NOT contain line1 (outside range), got: {}",
        output
    );
    assert!(
        !output.contains("line5"),
        "blame output should NOT contain line5 (outside range), got: {}",
        output
    );
}

#[test]
fn test_blame_from_deep_subdir_dotdot_into_sibling_dir() {
    let repo = TestRepo::new();

    let dir_a = repo.path().join("a").join("b").join("c");
    let dir_b = repo.path().join("x").join("y");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();

    let file_path = dir_b.join("target.rs");
    fs::write(&file_path, "pub fn target() {}\n").unwrap();

    repo.git(&["add", "x/y/target.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "x/y/target.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add target file").unwrap();

    let output = repo
        .git_ai_from_working_dir(&dir_a, &["blame", "../../../x/y/target.rs"])
        .expect("blame with .. traversal into sibling directory should succeed");

    assert!(
        output.contains("pub fn target()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_deep_subdir_dotdot_up_one_level() {
    let repo = TestRepo::new();

    let parent_dir = repo.path().join("src");
    let child_dir = parent_dir.join("sub");
    fs::create_dir_all(&child_dir).unwrap();

    let file_path = parent_dir.join("lib.rs");
    fs::write(&file_path, "pub mod sub;\n").unwrap();

    repo.git(&["add", "src/lib.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add lib").unwrap();

    let output = repo
        .git_ai_from_working_dir(&child_dir, &["blame", "../lib.rs"])
        .expect("blame with ../file from child dir should succeed");

    assert!(
        output.contains("pub mod sub"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_deep_subdir_dotdot_multiple_levels() {
    let repo = TestRepo::new();

    let deep_dir = repo.path().join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep_dir).unwrap();

    let file_path = repo.path().join("a").join("root_level.rs");
    fs::write(&file_path, "fn root_level() {}\n").unwrap();

    repo.git(&["add", "a/root_level.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "a/root_level.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add root level file").unwrap();

    let output = repo
        .git_ai_from_working_dir(&deep_dir, &["blame", "../../../root_level.rs"])
        .expect("blame with multiple ../.. from deep dir should succeed");

    assert!(
        output.contains("fn root_level()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_deep_subdir_file_in_repo_root() {
    let repo = TestRepo::new();

    let deep_dir = repo.path().join("a").join("b").join("c");
    fs::create_dir_all(&deep_dir).unwrap();

    let mut file = repo.filename("root.txt");
    file.set_contents(lines!["root content".ai()]);
    repo.stage_all_and_commit("Add root file").unwrap();

    let output = repo
        .git_ai_from_working_dir(&deep_dir, &["blame", "../../../root.txt"])
        .expect("blame from deep subdir targeting repo root file should succeed");

    assert!(
        output.contains("root content"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_subdir_dotdot_into_different_subtree() {
    let repo = TestRepo::new();

    let frontend_dir = repo.path().join("packages").join("frontend").join("src");
    let backend_dir = repo.path().join("packages").join("backend").join("src");
    fs::create_dir_all(&frontend_dir).unwrap();
    fs::create_dir_all(&backend_dir).unwrap();

    let file_path = backend_dir.join("server.rs");
    fs::write(&file_path, "fn start_server() {}\n").unwrap();

    repo.git(&["add", "packages/backend/src/server.rs"])
        .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "packages/backend/src/server.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add server").unwrap();

    let output = repo
        .git_ai_from_working_dir(&frontend_dir, &["blame", "../../backend/src/server.rs"])
        .expect("blame with .. traversal from frontend into backend subtree should succeed");

    assert!(
        output.contains("fn start_server()"),
        "blame output should contain file content, got: {}",
        output
    );
}

#[test]
fn test_blame_from_deep_subdir_preserves_ai_authorship_with_dotdot() {
    let repo = TestRepo::new();

    let deep_dir = repo.path().join("src").join("modules").join("core");
    let target_dir = repo.path().join("src").join("utils");
    fs::create_dir_all(&deep_dir).unwrap();
    fs::create_dir_all(&target_dir).unwrap();

    let mut file = repo.filename("src/utils/helpers.rs");
    file.set_contents(lines![
        "fn human_helper() {}".human(),
        "fn ai_helper() {}".ai()
    ]);
    repo.stage_all_and_commit("Add helpers").unwrap();

    let root_output = repo
        .git_ai(&["blame", "src/utils/helpers.rs"])
        .expect("blame from root should work");

    let subdir_output = repo
        .git_ai_from_working_dir(&deep_dir, &["blame", "../../utils/helpers.rs"])
        .expect("blame with .. from deep subdir should work");

    assert_eq!(
        root_output, subdir_output,
        "blame output from root and via .. traversal should be identical"
    );
}
