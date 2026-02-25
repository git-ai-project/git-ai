#[macro_use]
mod repos;

use git_ai::authorship::attribution_tracker::LineAttribution;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::transcript::Message;
use git_ai::authorship::working_log::AgentId;
use git_ai::git::repository as GitAiRepository;
use insta::assert_debug_snapshot;
use rand::Rng;
use regex::Regex;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::GitTestMode;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(cwd: &Path, args: &[&str]) {
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
}

fn normalize_blame_output(blame_output: &str) -> String {
    let re_sha = Regex::new(r"[0-9a-f]{40}|[0-9a-f]{7,}").expect("valid sha regex");
    let result = re_sha.replace_all(blame_output, "COMMIT_SHA");
    let re_timestamp = Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [\+\-]\d{4}")
        .expect("valid timestamp regex");
    let result = re_timestamp.replace_all(&result, "TIMESTAMP");
    let re_author = Regex::new(r"\(([^)]+?)\s+TIMESTAMP").expect("valid author regex");
    re_author
        .replace_all(&result, "(AUTHOR TIMESTAMP")
        .to_string()
}

fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim().to_string();
        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        return (author_parts.join(" "), content);
    }
    ("unknown".to_string(), line.to_string())
}

fn assert_hooks_line_is_human_or_ai(repo: &repos::test_repo::TestRepo, path: &str, line: &str) {
    let blame_output = repo.git_ai(&["blame", path]).expect("blame should succeed");
    let parsed_lines: Vec<(String, String)> = blame_output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_blame_line)
        .collect();
    let matched = parsed_lines
        .iter()
        .find(|(_, content)| content.trim() == line)
        .expect("expected line missing from blame output");
    let author = matched.0.trim();
    assert!(
        author == "Test User" || author == "mock_ai",
        "Expected '{}' to be attributed to Test User or mock_ai, got '{}'\nBlame output:\n{}",
        line,
        author,
        blame_output
    );
}

fn assert_file_lines(repo: &repos::test_repo::TestRepo, path: &str, expected_lines: &[&str]) {
    let content = fs::read_to_string(repo.path().join(path)).expect("read file");
    let actual_lines: Vec<&str> = content.lines().collect();
    assert_eq!(actual_lines, expected_lines);
}

fn unique_worktree_path() -> PathBuf {
    let mut rng = rand::thread_rng();
    let n: u64 = rng.gen_range(0..10_000_000_000);
    std::env::temp_dir().join(format!("git-ai-worktree-{}", n))
}

worktree_test_wrappers! {
    fn repository_paths_and_storage_are_worktree_aware() {
        let repo = TestRepo::new();

        let common_dir = PathBuf::from(
            repo.git(&["rev-parse", "--git-common-dir"])
                .expect("resolve common dir")
                .trim(),
        );
        let git_dir = PathBuf::from(
            repo.git(&["rev-parse", "--git-dir"])
                .expect("resolve git dir")
                .trim(),
        );

        assert!(
            repo.path().join(".git").is_file(),
            "linked worktree should have a .git file"
        );

        let gitai_repo = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
            .expect("find git-ai repository");
        assert_eq!(
            gitai_repo.workdir().unwrap().canonicalize().unwrap(),
            repo.path().canonicalize().unwrap(),
            "workdir should match linked worktree root"
        );
        assert_eq!(
            gitai_repo.path().canonicalize().unwrap(),
            git_dir.canonicalize().unwrap(),
            "git dir should match rev-parse --git-dir for linked worktree"
        );

        let expected_prefix = common_dir.join("ai").join("worktrees");
        assert!(
            gitai_repo.storage.working_logs.starts_with(&expected_prefix),
            "working logs should live under common-dir isolated storage: {}",
            gitai_repo.storage.working_logs.display()
        );
    }
}

worktree_test_wrappers! {
    fn hooks_config_resolves_to_common_dir() {
        let repo = TestRepo::new();
        repo.git_ai(&["git-hooks", "ensure"])
            .expect("git-hooks ensure should succeed in linked worktree");

        let hooks_path = repo
            .git(&["config", "--local", "--get", "core.hooksPath"])
            .expect("core.hooksPath should be set")
            .trim()
            .to_string();
        let hooks_path = PathBuf::from(hooks_path);
        let common_dir = PathBuf::from(
            repo.git(&["rev-parse", "--git-common-dir"])
                .expect("resolve common dir")
                .trim(),
        );
        let expected_hooks = common_dir.join("ai").join("hooks");

        assert_eq!(
            hooks_path.canonicalize().unwrap(),
            expected_hooks.canonicalize().unwrap(),
            "managed hooks path should be stable across worktrees"
        );
    }
}

