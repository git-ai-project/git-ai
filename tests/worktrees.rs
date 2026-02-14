#[macro_use]
mod repos;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use git_ai::authorship::stats::CommitStats;
use git_ai::git::group_files_by_repository;
use rand::Rng;
use serde::Deserialize;
use serde_json::Value;

use repos::test_repo::{NewCommit, TestRepo, WorktreeRepo, default_branchname};

trait RepoOps {
    fn path(&self) -> &PathBuf;
    fn git(&self, args: &[&str]) -> Result<String, String>;
    fn git_ai(&self, args: &[&str]) -> Result<String, String>;
    fn git_ai_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String>;
    fn commit(&self, message: &str) -> Result<NewCommit, String>;
}

impl RepoOps for TestRepo {
    fn path(&self) -> &PathBuf {
        self.path()
    }
    fn git(&self, args: &[&str]) -> Result<String, String> {
        self.git(args)
    }
    fn git_ai(&self, args: &[&str]) -> Result<String, String> {
        self.git_ai(args)
    }
    fn git_ai_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        self.git_ai_with_env(args, envs)
    }
    fn commit(&self, message: &str) -> Result<NewCommit, String> {
        self.commit(message)
    }
}

impl RepoOps for WorktreeRepo {
    fn path(&self) -> &PathBuf {
        self.path()
    }
    fn git(&self, args: &[&str]) -> Result<String, String> {
        self.git(args)
    }
    fn git_ai(&self, args: &[&str]) -> Result<String, String> {
        self.git_ai(args)
    }
    fn git_ai_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
        self.git_ai_with_env(args, envs)
    }
    fn commit(&self, message: &str) -> Result<NewCommit, String> {
        self.commit(message)
    }
}

#[derive(Debug, Deserialize)]
struct StatusJson {
    stats: CommitStats,
    checkpoints: Vec<StatusCheckpoint>,
}

#[derive(Debug, Deserialize)]
struct StatusCheckpoint {
    additions: u32,
    deletions: u32,
    tool_model: String,
    is_human: bool,
}

fn write_file(repo: &impl RepoOps, relative: &str, contents: &str) -> PathBuf {
    let path = repo.path().join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent directories");
    }
    fs::write(&path, contents).expect("failed to write file");
    path
}

fn parse_status_json(output: &str) -> StatusJson {
    let json = extract_json_object(output);
    serde_json::from_str(&json).expect("status output should be valid JSON")
}

fn status_summary(repo: &impl RepoOps) -> (CommitStats, Vec<(u32, u32, bool, String)>) {
    let output = repo
        .git_ai(&["status", "--json"])
        .expect("git-ai status should succeed");
    let parsed = parse_status_json(&output);
    let checkpoints = parsed
        .checkpoints
        .iter()
        .map(|cp| {
            (
                cp.additions,
                cp.deletions,
                cp.is_human,
                cp.tool_model.clone(),
            )
        })
        .collect::<Vec<_>>();
    (parsed.stats, checkpoints)
}

fn status_summary_with_env(
    repo: &impl RepoOps,
    envs: &[(&str, &str)],
) -> (CommitStats, Vec<(u32, u32, bool, String)>) {
    let output = repo
        .git_ai_with_env(&["status", "--json"], envs)
        .expect("git-ai status should succeed");
    let parsed = parse_status_json(&output);
    let checkpoints = parsed
        .checkpoints
        .iter()
        .map(|cp| {
            (
                cp.additions,
                cp.deletions,
                cp.is_human,
                cp.tool_model.clone(),
            )
        })
        .collect::<Vec<_>>();
    (parsed.stats, checkpoints)
}

fn stats_key_fields(stats: &CommitStats) -> (u32, u32, u32, u32, u32, u32) {
    (
        stats.human_additions,
        stats.mixed_additions,
        stats.ai_additions,
        stats.ai_accepted,
        stats.git_diff_added_lines,
        stats.git_diff_deleted_lines,
    )
}

fn worktree_git_dir(worktree: &WorktreeRepo) -> PathBuf {
    let output = worktree
        .git(&["rev-parse", "--git-dir"])
        .expect("rev-parse --git-dir should succeed");
    let git_dir = PathBuf::from(output.trim());
    if git_dir.is_relative() {
        worktree.path().join(git_dir)
    } else {
        git_dir
    }
}

