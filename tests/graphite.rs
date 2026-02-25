#[macro_use]
mod repos;

use repos::graphite_test_harness::{
    assert_blame_is_unchanged, assert_exact_blame, capture_blame, init_graphite, skip_unless_gt,
};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::collections::BTreeSet;
use std::process::Command;

const REQUIRED_NATIVE_MUTATING_COMMANDS: &[&str] = &[
    "create",
    "modify",
    "commit create",
    "commit amend",
    "restack",
    "stack restack",
    "upstack restack",
    "downstack restack",
    "move",
    "upstack onto",
    "fold",
    "split --by-file",
    "squash",
    "delete",
    "pop",
    "rename",
    "undo",
    "absorb",
    "revert",
    "reorder",
];

const REQUIRED_PASSTHROUGH_MUTATING_COMMANDS: &[&str] = &[
    "add",
    "rebase",
    "reset",
    "cherry-pick",
    "stash",
    "checkout",
    "switch",
    "fetch",
    "pull",
    "push",
    "restore",
    "rm",
    "mv",
    "clean",
    "clone",
    "am",
    "apply",
];

const OPTIONAL_AUTH_MUTATING_COMMANDS: &[&str] = &[
    "submit",
    "stack submit",
    "upstack submit",
    "downstack submit",
    "sync",
    "get",
    "merge",
    "pr",
    "unlink",
];

fn maybe_skip_for_gt() -> bool {
    skip_unless_gt()
}

fn assert_command_lists_have_no_duplicates() {
    let mut entries = BTreeSet::new();
    for command in REQUIRED_NATIVE_MUTATING_COMMANDS
        .iter()
        .chain(REQUIRED_PASSTHROUGH_MUTATING_COMMANDS.iter())
        .chain(OPTIONAL_AUTH_MUTATING_COMMANDS.iter())
    {
        assert!(
            entries.insert(*command),
            "duplicate command found in classification list: {}",
            command
        );
    }
}

fn gt_help_all() -> String {
    let output = Command::new("gt")
        .args(["--help", "--all"])
        .output()
        .expect("gt --help --all should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{}{}", stdout, stderr)
}

#[test]
fn test_gt_command_index_is_classified() {
    if maybe_skip_for_gt() {
        return;
    }

    assert_command_lists_have_no_duplicates();

    let help = gt_help_all();
    for command in REQUIRED_NATIVE_MUTATING_COMMANDS {
        let first = command
            .split_whitespace()
            .next()
            .expect("command list cannot include empty entries");
        let help_output = Command::new("gt")
            .args([first, "--help"])
            .output()
            .expect("gt <command> --help should run");
        assert!(
            help.contains(&format!("gt {}", first))
                || String::from_utf8_lossy(&help_output.stdout).contains(&format!("gt {}", first))
                || String::from_utf8_lossy(&help_output.stderr).contains(&format!("gt {}", first)),
            "expected gt help output to include command: {} (or nested help to expose it)",
            first,
        );
    }
}

#[test]
fn test_gt_create_and_modify_preserve_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("app.txt");
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("base commit").unwrap();
    init_graphite(&repo);

    file.insert_at(1, lines!["ai line 1".ai(), "human line".human()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "feature/create", "-m", "create feature"])
        .expect("gt create should succeed");

    assert_exact_blame(
        &repo,
        "app.txt",
        lines!["base".human(), "ai line 1".ai(), "human line".human()],
    );

    file.replace_at(2, "human line v2".human());
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["modify", "-m", "modify feature"])
        .expect("gt modify should succeed");

    assert_exact_blame(
        &repo,
        "app.txt",
        lines!["base".human(), "ai line 1".ai(), "human line v2".human()],
    );
}

#[test]
fn test_gt_commit_create_and_amend_preserve_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("commit.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(1, lines!["ai create".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "feature/commit", "-m", "feature commit"])
        .expect("gt create should succeed");

    file.insert_at(2, lines!["ai commit create".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["commit", "create", "-m", "new commit"])
        .expect("gt commit create should succeed");

    file.replace_at(2, "human amend".human());
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["commit", "amend", "-m", "amend commit"])
        .expect("gt commit amend should succeed");

    assert_exact_blame(
        &repo,
        "commit.txt",
        lines!["base".human(), "ai create".ai(), "human amend".human()],
    );
}