worktree_test_wrappers! {
    fn checkpoint_and_blame_support_absolute_paths_in_worktree() {
        let repo = TestRepo::new();
        let mut file = repo.filename("src/lib.rs");
        file.set_contents(lines!["fn a() {}".human(), "fn ai() {}".ai()]);
        repo.stage_all_and_commit("add file with ai lines").unwrap();

        let abs_path = repo.path().join("src/lib.rs");
        let output = repo
            .git_ai(&["blame", abs_path.to_str().unwrap()])
            .expect("blame should work for absolute path in worktree");
        assert!(output.contains("fn ai() {}"));
    }
}

worktree_test_wrappers! {
    fn diff_works_in_worktree_context() {
        let repo = TestRepo::new();
        let mut file = repo.filename("diff.txt");
        file.set_contents(lines!["old".human()]);
        repo.stage_all_and_commit("initial").unwrap();

        file.set_contents(lines!["new".ai()]);
        let commit = repo.stage_all_and_commit("ai update").unwrap();

        let output = repo
            .git_ai(&["diff", &commit.commit_sha])
            .expect("git-ai diff should succeed in worktree");

        assert!(output.contains("diff.txt"));
        assert!(output.contains("+new"));
    }
}

worktree_test_wrappers! {
    fn stash_pop_preserves_ai_authorship() {
        let repo = TestRepo::new();
        let mut file = repo.filename("stash.txt");
        file.set_contents(lines!["base".human()]);
        repo.stage_all_and_commit("base").unwrap();

        file.set_contents(lines!["base".human(), "ai stash line".ai()]);
        repo.git(&["stash", "push", "-u", "-m", "wip"]).unwrap();
        repo.git(&["stash", "pop"]).unwrap();
        repo.stage_all_and_commit("apply stash").unwrap();

        if matches!(TestRepo::git_mode(), GitTestMode::Hooks) {
            assert_file_lines(&repo, "stash.txt", &["base", "ai stash line"]);
            assert_hooks_line_is_human_or_ai(&repo, "stash.txt", "ai stash line");
        } else {
            file.assert_lines_and_blame(lines!["base".human(), "ai stash line".ai()]);
        }
    }
}

worktree_test_wrappers! {
    fn reset_mixed_reconstructs_working_log() {
        let repo = TestRepo::new();
        let mut file = repo.filename("reset.txt");
        file.set_contents(lines!["base".human()]);
        repo.stage_all_and_commit("base").unwrap();

        file.set_contents(lines!["base".human(), "ai reset line".ai()]);
        repo.stage_all_and_commit("ai commit").unwrap();

        repo.git(&["reset", "--mixed", "HEAD~1"])
            .expect("mixed reset should succeed");
        repo.stage_all_and_commit("recommit after reset").unwrap();

        if matches!(TestRepo::git_mode(), GitTestMode::Hooks) {
            assert_file_lines(&repo, "reset.txt", &["base", "ai reset line"]);
            assert_hooks_line_is_human_or_ai(&repo, "reset.txt", "ai reset line");
        } else {
            file.assert_lines_and_blame(lines!["base".human(), "ai reset line".ai()]);
        }
    }
}

worktree_test_wrappers! {
    fn rebase_preserves_ai_authorship() {
        let repo = TestRepo::new();
        let mut file = repo.filename("rebase.txt");
        file.set_contents(lines!["base".human()]);
        repo.stage_all_and_commit("base").unwrap();
        repo.git(&["checkout", "-b", "integration"]).unwrap();

        repo.git(&["checkout", "-b", "feature", "integration"]).unwrap();
        file.set_contents(lines!["base".human(), "feature ai line".ai()]);
        repo.stage_all_and_commit("feature ai").unwrap();

        repo.git(&["checkout", "integration"]).unwrap();
        let mut main_only = repo.filename("main-only.txt");
        main_only.set_contents(lines!["main human".human()]);
        repo.stage_all_and_commit("main human commit").unwrap();

        repo.git(&["checkout", "feature"]).unwrap();
        repo.git(&["rebase", "integration"]).unwrap();

        if matches!(TestRepo::git_mode(), GitTestMode::Hooks) {
            assert_file_lines(&repo, "rebase.txt", &["base", "feature ai line"]);
            assert_hooks_line_is_human_or_ai(&repo, "rebase.txt", "feature ai line");
        } else {
            file.assert_lines_and_blame(lines!["base".human(), "feature ai line".ai()]);
        }
    }
}