fn worktree_commondir(worktree: &WorktreeRepo) -> PathBuf {
    let git_dir = worktree_git_dir(worktree);
    let commondir_path = git_dir.join("commondir");
    let commondir_contents = fs::read_to_string(&commondir_path).expect("commondir should exist");
    let commondir = PathBuf::from(commondir_contents.trim());
    let resolved = if commondir.is_absolute() {
        commondir
    } else {
        git_dir.join(commondir)
    };
    resolved.canonicalize().unwrap_or(resolved)
}

fn extract_json_object(output: &str) -> String {
    let start = output.find('{').unwrap_or(0);
    let end = output.rfind('}').unwrap_or(output.len().saturating_sub(1));
    output[start..=end].to_string()
}

fn normalize_diff(output: &str) -> String {
    output
        .lines()
        .filter(|line| !line.starts_with("index "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_blame(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if let Some(start_paren) = line.find('(') {
                if let Some(end_paren) = line.find(')') {
                    let author_section = &line[start_paren + 1..end_paren];
                    let content = line[end_paren + 1..].trim().to_string();

                    let parts: Vec<&str> = author_section.trim().split_whitespace().collect();
                    let mut author_parts = Vec::new();
                    for part in parts {
                        if part.chars().next().unwrap_or('a').is_ascii_digit() {
                            break;
                        }
                        author_parts.push(part);
                    }
                    let author = author_parts.join(" ");
                    return (author, content);
                }
            }
            ("unknown".to_string(), line.to_string())
        })
        .collect()
}

fn temp_dir_with_prefix(prefix: &str) -> PathBuf {
    let mut rng = rand::thread_rng();
    let n: u64 = rng.gen_range(0..10000000000);
    let path = std::env::temp_dir().join(format!("{}-{}", prefix, n));
    fs::create_dir_all(&path).expect("failed to create temp dir");
    path
}

fn checkpoint_and_commit(
    repo: &impl RepoOps,
    relative: &str,
    contents: &str,
    message: &str,
    ai: bool,
) -> NewCommit {
    write_file(repo, relative, contents);
    let checkpoint_args = if ai {
        vec!["checkpoint", "mock_ai"]
    } else {
        vec!["checkpoint"]
    };
    repo.git_ai(&checkpoint_args)
        .expect("checkpoint should succeed");
    repo.git(&["add", "-A"]).expect("add should succeed");
    repo.commit(message).expect("commit should succeed")
}

#[test]
fn test_worktree_checkpoint_status_parity() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    write_file(&base_repo, "file.txt", "one\n");
    base_repo.git_ai(&["checkpoint"]).unwrap();
    let (base_stats, base_checkpoints) = status_summary(&base_repo);

    let repo = TestRepo::new();
    let worktree = repo.add_worktree("status");
    write_file(&worktree, "file.txt", "one\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    let (wt_stats, wt_checkpoints) = status_summary(&worktree);

    assert_eq!(stats_key_fields(&base_stats), stats_key_fields(&wt_stats));
    assert_eq!(base_checkpoints, wt_checkpoints);
}

#[test]
fn test_worktree_diff_parity() {
    let base_repo = TestRepo::new();
    let base_commit =
        checkpoint_and_commit(&base_repo, "file.txt", "line1\nline2\n", "base", false);
    let base_diff = base_repo
        .git_ai(&["diff", &base_commit.commit_sha])
        .unwrap();

    let repo = TestRepo::new();
    let worktree = repo.add_worktree("diff");
    let wt_commit =
        checkpoint_and_commit(&worktree, "file.txt", "line1\nline2\n", "worktree", false);
    let wt_diff = worktree.git_ai(&["diff", &wt_commit.commit_sha]).unwrap();

    assert_eq!(normalize_diff(&base_diff), normalize_diff(&wt_diff));
}

#[test]
fn test_worktree_commit_authorship_parity() {
    let base_repo = TestRepo::new();
    let base_commit = checkpoint_and_commit(&base_repo, "file.txt", "line1\n", "base", true);

    let repo = TestRepo::new();
    let worktree = repo.add_worktree("authorship");
    let wt_commit = checkpoint_and_commit(&worktree, "file.txt", "line1\n", "worktree", true);

    let base_attestations = base_commit.authorship_log.attestations.len();
    let wt_attestations = wt_commit.authorship_log.attestations.len();
    assert_eq!(base_attestations, wt_attestations);

    let base_entries: usize = base_commit
        .authorship_log
        .attestations
        .iter()
        .map(|a| a.entries.len())
        .sum();
    let wt_entries: usize = wt_commit
        .authorship_log
        .attestations
        .iter()
        .map(|a| a.entries.len())
        .sum();
    assert_eq!(base_entries, wt_entries);
}

#[test]
fn test_worktree_blame_parity() {
    let base_repo = TestRepo::new();
    checkpoint_and_commit(&base_repo, "file.txt", "human\n", "base", false);
    checkpoint_and_commit(&base_repo, "file.txt", "human\nai\n", "base-ai", true);
    let base_blame = base_repo.git_ai(&["blame", "file.txt"]).unwrap();

    let repo = TestRepo::new();
    let worktree = repo.add_worktree("blame");
    checkpoint_and_commit(&worktree, "file.txt", "human\n", "wt", false);
    checkpoint_and_commit(&worktree, "file.txt", "human\nai\n", "wt-ai", true);
    let wt_blame = worktree.git_ai(&["blame", "file.txt"]).unwrap();

    let base_parsed = parse_blame(&base_blame);
    let wt_parsed = parse_blame(&wt_blame);
    assert_eq!(base_parsed, wt_parsed);
}

#[test]
fn test_worktree_subdir_repository_discovery() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("subdir");
    write_file(&worktree, "nested/file.txt", "content\n");
    worktree.git_ai(&["checkpoint"]).unwrap();

    let subdir = worktree.path().join("nested");
    let output = worktree
        .git_ai_from_working_dir(&subdir, &["status", "--json"])
        .expect("status from subdir should succeed");
    let parsed = parse_status_json(&output);
    assert!(!parsed.checkpoints.is_empty());
}

