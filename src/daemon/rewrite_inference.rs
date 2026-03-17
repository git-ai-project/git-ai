use crate::daemon::domain::NormalizedCommand;
use crate::error::GitAiError;
use crate::git::rewrite_log::RewriteLogEvent;

pub(crate) fn fallback_commit_rewrite_event(cmd: &NormalizedCommand) -> Option<RewriteLogEvent> {
    if cmd.exit_code != 0 {
        return None;
    }
    let worktree = cmd.worktree.as_ref()?.to_string_lossy().to_string();
    let command = cmd
        .primary_command
        .as_deref()
        .or(cmd.invoked_command.as_deref())?;
    if command != "commit" {
        return None;
    }

    let new_head = run_git_capture(&worktree, &["rev-parse", "HEAD"])
        .ok()
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha))?;
    if cmd.invoked_args.iter().any(|arg| arg == "--amend") {
        let old_head = run_git_capture(&worktree, &["rev-parse", "HEAD@{1}"])
            .ok()
            .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha));
        if let Some(old_head) = old_head
            && old_head != new_head
        {
            return Some(RewriteLogEvent::commit_amend(old_head, new_head));
        }
        return None;
    }

    let base = cmd
        .pre_repo
        .as_ref()
        .and_then(|repo| repo.head.clone())
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        .or_else(|| {
            run_git_capture(&worktree, &["rev-parse", "HEAD@{1}"])
                .ok()
                .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        })
        .or_else(|| {
            run_git_capture(&worktree, &["rev-parse", "HEAD^"])
                .ok()
                .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        });

    // Root commits on fresh branches can lack both `HEAD@{1}` and `HEAD^`.
    // Preserve the rewrite event with `base_commit = None` so replay treats
    // the commit as based on `initial`.
    Some(RewriteLogEvent::commit(base, new_head))
}

fn run_git_capture(worktree: &str, args: &[&str]) -> Result<String, GitAiError> {
    let mut command = std::process::Command::new("/opt/homebrew/bin/git");
    command.arg("-C").arg(worktree);
    command.args(args);
    let output = command.output()?;
    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git {:?} failed in {}: {}",
            args,
            worktree,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{
        CommandScope, Confidence, FamilyKey, NormalizedCommand, RepoContext,
    };
    use std::path::Path;
    use std::process::Command;

    fn run_git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("/opt/homebrew/bin/git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn fallback_prefers_primary_commit_over_invoked_alias() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path();

        run_git(repo, &["init"]);
        run_git(repo, &["config", "user.name", "Test User"]);
        run_git(repo, &["config", "user.email", "test@example.com"]);

        std::fs::write(repo.join("file.txt"), "base\n").expect("write base");
        run_git(repo, &["add", "file.txt"]);
        run_git(repo, &["commit", "-m", "base"]);
        let base = run_git(repo, &["rev-parse", "HEAD"]);

        std::fs::write(repo.join("file.txt"), "base\nnext\n").expect("write next");
        run_git(repo, &["add", "file.txt"]);
        run_git(repo, &["commit", "-m", "next"]);
        let head = run_git(repo, &["rev-parse", "HEAD"]);

        let cmd = NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new("family:/repo")),
            family_key: Some(FamilyKey::new("family:/repo")),
            worktree: Some(repo.to_path_buf()),
            root_sid: "sid".to_string(),
            raw_argv: vec![
                "git".to_string(),
                "ci".to_string(),
                "-m".to_string(),
                "next".to_string(),
            ],
            primary_command: Some("commit".to_string()),
            invoked_command: Some("ci".to_string()),
            invoked_args: vec!["-m".to_string(), "next".to_string()],
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: Some(RepoContext {
                head: Some(base.clone()),
                branch: Some("main".to_string()),
                detached: false,
                cherry_pick_head: None,
            }),
            post_repo: Some(RepoContext {
                head: Some(head.clone()),
                branch: Some("main".to_string()),
                detached: false,
                cherry_pick_head: None,
            }),
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        };

        let event = fallback_commit_rewrite_event(&cmd).expect("fallback commit event");
        match event {
            RewriteLogEvent::Commit { commit } => {
                assert_eq!(commit.commit_sha, head);
                assert_eq!(commit.base_commit, Some(base));
            }
            other => panic!("expected commit event, got {:?}", other),
        }
    }
}