worktree_test_wrappers! {
    fn cherry_pick_preserves_ai_authorship() {
        let repo = TestRepo::new();
        let mut file = repo.filename("cherry.txt");
        file.set_contents(lines!["base".human()]);
        repo.stage_all_and_commit("base").unwrap();
        repo.git(&["checkout", "-b", "integration"]).unwrap();

        repo.git(&["checkout", "-b", "feature", "integration"]).unwrap();
        file.set_contents(lines!["base".human(), "feature ai".ai()]);
        let ai_commit = repo.stage_all_and_commit("feature ai").unwrap();

        repo.git(&["checkout", "integration"]).unwrap();
        repo.git(&["cherry-pick", &ai_commit.commit_sha]).unwrap();

        if matches!(TestRepo::git_mode(), GitTestMode::Hooks) {
            assert_file_lines(&repo, "cherry.txt", &["base", "feature ai"]);
            assert_hooks_line_is_human_or_ai(&repo, "cherry.txt", "feature ai");
        } else {
            file.assert_lines_and_blame(lines!["base".human(), "feature ai".ai()]);
        }
    }
}

worktree_test_wrappers! {
    fn multi_worktree_storage_isolation_prevents_cross_talk() {
        let repo = TestRepo::new();
        let common_dir = PathBuf::from(
            repo.git(&["rev-parse", "--git-common-dir"])
                .expect("resolve common dir")
                .trim(),
        );
        let main_repo_dir = common_dir.parent().expect("main repo dir");
        let second_worktree = unique_worktree_path();

        run_git(
            main_repo_dir,
            &["worktree", "add", second_worktree.to_str().unwrap()],
        );

        let repo_one = GitAiRepository::find_repository_in_path(repo.path().to_str().unwrap())
            .expect("find first worktree repo");
        let repo_two =
            GitAiRepository::find_repository_in_path(second_worktree.to_str().unwrap())
                .expect("find second worktree repo");

        let expected_prefix = common_dir.join("ai").join("worktrees");
        assert!(repo_one.storage.working_logs.starts_with(&expected_prefix));
        assert!(repo_two.storage.working_logs.starts_with(&expected_prefix));
        assert_ne!(
            repo_one.storage.working_logs,
            repo_two.storage.working_logs,
            "distinct linked worktrees must not share the same working_logs path"
        );

        let wl_one = repo_one.storage.working_log_for_base_commit("initial");
        let wl_two = repo_two.storage.working_log_for_base_commit("initial");
        fs::write(wl_one.dir.join("sentinel"), "one").expect("write sentinel one");
        assert!(
            !wl_two.dir.join("sentinel").exists(),
            "worktree-local storage should remain isolated"
        );
    }
}

worktree_test_wrappers! {
    fn worktree_initial_attributions_snapshot() {
        let repo = TestRepo::new();

        let mut readme = repo.filename("README.md");
        readme.set_contents(lines!["# Test Repo"]);
        repo.stage_all_and_commit("initial commit").unwrap();

        let working_log = repo.current_working_logs();
        let mut initial_attributions = HashMap::new();
        initial_attributions.insert(
            "initial.txt".to_string(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 2,
                author_id: "initial-ai-1".to_string(),
                overrode: None,
            }],
        );
        let mut prompts = HashMap::new();
        prompts.insert(
            "initial-ai-1".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "test-tool".to_string(),
                    id: "session-1".to_string(),
                    model: "test-model".to_string(),
                },
                human_author: None,
                messages: vec![Message::assistant("initial".to_string(), None)],
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                messages_url: None,
            },
        );
        working_log
            .write_initial_attributions(initial_attributions, prompts)
            .expect("write initial attributions");

        fs::write(repo.path().join("initial.txt"), "a\nb\n").expect("write file");
        repo.git_ai(&["checkpoint"]).unwrap();
        repo.stage_all_and_commit("commit initial attribution")
            .unwrap();

        let blame_output = repo.git_ai(&["blame", "initial.txt"]).unwrap();
        let normalized = normalize_blame_output(&blame_output);
        assert_debug_snapshot!(normalized);
    }
}

worktree_test_wrappers! {
    fn worktree_stats_snapshot() {
        let repo = TestRepo::new();
        let mut file = repo.filename("stats.txt");
        file.set_contents(lines!["one".human(), "two".ai(), "three".ai()]);
        repo.stage_all_and_commit("stats seed").unwrap();

        let stats = repo.stats().expect("stats should succeed");
        if matches!(TestRepo::git_mode(), GitTestMode::Hooks) {
            assert_eq!(
                stats.human_additions + stats.ai_additions + stats.mixed_additions,
                3
            );
            assert_eq!(stats.git_diff_added_lines, 3);
            assert_eq!(stats.git_diff_deleted_lines, 0);
        } else {
            assert_debug_snapshot!(stats);
        }
    }
}
