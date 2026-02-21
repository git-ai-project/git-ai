mod repos;

use git_ai::authorship::stats::CommitStats;
use repos::test_repo::TestRepo;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct StatusOutput {
    stats: CommitStats,
    checkpoints: Vec<serde_json::Value>,
}

fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

fn status_from_args(repo: &TestRepo, args: &[&str]) -> StatusOutput {
    let raw = repo.git_ai(args).expect("git-ai status should succeed");
    let json = extract_json_object(&raw);
    serde_json::from_str(&json).expect("valid status json")
}

fn write_file(repo: &TestRepo, path: &str, contents: &str) {
    let abs_path = repo.path().join(path);
    if let Some(parent) = abs_path.parent() {
        std::fs::create_dir_all(parent).expect("parent directory should be creatable");
    }
    std::fs::write(abs_path, contents).expect("file write should succeed");
}

#[test]
fn test_checkpoint_ignores_default_lockfiles_integration() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, "README.md", "# repo\nupdated\n");
    write_file(&repo, "Cargo.lock", "# lock\n# lock2\n# lock3\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    let checkpoints = repo.current_working_logs().read_all_checkpoints().unwrap();
    let latest = checkpoints.last().expect("checkpoint should be present");

    assert!(
        latest.entries.iter().any(|entry| entry.file == "README.md"),
        "Expected non-ignored file to be checkpointed"
    );
    assert!(
        latest
            .entries
            .iter()
            .all(|entry| entry.file != "Cargo.lock"),
        "Expected Cargo.lock to be filtered out by default ignores"
    );
}

#[test]
fn test_checkpoint_honors_uncommitted_root_gitattributes_linguist_generated_integration() {
    let repo = TestRepo::new();

    write_file(&repo, "src/main.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(
        &repo,
        ".gitattributes",
        "generated/** linguist-generated=true\n",
    );
    write_file(&repo, "src/main.rs", "fn main() {}\nfn added() {}\n");
    write_file(
        &repo,
        "generated/api.generated.ts",
        "export const one = 1;\nexport const two = 2;\n",
    );

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/main.rs",
        "generated/api.generated.ts",
    ])
    .unwrap();

    let checkpoints = repo.current_working_logs().read_all_checkpoints().unwrap();
    let latest = checkpoints.last().expect("checkpoint should be present");

    assert!(
        latest
            .entries
            .iter()
            .any(|entry| entry.file == "src/main.rs"),
        "Expected regular source file to be checkpointed"
    );
    assert!(
        latest
            .entries
            .iter()
            .all(|entry| entry.file != "generated/api.generated.ts"),
        "Expected linguist-generated file to be filtered out"
    );
}

#[test]
fn test_status_default_ignores_affect_git_diff_and_ai_accepted() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, "README.md", "# repo\nnew ai line\n");
    write_file(&repo, "Cargo.lock", "# lock\n# lock2\n# lock3\n");

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    let status = status_from_args(&repo, &["status", "--json"]);

    assert_eq!(status.stats.git_diff_added_lines, 1);
    assert_eq!(status.stats.git_diff_deleted_lines, 0);
    assert_eq!(status.stats.ai_accepted, 1);
    assert!(
        !status.checkpoints.is_empty(),
        "status should report at least one checkpoint"
    );
}

#[test]
fn test_status_honors_uncommitted_root_gitattributes_linguist_generated() {
    let repo = TestRepo::new();

    write_file(&repo, "src/app.ts", "export const app = 1;\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(
        &repo,
        ".gitattributes",
        "generated/** linguist-generated=true\n",
    );
    write_file(
        &repo,
        "src/app.ts",
        "export const app = 1;\nexport const next = 2;\n",
    );
    write_file(
        &repo,
        "generated/out.generated.ts",
        "export const generatedA = 1;\nexport const generatedB = 2;\n",
    );

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/app.ts",
        "generated/out.generated.ts",
    ])
    .unwrap();

    let status = status_from_args(&repo, &["status", "--json"]);

    assert_eq!(status.stats.git_diff_added_lines, 1);
    assert_eq!(status.stats.git_diff_deleted_lines, 0);
    assert_eq!(status.stats.ai_accepted, 1);
}

