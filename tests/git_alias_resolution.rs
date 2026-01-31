#[macro_use]
mod repos;

use git_ai::commands::git_handlers::resolve_alias_invocation_for_tests;
use git_ai::git::cli_parser::{parse_git_cli_args, ParsedGitInvocation};
use git_ai::git::find_repository_in_path;
use repos::test_repo::TestRepo;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn resolve(repo: &TestRepo, argv: &[&str]) -> Option<ParsedGitInvocation> {
    let parsed = parse_git_cli_args(&args(argv));
    let git_repo =
        find_repository_in_path(repo.path().to_str().unwrap()).expect("expected to find git repo");
    resolve_alias_invocation_for_tests(&parsed, &git_repo)
}

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