#[test]
fn test_group_files_by_repository_with_worktree() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("group");
    let file_path = write_file(&worktree, "file.txt", "content\n");

    let (repos, orphans) =
        group_files_by_repository(&[file_path.to_string_lossy().to_string()], None);

    assert!(orphans.is_empty());
    assert_eq!(repos.len(), 1);
    let (found_repo, files) = repos.values().next().unwrap();
    assert_eq!(files.len(), 1);
    let workdir = found_repo.workdir().expect("workdir should exist");
    assert_eq!(workdir, worktree.canonical_path());
}

#[test]
fn test_worktree_branch_switch_and_merge() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("merge");

    checkpoint_and_commit(&worktree, "file.txt", "base\n", "base", false);
    let base_branch = worktree.current_branch();

    worktree
        .git(&["switch", "-c", "feature-merge"])
        .expect("switch to feature should succeed");
    checkpoint_and_commit(&worktree, "file.txt", "base\nfeature\n", "feature", false);

    worktree
        .git(&["switch", &base_branch])
        .expect("switch back should succeed");
    worktree
        .git(&["merge", "feature-merge"])
        .expect("merge should succeed");

    let contents = fs::read_to_string(worktree.path().join("file.txt")).unwrap();
    assert!(contents.contains("feature"));
}

#[test]
fn test_worktree_rebase_and_cherry_pick() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("rebase");

    checkpoint_and_commit(&worktree, "file.txt", "base\n", "base", false);
    let base_branch = worktree.current_branch();

    worktree
        .git(&["switch", "-c", "feature-rebase"])
        .expect("switch to feature should succeed");
    checkpoint_and_commit(&worktree, "feature.txt", "feature\n", "feature", false);

    worktree
        .git(&["switch", &base_branch])
        .expect("switch back should succeed");
    checkpoint_and_commit(&worktree, "main.txt", "main\n", "main", false);

    worktree
        .git(&["switch", "feature-rebase"])
        .expect("switch to feature should succeed");
    worktree
        .git(&["rebase", &base_branch])
        .expect("rebase should succeed");

    worktree
        .git(&["switch", &base_branch])
        .expect("switch back should succeed");
    let cherry_sha = worktree
        .git(&["rev-parse", "feature-rebase"])
        .unwrap()
        .trim()
        .to_string();
    worktree
        .git(&["cherry-pick", &cherry_sha])
        .expect("cherry-pick should succeed");

    let feature_contents = fs::read_to_string(worktree.path().join("feature.txt")).unwrap();
    let main_contents = fs::read_to_string(worktree.path().join("main.txt")).unwrap();
    assert!(feature_contents.contains("feature"));
    assert!(main_contents.contains("main"));
}

