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
