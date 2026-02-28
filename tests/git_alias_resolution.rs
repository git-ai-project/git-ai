#[macro_use]
mod repos;

use git_ai::commands::git_handlers::resolve_alias_invocation;
use git_ai::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use git_ai::git::find_repository_in_path;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn resolve(repo: &TestRepo, argv: &[&str]) -> Option<ParsedGitInvocation> {
    let parsed = parse_git_cli_args(&args(argv));
    let git_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("expected to find git repo");
    resolve_alias_invocation(&parsed, &git_repo)
}

// ─── Unit-style alias resolution tests ───────────────────────────────────────

#[test]
fn alias_with_args_resolves_command_for_hooks() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.ci", "commit -v"]).unwrap();

    let resolved = resolve(&repo, &["ci", "-m", "msg"]).expect("expected alias resolution");

    assert_eq!(resolved.command.as_deref(), Some("commit"));
    assert_eq!(
        resolved.command_args,
        vec!["-v".to_string(), "-m".to_string(), "msg".to_string()]
    );
}

#[test]
fn alias_chain_resolves_to_final_command() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.lg", "log --oneline"]).unwrap();
    repo.git(&["config", "alias.l", "lg -5"]).unwrap();

    let resolved = resolve(&repo, &["l"]).expect("expected alias resolution");

    assert_eq!(resolved.command.as_deref(), Some("log"));
    assert_eq!(
        resolved.command_args,
        vec!["--oneline".to_string(), "-5".to_string()]
    );
}

#[test]
fn alias_cycle_returns_none() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.a", "b"]).unwrap();
    repo.git(&["config", "alias.b", "a"]).unwrap();

    assert!(resolve(&repo, &["a"]).is_none());
}

#[test]
fn alias_self_recursive_with_args_returns_none() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.ls", "ls -la"]).unwrap();

    assert!(resolve(&repo, &["ls"]).is_none());
}

#[test]
fn shell_alias_returns_none() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.root", "!git rev-parse --show-toplevel"])
        .unwrap();

    assert!(resolve(&repo, &["root"]).is_none());
}

#[test]
fn alias_parsing_respects_quotes() {
    let repo = TestRepo::new();
    repo.git(&[
        "config",
        "alias.pretty",
        "log --pretty='format:%h %s' --abbrev-commit",
    ])
    .unwrap();

    let resolved = resolve(&repo, &["pretty"]).expect("expected alias resolution");

    assert_eq!(resolved.command.as_deref(), Some("log"));
    assert_eq!(
        resolved.command_args,
        vec![
            "--pretty=format:%h %s".to_string(),
            "--abbrev-commit".to_string(),
        ]
    );
}

#[test]
fn non_alias_passthrough() {
    let repo = TestRepo::new();
    // No aliases configured — "commit" should pass through unchanged
    let resolved = resolve(&repo, &["commit", "-m", "msg"]).expect("expected passthrough");

    assert_eq!(resolved.command.as_deref(), Some("commit"));
    assert_eq!(
        resolved.command_args,
        vec!["-m".to_string(), "msg".to_string()]
    );
}

#[test]
fn global_args_preserved_after_alias_resolution() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.ci", "commit"]).unwrap();

    let resolved =
        resolve(&repo, &["-c", "user.name=Test", "ci", "-m", "msg"]).expect("expected resolution");

    assert_eq!(resolved.command.as_deref(), Some("commit"));
    assert!(
        resolved.global_args.contains(&"-c".to_string()),
        "global args should contain -c, got: {:?}",
        resolved.global_args
    );
    assert!(
        resolved.global_args.contains(&"user.name=Test".to_string()),
        "global args should contain user.name=Test, got: {:?}",
        resolved.global_args
    );
    assert_eq!(
        resolved.command_args,
        vec!["-m".to_string(), "msg".to_string()]
    );
}

#[test]
fn alias_to_non_hooked_command() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.s", "status"]).unwrap();

    let resolved = resolve(&repo, &["s", "--short"]).expect("expected resolution");

    assert_eq!(resolved.command.as_deref(), Some("status"));
    assert_eq!(resolved.command_args, vec!["--short".to_string()]);
}