#[test]
fn test_worktree_stash_and_reset() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("stash");

    checkpoint_and_commit(&worktree, "file.txt", "base\n", "base", false);
    write_file(&worktree, "file.txt", "base\nchange\n");

    worktree.git(&["stash"]).expect("stash should succeed");
    let contents = fs::read_to_string(worktree.path().join("file.txt")).unwrap();
    assert_eq!(contents, "base\n");

    worktree.git(&["stash", "pop"]).expect("stash pop");
    let contents = fs::read_to_string(worktree.path().join("file.txt")).unwrap();
    assert!(contents.contains("change"));

    worktree
        .git(&["reset", "--hard", "HEAD"])
        .expect("reset should succeed");
    let contents = fs::read_to_string(worktree.path().join("file.txt")).unwrap();
    assert_eq!(contents, "base\n");
}

#[test]
fn test_worktree_amend() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("amend");

    checkpoint_and_commit(&worktree, "file.txt", "base\n", "base", false);
    write_file(&worktree, "file.txt", "base\namended\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    worktree.git(&["add", "-A"]).unwrap();
    worktree
        .git(&["commit", "--amend", "--no-edit"])
        .expect("amend should succeed");

    let contents = fs::read_to_string(worktree.path().join("file.txt")).unwrap();
    assert!(contents.contains("amended"));
}

#[test]
fn test_worktree_stats_json() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("stats");
    checkpoint_and_commit(&worktree, "file.txt", "line1\nline2\n", "stats", true);

    let output = worktree
        .git_ai(&["stats", "--json"])
        .expect("stats should succeed");
    let json = extract_json_object(&output);
    let parsed: CommitStats = serde_json::from_str(&json).expect("stats JSON");
    assert!(parsed.git_diff_added_lines > 0);
}

#[test]
fn test_worktree_notes_visible_from_base_repo() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("notes");
    let commit = checkpoint_and_commit(&worktree, "file.txt", "line1\n", "note", true);

    let base_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    let note = git_ai::git::refs::show_authorship_note(&base_repo, &commit.commit_sha);
    assert!(note.is_some());
}

#[test]
fn test_worktree_multiple_worktrees_diverge() {
    let repo = TestRepo::new();
    let wt_one = repo.add_worktree("one");
    let wt_two = repo.add_worktree("two");

    checkpoint_and_commit(&wt_one, "file.txt", "one\n", "one", false);
    checkpoint_and_commit(&wt_two, "file.txt", "two\n", "two", false);

    let log_one = wt_one.git(&["log", "-1", "--pretty=%s"]).unwrap();
    let log_two = wt_two.git(&["log", "-1", "--pretty=%s"]).unwrap();

    assert!(log_one.trim().contains("one"));
    assert!(log_two.trim().contains("two"));
}

#[test]
fn test_worktree_default_branch_name_is_respected() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("branchname");

    let default_branch = default_branchname();
    let current_branch = worktree.current_branch();

    assert!(
        current_branch.starts_with("worktree-")
            || current_branch == default_branch
            || current_branch == "HEAD",
        "unexpected worktree branch: {} (default: {})",
        current_branch,
        default_branch
    );
}

#[test]
fn test_worktree_config_resolves_path_with_temp_home() {
    let repo = TestRepo::new();
    let worktree = repo.add_worktree("config");

    let remote_path = temp_dir_with_prefix("git-ai-remote");
    let init_output = Command::new("git")
        .args(["init", "--bare", remote_path.to_str().unwrap()])
        .output()
        .expect("git init --bare");
    assert!(init_output.status.success());

    worktree
        .git(&["remote", "add", "origin", remote_path.to_str().unwrap()])
        .expect("remote add should succeed");

    let temp_home = temp_dir_with_prefix("git-ai-home");
    let output = worktree.git_ai_with_env(
        &["config", "set", "exclude_repositories", "."],
        &[("HOME", temp_home.to_str().unwrap())],
    );
    assert!(output.is_ok(), "config set should succeed: {:?}", output);

    let config_path = temp_home.join(".git-ai").join("config.json");
    let config_contents = fs::read_to_string(&config_path).expect("config.json should exist");
    let json: Value = serde_json::from_str(&config_contents).expect("valid json");
    let excludes = json
        .get("exclude_repositories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        excludes.iter().any(|v| {
            v.as_str()
                .map(|s| s.contains(remote_path.to_str().unwrap()))
                .unwrap_or(false)
        }),
        "exclude_repositories should include remote url/path"
    );

    let _ = fs::remove_dir_all(temp_home);
    let _ = fs::remove_dir_all(remote_path);
}