#[test]
fn test_gt_restack_variants_keep_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("restack.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(1, lines!["ai a".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "a", "-m", "a"]).unwrap();

    file.insert_at(2, lines!["ai b".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "b", "-m", "b"]).unwrap();

    let before = capture_blame(&repo, "restack.txt");

    repo.gt(&["restack"]).expect("gt restack should succeed");
    repo.gt(&["stack", "restack"])
        .expect("gt stack restack should succeed");
    repo.gt(&["upstack", "restack"])
        .expect("gt upstack restack should succeed");
    repo.gt(&["downstack", "restack"])
        .expect("gt downstack restack should succeed");

    assert_blame_is_unchanged(&repo, "restack.txt", &before);
}

#[test]
fn test_gt_move_and_upstack_onto_keep_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut root = repo.filename("root.txt");
    root.set_contents(lines!["root"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    let mut a_file = repo.filename("a.txt");
    a_file.set_contents(lines!["a-root", "ai a".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "a", "-m", "a"]).unwrap();

    let mut b_file = repo.filename("b.txt");
    b_file.set_contents(lines!["b-root", "ai b".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "b", "-m", "b"]).unwrap();

    repo.gt(&["move", "--onto", "main"])
        .expect("gt move --onto should succeed");
    assert_exact_blame(&repo, "b.txt", lines!["b-root".human(), "ai b".ai()]);

    repo.gt(&["checkout", "a"]).unwrap();
    assert_exact_blame(&repo, "a.txt", lines!["a-root".human(), "ai a".ai()]);
    repo.gt(&["upstack", "onto", "main"])
        .expect("gt upstack onto should succeed");
    assert_exact_blame(&repo, "a.txt", lines!["a-root".human(), "ai a".ai()]);
}

#[test]
fn test_gt_fold_preserves_surviving_line_ownership() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("fold.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(1, lines!["ai-parent".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "parent", "-m", "parent"]).unwrap();

    file.insert_at(2, lines!["human-child".human()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "child", "-m", "child"]).unwrap();

    repo.gt(&["fold"]).expect("gt fold should succeed");
    assert_exact_blame(
        &repo,
        "fold.txt",
        lines!["base".human(), "ai-parent".ai(), "human-child".human()],
    );
}

#[test]
fn test_gt_squash_preserves_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("squash.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(1, lines!["ai-1".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "sq", "-m", "sq"]).unwrap();

    file.insert_at(2, lines!["human-2".human()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["commit", "create", "-m", "second"]).unwrap();

    repo.gt(&["squash", "-m", "squashed"])
        .expect("gt squash should succeed");
    assert_exact_blame(
        &repo,
        "squash.txt",
        lines!["base".human(), "ai-1".ai(), "human-2".human()],
    );
}

#[test]
fn test_gt_split_by_file_preserves_exact_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut app = repo.filename("app.txt");
    let mut lib = repo.filename("lib.txt");
    app.set_contents(lines!["app-base", "app-ai".ai()]);
    lib.set_contents(lines!["lib-base", "lib-ai".ai()]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    app.insert_at(2, lines!["app-ai-2".ai()]);
    lib.insert_at(2, lines!["lib-human-2".human()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "split-target", "-m", "split target"])
        .unwrap();

    repo.gt(&["split", "--by-file", "lib.txt"])
        .expect("gt split --by-file should succeed");

    assert_exact_blame(
        &repo,
        "app.txt",
        lines!["app-base".human(), "app-ai".ai(), "app-ai-2".ai()],
    );
}

#[test]
fn test_gt_delete_pop_rename_undo_do_not_regress_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("meta.txt");
    file.set_contents(lines!["base", "ai".ai()]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(2, lines!["next".human()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "a", "-m", "a"]).unwrap();
    let before_meta = capture_blame(&repo, "meta.txt");

    repo.gt(&["rename", "renamed"])
        .expect("gt rename should succeed");
    assert_blame_is_unchanged(&repo, "meta.txt", &before_meta);

    repo.gt(&["undo", "--force"])
        .expect("gt undo --force should succeed");
    assert_blame_is_unchanged(&repo, "meta.txt", &before_meta);

    repo.gt(&["create", "child", "-m", "child"]).unwrap();
    repo.gt(&["delete", "child", "--force"])
        .expect("gt delete should succeed");
    assert_blame_is_unchanged(&repo, "meta.txt", &before_meta);

    repo.gt(&["pop"]).expect("gt pop should succeed");
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("commit after pop").unwrap();
    assert_blame_is_unchanged(&repo, "meta.txt", &before_meta);
}

#[test]
fn test_gt_absorb_revert_and_reorder_keep_correct_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("ops.txt");
    file.set_contents(lines!["base", "first".ai()]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    file.insert_at(2, lines!["second".ai()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["create", "absorb", "-m", "absorb"]).unwrap();

    file.replace_at(1, "first-updated".human());
    repo.git(&["add", "-A"]).unwrap();
    repo.gt(&["absorb", "--force"])
        .expect("gt absorb should succeed");

    assert_exact_blame(
        &repo,
        "ops.txt",
        lines!["base".human(), "first-updated".human(), "second".ai()],
    );

    repo.gt(&["reorder"]).expect("gt reorder should succeed");
    assert_exact_blame(
        &repo,
        "ops.txt",
        lines!["base".human(), "first-updated".human(), "second".ai()],
    );

    let head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    repo.gt(&["checkout", "main"]).unwrap();
    if let Err(err) = repo.gt(&["revert", "--sha", &head]) {
        assert!(
            err.contains("nothing to commit")
                || err.contains("working tree clean")
                || err.contains("detached"),
            "gt revert should either succeed or be a clean no-op, got: {}",
            err
        );
    }
}

#[test]
fn test_gt_passthrough_rebase_reset_and_cherry_pick() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("pass.txt");
    file.set_contents(lines!["base"]);
    let base_commit = repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["ai-feature".ai()]);
    repo.stage_all_and_commit("feature work").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.gt(&["checkout", "main"]).unwrap();
    repo.gt(&["cherry-pick", &feature_commit])
        .expect("gt cherry-pick should succeed");
    assert_exact_blame(&repo, "pass.txt", lines!["base".human(), "ai-feature".ai()]);

    repo.gt(&["reset", "--soft", &base_commit.commit_sha])
        .expect("gt reset --soft should succeed");
    repo.commit("recommit after reset").unwrap();
    assert_exact_blame(&repo, "pass.txt", lines!["base".human(), "ai-feature".ai()]);

    repo.gt(&["checkout", "feature"]).unwrap();
    repo.gt(&["rebase", "main"])
        .expect("gt rebase should succeed");
    assert_exact_blame(&repo, "pass.txt", lines!["base".human(), "ai-feature".ai()]);
}

#[test]
fn test_gt_passthrough_stash_checkout_switch_restore() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("stash.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["ai-stash".ai()]);
    repo.gt(&["stash", "push", "-m", "stash"])
        .expect("gt stash push should succeed");
    repo.gt(&["stash", "pop"])
        .expect("gt stash pop should succeed");
    repo.stage_all_and_commit("apply stash").unwrap();
    assert_exact_blame(&repo, "stash.txt", lines!["base".human(), "ai-stash".ai()]);

    file.replace_at(1, "tmp".human());
    repo.gt(&["restore", "stash.txt"])
        .expect("gt restore should succeed");
    assert_exact_blame(&repo, "stash.txt", lines!["base".human(), "ai-stash".ai()]);

    repo.gt(&["switch", "main"])
        .expect("gt switch should succeed");
    repo.gt(&["checkout", "feature"])
        .expect("gt checkout should succeed");
}

#[test]
fn test_gt_passthrough_add_mv_rm_clean() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut seed = repo.filename("seed.txt");
    seed.set_contents(lines!["seed"]);
    repo.stage_all_and_commit("seed").unwrap();
    init_graphite(&repo);

    let mut file = repo.filename("old.txt");
    file.set_contents(lines!["one".ai(), "two".ai()]);
    repo.gt(&["add", "old.txt"]).expect("gt add should succeed");
    repo.commit("add file").unwrap();
    assert_exact_blame(&repo, "old.txt", lines!["one".ai(), "two".ai()]);

    repo.gt(&["mv", "old.txt", "new.txt"])
        .expect("gt mv should succeed");
    repo.commit("rename").unwrap();
    assert_exact_blame(&repo, "new.txt", lines!["one".ai(), "two".ai()]);

    std::fs::write(repo.path().join("trash.tmp"), "tmp").unwrap();
    repo.gt(&["clean", "-fd"]).expect("gt clean should succeed");

    repo.gt(&["rm", "new.txt"]).expect("gt rm should succeed");
    repo.commit("remove file").unwrap();
    assert!(
        repo.read_file("new.txt").is_none(),
        "gt rm should delete the tracked file"
    );
}