#[test]
fn test_status_with_only_ignored_changes_reports_zero_diff() {
    let repo = TestRepo::new();

    write_file(&repo, "README.md", "# repo\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, "Cargo.lock", "# lock\n# lock2\n# lock3\n");

    let status = status_from_args(&repo, &["status", "--json"]);

    assert_eq!(status.stats.git_diff_added_lines, 0);
    assert_eq!(status.stats.git_diff_deleted_lines, 0);
    assert_eq!(status.stats.ai_accepted, 0);
}

#[test]
fn test_checkpoint_honors_git_ai_ignore_file() {
    let repo = TestRepo::new();

    write_file(&repo, "src/main.rs", "fn main() {}\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, ".git-ai-ignore", "docs/**\n");
    write_file(&repo, "src/main.rs", "fn main() {}\nfn added() {}\n");
    write_file(
        &repo,
        "docs/guide.md",
        "# Guide\nLine 1\nLine 2\n",
    );

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/main.rs",
        "docs/guide.md",
    ])
    .unwrap();

    let checkpoints = repo.current_working_logs().read_all_checkpoints().unwrap();
    let latest = checkpoints.last().expect("checkpoint should be present");

    assert!(
        latest
            .entries
            .iter()
            .any(|entry| entry.file == "src/main.rs"),
        "Expected regular source file to be checkpointed"
    );
    assert!(
        latest
            .entries
            .iter()
            .all(|entry| entry.file != "docs/guide.md"),
        "Expected .git-ai-ignore pattern to filter out docs/guide.md"
    );
}

#[test]
fn test_status_honors_git_ai_ignore_file() {
    let repo = TestRepo::new();

    write_file(&repo, "src/app.ts", "export const app = 1;\n");
    repo.stage_all_and_commit("initial").unwrap();

    write_file(&repo, ".git-ai-ignore", "docs/**\n");
    write_file(
        &repo,
        "src/app.ts",
        "export const app = 1;\nexport const next = 2;\n",
    );
    write_file(
        &repo,
        "docs/api.md",
        "# API\nendpoint 1\nendpoint 2\n",
    );

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/app.ts",
        "docs/api.md",
    ])
    .unwrap();

    let status = status_from_args(&repo, &["status", "--json"]);

    assert_eq!(status.stats.git_diff_added_lines, 1);
    assert_eq!(status.stats.git_diff_deleted_lines, 0);
    assert_eq!(status.stats.ai_accepted, 1);
}

#[test]
fn test_status_git_ai_ignore_union_with_gitattributes() {
    let repo = TestRepo::new();

    write_file(&repo, "src/app.ts", "export const app = 1;\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Set up both .gitattributes and .git-ai-ignore
    write_file(
        &repo,
        ".gitattributes",
        "generated/** linguist-generated=true\n",
    );
    write_file(&repo, ".git-ai-ignore", "docs/**\n");
    write_file(
        &repo,
        "src/app.ts",
        "export const app = 1;\nexport const next = 2;\n",
    );
    write_file(
        &repo,
        "generated/out.ts",
        "export const gen = 1;\nexport const gen2 = 2;\n",
    );
    write_file(
        &repo,
        "docs/api.md",
        "# API\nendpoint 1\nendpoint 2\n",
    );

    repo.git_ai(&[
        "checkpoint",
        "mock_ai",
        "src/app.ts",
        "generated/out.ts",
        "docs/api.md",
    ])
    .unwrap();

    let status = status_from_args(&repo, &["status", "--json"]);

    // Only src/app.ts addition should be counted (1 line)
    // generated/out.ts ignored by .gitattributes linguist-generated
    // docs/api.md ignored by .git-ai-ignore
    assert_eq!(status.stats.git_diff_added_lines, 1);
    assert_eq!(status.stats.git_diff_deleted_lines, 0);
    assert_eq!(status.stats.ai_accepted, 1);
}