#[test]
fn alias_with_no_extra_args() {
    let repo = TestRepo::new();
    repo.git(&["config", "alias.ci", "commit"]).unwrap();

    let resolved = resolve(&repo, &["ci"]).expect("expected resolution");

    assert_eq!(resolved.command.as_deref(), Some("commit"));
    assert!(resolved.command_args.is_empty());
}

#[test]
fn alias_with_double_quotes() {
    let repo = TestRepo::new();
    repo.git(&[
        "config",
        "alias.lg",
        r#"log "--format=%H %s" --abbrev-commit"#,
    ])
    .unwrap();

    let resolved = resolve(&repo, &["lg"]).expect("expected resolution");

    assert_eq!(resolved.command.as_deref(), Some("log"));
    assert_eq!(
        resolved.command_args,
        vec!["--format=%H %s".to_string(), "--abbrev-commit".to_string(),]
    );
}

// ─── E2E: aliased commit triggers hooks ──────────────────────────────────────

#[test]
fn aliased_commit_triggers_authorship_hooks() {
    let repo = TestRepo::new();

    // Configure alias: ci = commit
    repo.git(&["config", "alias.ci", "commit"]).unwrap();

    // Create a file with AI-authored content (set_contents handles checkpoints)
    let mut file = repo.filename("feature.rs");
    file.set_contents(lines![
        "fn hello() {".ai(),
        "  println!(\"hello\");".ai(),
        "}".ai(),
    ]);

    // Stage all changes
    repo.git(&["add", "-A"]).unwrap();

    // Commit using the alias — hooks should still fire
    repo.git(&["ci", "-m", "Add AI feature"])
        .expect("aliased commit should succeed");

    // Verify authorship was tracked by the commit hooks
    file.assert_lines_and_blame(lines![
        "fn hello() {".ai(),
        "  println!(\"hello\");".ai(),
        "}".ai(),
    ]);
}

#[test]
fn aliased_commit_with_extra_flags_triggers_authorship_hooks() {
    let repo = TestRepo::new();

    // Configure alias: ci = commit -v
    repo.git(&["config", "alias.ci", "commit -v"]).unwrap();

    // Create a file with mixed human and AI content
    let mut file = repo.filename("lib.rs");
    file.set_contents(lines![
        "// human-written module header".human(),
        "pub fn generated() -> i32 {".ai(),
        "    42".ai(),
        "}".ai(),
    ]);

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["ci", "-m", "Add generated function"])
        .expect("aliased commit with flags should succeed");

    // Verify mixed authorship was correctly tracked
    file.assert_lines_and_blame(lines![
        "// human-written module header".human(),
        "pub fn generated() -> i32 {".ai(),
        "    42".ai(),
        "}".ai(),
    ]);
}

// ─── E2E: aliased push triggers hooks ────────────────────────────────────────

#[test]
fn aliased_push_succeeds_with_hooks() {
    let (local, _upstream) = TestRepo::new_with_remote();

    // Configure alias: p = push
    local.git(&["config", "alias.p", "push"]).unwrap();

    // Create and commit a file with AI content
    let mut file = local.filename("module.py");
    file.set_contents(lines!["def ai_func():".ai(), "    return True".ai(),]);
    local
        .stage_all_and_commit("Add AI module")
        .expect("commit should succeed");

    // Push to set up tracking branch using alias — push hooks should fire
    local
        .git(&["p", "-u", "origin", "HEAD"])
        .expect("aliased push should succeed");

    // Verify authorship is intact after aliased push
    file.assert_lines_and_blame(lines!["def ai_func():".ai(), "    return True".ai(),]);
}

reuse_tests_in_worktree!(
    alias_with_args_resolves_command_for_hooks,
    alias_chain_resolves_to_final_command,
    alias_cycle_returns_none,
    alias_self_recursive_with_args_returns_none,
    shell_alias_returns_none,
    alias_parsing_respects_quotes,
    non_alias_passthrough,
    global_args_preserved_after_alias_resolution,
    alias_to_non_hooked_command,
    alias_with_no_extra_args,
    alias_with_double_quotes,
    aliased_commit_triggers_authorship_hooks,
    aliased_commit_with_extra_flags_triggers_authorship_hooks,
    aliased_push_succeeds_with_hooks,
);