#[test]
fn test_gt_passthrough_fetch_pull_push_clone_am_apply() {
    if maybe_skip_for_gt() {
        return;
    }

    let (local, upstream) = TestRepo::new_with_remote();
    let mut seed = local.filename("seed.txt");
    seed.set_contents(lines!["seed"]);
    local.stage_all_and_commit("seed").unwrap();
    init_graphite(&local);

    let mut local_file = local.filename("remote.txt");
    local_file.set_contents(lines!["local-ai".ai()]);
    local.stage_all_and_commit("local commit").unwrap();
    local
        .gt(&["push", "--set-upstream", "origin", "main"])
        .expect("gt push should succeed");

    local
        .gt(&["fetch", "origin"])
        .expect("gt fetch should succeed");
    local
        .gt(&["pull", "--rebase"])
        .expect("gt pull should succeed");

    let clone_parent = std::env::temp_dir().join("gt-clone-test");
    let _ = std::fs::remove_dir_all(&clone_parent);
    std::fs::create_dir_all(&clone_parent).unwrap();
    let clone_target = clone_parent.join("mirror");

    local
        .gt(&[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_target.to_str().unwrap(),
        ])
        .expect("gt clone should succeed");

    let apply_patch_path = local.path().join("change.patch");
    std::fs::write(
        local.path().join("remote.txt"),
        "local-ai\nline-from-apply\n",
    )
    .unwrap();
    let patch = local
        .git(&["diff", "--", "remote.txt"])
        .expect("git diff for patch generation should succeed");
    std::fs::write(&apply_patch_path, patch).unwrap();
    local
        .git(&["checkout", "--", "remote.txt"])
        .expect("git checkout -- remote.txt should succeed");
    local
        .gt(&["apply", apply_patch_path.to_str().unwrap()])
        .expect("gt apply should succeed");
    local.gt(&["add", "remote.txt"]).unwrap();
    local.commit("apply patch").unwrap();
    assert_exact_blame(
        &local,
        "remote.txt",
        lines!["local-ai".ai(), "line-from-apply".human()],
    );

    // Validate `gt am` passthrough with a simple one-commit patch series.
    local.git(&["checkout", "-b", "am-source"]).unwrap();
    let mut am_file = local.filename("am.txt");
    am_file.set_contents(lines!["from-am".ai()]);
    local.stage_all_and_commit("am source commit").unwrap();
    let patch_dir = local.path().join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    local
        .git(&[
            "format-patch",
            "-1",
            "HEAD",
            "-o",
            patch_dir.to_str().unwrap(),
        ])
        .unwrap();
    local.git(&["checkout", "main"]).unwrap();
    let patch_file = std::fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().map(|ext| ext == "patch").unwrap_or(false))
        .expect("format-patch should produce one .patch file");
    local
        .gt(&["am", patch_file.to_str().unwrap()])
        .expect("gt am should succeed");
}

#[test]
fn test_gt_track_untrack_freeze_unfreeze_do_not_change_blame() {
    if maybe_skip_for_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("state.txt");
    file.set_contents(lines!["base", "ai".ai()]);
    repo.stage_all_and_commit("base").unwrap();
    init_graphite(&repo);

    repo.gt(&["create", "tracked", "-m", "tracked"]).unwrap();
    let before = capture_blame(&repo, "state.txt");

    repo.gt(&["track"]).expect("gt track should succeed");
    repo.gt(&["freeze"]).expect("gt freeze should succeed");
    repo.gt(&["unfreeze"]).expect("gt unfreeze should succeed");
    repo.gt(&["untrack", "--force"])
        .expect("gt untrack should succeed");

    assert_blame_is_unchanged(&repo, "state.txt", &before);
}