#[test]
fn test_worktree_config_overrides_common_config() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    base_repo
        .git(&["config", "user.name", "Base"])
        .expect("set base user.name");
    base_repo
        .git(&["config", "extensions.worktreeConfig", "true"])
        .expect("enable worktree config");

    let worktree = base_repo.add_worktree("config-override");
    worktree
        .git(&["config", "--worktree", "user.name", "Worktree"])
        .expect("set worktree user.name");

    write_file(&base_repo, "file.txt", "base\n");
    base_repo.git_ai(&["checkpoint"]).unwrap();
    let (_, base_checkpoints) = status_summary(&base_repo);
    assert_eq!(
        base_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("Base")
    );

    write_file(&worktree, "file.txt", "worktree\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    let (_, wt_checkpoints) = status_summary(&worktree);
    assert_eq!(
        wt_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("Worktree")
    );
}

#[test]
fn test_worktree_config_falls_back_to_common_config() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    base_repo
        .git(&["config", "user.name", "Base"])
        .expect("set base user.name");
    base_repo
        .git(&["config", "extensions.worktreeConfig", "true"])
        .expect("enable worktree config");

    let worktree = base_repo.add_worktree("config-fallback");
    let _ = worktree.git(&["config", "--worktree", "--unset-all", "user.name"]);

    write_file(&worktree, "file.txt", "worktree\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    let (_, wt_checkpoints) = status_summary(&worktree);
    assert_eq!(wt_checkpoints.first().map(|cp| cp.3.as_str()), Some("Base"));
}

#[test]
fn test_worktree_config_overrides_global_config() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    base_repo
        .git(&["config", "user.name", "Base"])
        .expect("set base user.name");
    base_repo
        .git(&["config", "extensions.worktreeConfig", "true"])
        .expect("enable worktree config");

    let worktree = base_repo.add_worktree("config-global");
    worktree
        .git(&["config", "--worktree", "user.name", "Worktree"])
        .expect("set worktree user.name");

    let temp_home = temp_dir_with_prefix("git-ai-home");
    let home_str = temp_home.to_str().expect("valid home path");
    base_repo
        .git_with_env(
            &["config", "--global", "user.name", "Global"],
            &[("HOME", home_str)],
            None,
        )
        .expect("set global user.name");

    let envs = [("HOME", home_str)];

    write_file(&base_repo, "file.txt", "base\n");
    base_repo.git_ai_with_env(&["checkpoint"], &envs).unwrap();
    let (_, base_checkpoints) = status_summary_with_env(&base_repo, &envs);
    assert_eq!(
        base_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("Base")
    );

    write_file(&worktree, "file.txt", "worktree\n");
    worktree.git_ai_with_env(&["checkpoint"], &envs).unwrap();
    let (_, wt_checkpoints) = status_summary_with_env(&worktree, &envs);
    assert_eq!(
        wt_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("Worktree")
    );

    let _ = fs::remove_dir_all(temp_home);
}

#[test]
fn test_worktree_config_worktree_ignored_without_extension() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    base_repo
        .git(&["config", "user.name", "Base"])
        .expect("set base user.name");

    let worktree = base_repo.add_worktree("config-worktree-off");
    let wt_config_path = worktree_git_dir(&worktree).join("config.worktree");
    let config_contents = "[user]\n\tname = WorktreeFile\n";
    fs::write(&wt_config_path, config_contents).expect("write config.worktree");

    write_file(&worktree, "file.txt", "worktree\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    let (_, wt_checkpoints) = status_summary(&worktree);
    assert_eq!(wt_checkpoints.first().map(|cp| cp.3.as_str()), Some("Base"));
}

