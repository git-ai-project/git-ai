//! Unit tests for `summarize_rebase_args` — the rebase CLI arg classifier used
//! by the daemon's rebase detection. Pure parsing, no git repo required.
//!
//! Migrated from the removed `rebase_hooks_unit.rs` (whose
//! `build_rebase_commit_mappings` half tested the deleted hooks module). These
//! tests cover `summarize_rebase_args`, which is still live in
//! `src/git/cli_parser.rs`.

use git_ai::git::cli_parser::summarize_rebase_args;

/// Build a `command_args` slice as `summarize_rebase_args` expects (args after
/// the "rebase" command word).
fn args(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_summarize_rebase_args_continue_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--continue"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_abort_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--abort"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_skip_is_control_mode() {
    let summary = summarize_rebase_args(&args(&["--skip"]));
    assert!(summary.is_control_mode);
}

#[test]
fn test_summarize_rebase_args_upstream_only() {
    let summary = summarize_rebase_args(&args(&["origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_upstream_and_branch() {
    let summary = summarize_rebase_args(&args(&["origin/main", "feature"]));
    assert!(!summary.is_control_mode);
    assert_eq!(
        summary.positionals,
        vec!["origin/main".to_string(), "feature".to_string()]
    );
}

#[test]
fn test_summarize_rebase_args_onto_flag() {
    let summary = summarize_rebase_args(&args(&["--onto", "abc123", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.onto_spec, Some("abc123".to_string()));
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_onto_equals_flag() {
    let summary = summarize_rebase_args(&args(&["--onto=abc123", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.onto_spec, Some("abc123".to_string()));
}

#[test]
fn test_summarize_rebase_args_root_flag() {
    let summary = summarize_rebase_args(&args(&["--root"]));
    assert!(!summary.is_control_mode);
    assert!(summary.has_root);
}

#[test]
fn test_summarize_rebase_args_interactive_with_upstream() {
    let summary = summarize_rebase_args(&args(&["-i", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}

#[test]
fn test_summarize_rebase_args_strategy_consumes_value() {
    let summary = summarize_rebase_args(&args(&["-s", "ours", "origin/main"]));
    assert!(!summary.is_control_mode);
    assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
}
