use crate::repos::test_repo::TestRepo;
use git_ai::authorship::virtual_attribution::VirtualAttributions;
use git_ai::git::repository::find_repository_in_path;

#[test]
fn test_virtual_attributions() {
    // Create a temporary repo with an initial commit
    let repo = TestRepo::new();

    // Write a test file with some content
    std::fs::write(
        repo.path().join("test_file.rs"),
        "fn main() {\n    println!(\"Hello\");\n}\n",
    )
    .unwrap();
    repo.git_og(&["add", "test_file.rs"]).unwrap();

    // Trigger checkpoint and commit to create proper authorship data
    repo.git_ai(&["checkpoint", "mock_known_human", "test_file.rs"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Get the commit SHA
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Get gitai repo handle
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Create VirtualAttributions using the temp repo
    let virtual_attributions = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            gitai_repo.clone(),
            commit_sha.clone(),
            &["test_file.rs".to_string()],
            None,
        )
        .await
    })
    .unwrap();

    // Verify files were tracked
    println!(
        "virtual_attributions files: {:?}",
        virtual_attributions.files()
    );
    println!("base_commit: {}", virtual_attributions.base_commit());
    println!("timestamp: {}", virtual_attributions.timestamp());

    // Print attribution details if available (for debugging)
    if let Some((char_attrs, line_attrs)) = virtual_attributions.get_attributions("test_file.rs")
    {
        println!("\n=== test_file.rs Attribution Info ===");
        println!("Character-level attributions: {} ranges", char_attrs.len());
        println!("Line-level attributions: {} ranges", line_attrs.len());
    }

    assert!(!virtual_attributions.files().is_empty());
}

crate::reuse_tests_in_worktree!(test_virtual_attributions,);