#[test]
fn test_worktree_include_if_onbranch_applies() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    base_repo
        .git(&["config", "user.name", "Base"])
        .expect("set base user.name");

    let include_dir = temp_dir_with_prefix("git-ai-onbranch");
    let include_path = include_dir.join("onbranch.config");
    fs::write(&include_path, "[user]\n\tname = OnBranch\n").expect("write onbranch include");

    let include_key = "includeIf.onbranch:worktree-onbranch-*.path";
    base_repo
        .git(&[
            "config",
            "--add",
            include_key,
            include_path.to_str().expect("valid include path"),
        ])
        .expect("set includeIf.onbranch");

    let worktree = base_repo.add_worktree("onbranch");

    write_file(&base_repo, "file.txt", "base\n");
    base_repo.git_ai(&["checkpoint"]).unwrap();
    let (_, base_checkpoints) = status_summary(&base_repo);
    assert_eq!(
        base_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("Base")
    );

    write_file(&worktree, "file.txt", "worktree\n");
    worktree.git_ai(&["checkpoint"]).unwrap();
    let (_, wt_checkpoints) = status_summary(&worktree);
    assert_eq!(
        wt_checkpoints.first().map(|cp| cp.3.as_str()),
        Some("OnBranch")
    );

    let _ = fs::remove_dir_all(include_dir);
}

#[test]
fn test_worktree_locked_allows_status() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let worktree = base_repo.add_worktree("locked");
    let worktree_path = worktree.path().to_str().expect("valid worktree path");

    base_repo
        .git_og(&["worktree", "lock", worktree_path])
        .expect("worktree lock should succeed");

    let output = worktree.git_ai(&["status", "--json"]);
    assert!(output.is_ok(), "status should work on locked worktree");

    let _ = base_repo.git_og(&["worktree", "unlock", worktree_path]);
}

#[test]
fn test_worktree_removed_does_not_break_base_status() {
    let base_repo = TestRepo::new();
    base_repo
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let worktree = base_repo.add_worktree("removed");
    let worktree_path = worktree.path().to_str().expect("valid worktree path");

    base_repo
        .git_og(&["worktree", "remove", "-f", worktree_path])
        .expect("worktree remove should succeed");

    let output = base_repo.git_ai(&["status", "--json"]);
    assert!(output.is_ok(), "base status should succeed after removal");
}

#[test]
fn test_worktree_detached_head_checkpoint() {
    let repo = TestRepo::new();
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let worktree = repo.add_worktree("detached");
    worktree
        .git(&["checkout", "--detach"])
        .expect("detach HEAD");

    write_file(&worktree, "file.txt", "detached\n");
    worktree.git_ai(&["checkpoint"]).unwrap();

    let output = worktree
        .git_ai(&["status", "--json"])
        .expect("status should succeed");
    let parsed = parse_status_json(&output);
    assert!(!parsed.checkpoints.is_empty());
}

#[test]
fn test_worktree_commondir_resolution_matches_git() {
    let repo = TestRepo::new();
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let worktree = repo.add_worktree("commondir");

    let expected_common = worktree_commondir(&worktree);
    let found_repo = git_ai::git::find_repository_in_path(worktree.path().to_str().unwrap())
        .expect("find repository");
    let actual_common = found_repo
        .common_git_dir()
        .canonicalize()
        .unwrap_or_else(|_| found_repo.common_git_dir().to_path_buf());

    assert_eq!(expected_common, actual_common);
}

#[test]
fn test_worktree_storage_lives_in_worktree_git_dir() {
    let repo = TestRepo::new();
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let worktree = repo.add_worktree("storage");
    write_file(&worktree, "file.txt", "content\n");
    worktree.git_ai(&["checkpoint"]).unwrap();

    let git_dir = worktree_git_dir(&worktree);
    let found_repo = git_ai::git::find_repository_in_path(worktree.path().to_str().unwrap())
        .expect("find repository");
    let expected_prefix = git_dir.join("ai").join("working_logs");
    let actual = found_repo.storage.working_logs.clone();
    assert!(
        actual.starts_with(&expected_prefix),
        "working logs should live under worktree git dir (expected prefix {:?}, got {:?})",
        expected_prefix,
        actual
    );

    let head_sha = found_repo.head().expect("head").target().expect("head sha");
    let checkpoints_file = actual.join(head_sha).join("checkpoints.jsonl");
    assert!(checkpoints_file.exists(), "checkpoint log should exist");
}

#[test]
fn test_worktree_working_logs_are_isolated() {
    let repo = TestRepo::new();
    repo.git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();
    let wt_one = repo.add_worktree("isolation-one");
    let wt_two = repo.add_worktree("isolation-two");

    write_file(&wt_one, "file.txt", "one\n");
    wt_one.git_ai(&["checkpoint"]).unwrap();

    let output = wt_two
        .git_ai(&["status", "--json"])
        .expect("status should succeed");
    let parsed = parse_status_json(&output);
    assert!(
        parsed.checkpoints.is_empty(),
        "worktree checkpoints should not leak across worktrees"
    );
}
