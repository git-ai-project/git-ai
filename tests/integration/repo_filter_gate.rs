//! `allow_repositories` / `exclude_repositories` must keep git-ai from writing
//! authorship notes for a filtered repository and from pushing notes to a
//! remote the filters reject.
//!
//! Remotes use real-looking URLs (the filters match on remote URLs) whose
//! transport is rewritten to local bare repos with `url.<path>.insteadOf`.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{DaemonTestScope, TestRepo};
use crate::test_utils::{codex_checkpoint, isolated_metrics_db_path};
use git_ai::authorship::authorship_log_serialization::generate_session_id;
use git_ai::metrics::db::MetricsDatabase;
use git_ai::metrics::types::MetricEventId;
use git_ai::metrics::{EventAttributes, MetricEvent, PosEncoded, SessionEventValues};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

const GITHUB_URL: &str = "https://github.com/acme/filtered.git";
const GITLAB_URL: &str = "https://gitlab.example.com/acme/filtered.git";

/// Merge repository filters into the git-ai config the CLI and the daemon read.
fn set_repository_filters(repo: &TestRepo, allow: &[&str], exclude: &[&str]) {
    let mut homes = vec![repo.test_home_path().clone(), repo.daemon_home_path()];
    homes.dedup();
    for home in homes {
        let config_path = home.join(".git-ai/config.json");
        let mut config: serde_json::Map<String, serde_json::Value> =
            fs::read_to_string(&config_path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        config.insert("allow_repositories".to_string(), json!(allow));
        config.insert("exclude_repositories".to_string(), json!(exclude));
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
    }
}

/// Point remote `name` at `url` while sending its transport to `bare`.
fn set_remote(repo: &TestRepo, name: &str, url: &str, bare: &Path) {
    if repo.git_og(&["remote", "get-url", name]).is_ok() {
        repo.git_og(&["remote", "set-url", name, url]).unwrap();
    } else {
        repo.git_og(&["remote", "add", name, url]).unwrap();
    }
    repo.git_og(&[
        "config",
        &format!("url.{}.insteadOf", bare.to_str().unwrap()),
        url,
    ])
    .unwrap();
}

fn notes_refs(repo: &TestRepo) -> String {
    repo.git_og(&["for-each-ref", "--format=%(refname)", "refs/notes/"])
        .unwrap()
        .trim()
        .to_string()
}

fn head_sha(repo: &TestRepo) -> String {
    repo.git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string()
}

fn commit_all(repo: &TestRepo, message: &str) -> String {
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", message]).unwrap();
    head_sha(repo)
}

fn filtered_repo_with_remote() -> (TestRepo, TestRepo) {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    set_repository_filters(&local, &[], &["https://github.com/*"]);
    (local, upstream)
}

#[test]
fn excluded_repository_commit_writes_no_authorship_note() {
    let (local, _upstream) = filtered_repo_with_remote();

    fs::write(local.path().join("human.txt"), "written by a human\n").unwrap();
    let sha = commit_all(&local, "human change in excluded repo");

    assert_eq!(
        local.read_authorship_note(&sha),
        None,
        "no authorship note may be written in a repository matched by exclude_repositories"
    );
}

#[test]
fn excluded_repository_checkpoint_is_skipped_and_commit_writes_no_note() {
    let (local, _upstream) = filtered_repo_with_remote();

    let mut file = local.filename("ai.txt");
    file.set_contents(vec!["line from an agent".ai()]);
    let sha = commit_all(&local, "agent change in excluded repo");

    assert_eq!(
        local.read_authorship_note(&sha),
        None,
        "the checkpoint was skipped by the filter; post-commit must not write a note anyway"
    );
}

#[test]
fn excluded_repository_amend_writes_no_authorship_note() {
    let (local, _upstream) = filtered_repo_with_remote();

    fs::write(local.path().join("human.txt"), "written by a human\n").unwrap();
    commit_all(&local, "human change in excluded repo");
    fs::write(local.path().join("human.txt"), "amended by a human\n").unwrap();
    local.git(&["add", "-A"]).unwrap();
    local.git(&["commit", "--amend", "--no-edit"]).unwrap();
    let amended = head_sha(&local);

    assert_eq!(
        local.read_authorship_note(&amended),
        None,
        "amending a commit in an excluded repository must not write a note"
    );
}

#[test]
fn repository_excluded_after_checkpoints_drops_its_working_log_on_commit() {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    fs::write(local.path().join("README.md"), "base\n").unwrap();
    let base = commit_all(&local, "base");

    // Checkpoints recorded while the repository is still tracked.
    local
        .filename("committed.txt")
        .set_contents(vec!["committed agent line".ai()]);
    local
        .filename("uncommitted.txt")
        .set_contents(vec!["uncommitted agent line".ai()]);
    assert!(
        !local
            .working_logs_for_base_commit(&base)
            .read_all_checkpoints()
            .unwrap()
            .is_empty(),
        "precondition: AI checkpoints were recorded before the repository was excluded"
    );

    set_repository_filters(&local, &[], &["https://github.com/*"]);
    local.git(&["add", "committed.txt"]).unwrap();
    local
        .git(&["commit", "-m", "commit after exclusion"])
        .unwrap();
    let sha = head_sha(&local);

    assert_eq!(
        local.read_authorship_note(&sha),
        None,
        "checkpoints recorded before the exclusion must not produce a note"
    );
    assert!(
        local
            .working_logs_for_base_commit(&base)
            .read_all_checkpoints()
            .unwrap()
            .is_empty(),
        "the old base's working log must be dropped"
    );
    let carried = local.working_logs_for_base_commit(&sha);
    assert!(
        carried.read_all_checkpoints().unwrap().is_empty()
            && carried.read_initial_attributions().files.is_empty(),
        "uncommitted AI attributions must not be carried to the new commit"
    );
}

#[test]
fn excluded_repository_push_sends_no_notes_to_remote() {
    let (local, upstream) = filtered_repo_with_remote();

    fs::write(local.path().join("human.txt"), "written by a human\n").unwrap();
    commit_all(&local, "change in excluded repo");
    local.git(&["push", "origin", "HEAD:main"]).unwrap();
    local.sync_daemon_force();

    assert_eq!(
        notes_refs(&upstream),
        "",
        "push to an excluded remote must not sync refs/notes/ai"
    );
}

/// Filters are evaluated per repository (any matching remote allows it), but a
/// push targets one remote. A repo allowed via its GitLab origin must still
/// not push notes to a GitHub remote that the allowlist does not cover.
#[test]
fn push_to_remote_outside_allowlist_sends_no_notes_even_if_repo_is_allowed() {
    let (local, github) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    let gitlab = TestRepo::new_bare_with_daemon_scope(DaemonTestScope::NoDaemon);
    set_remote(&local, "origin", GITLAB_URL, gitlab.path());
    set_remote(&local, "upstream", GITHUB_URL, github.path());
    set_repository_filters(&local, &["https://gitlab.example.com/*"], &[]);

    let mut file = local.filename("ai.txt");
    file.set_contents(vec!["line from an agent".ai()]);
    let sha = commit_all(&local, "agent change in allowed repo");
    assert!(
        local.read_authorship_note(&sha).is_some(),
        "precondition: the repo is allowed via origin, so a note is expected locally"
    );

    local.git(&["push", "upstream", "HEAD:main"]).unwrap();
    local.sync_daemon_force();

    assert_eq!(
        notes_refs(&github),
        "",
        "notes must not be pushed to a remote outside allow_repositories"
    );
}

/// Positive control: with filters configured, a push to an allowed remote
/// still syncs notes.
#[test]
fn push_to_allowed_remote_syncs_notes_when_filters_are_set() {
    let (local, gitlab) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITLAB_URL, gitlab.path());
    set_repository_filters(
        &local,
        &["https://gitlab.example.com/*"],
        &["https://github.com/*"],
    );

    let mut file = local.filename("ai.txt");
    file.set_contents(vec!["line from an agent".ai()]);
    let sha = commit_all(&local, "agent change in allowed repo");
    local.git(&["push", "origin", "HEAD:main"]).unwrap();

    assert!(
        local
            .read_authorship_note_in_git_dir(gitlab.path(), &sha)
            .is_some(),
        "notes must still be pushed to a remote the filters allow"
    );
}

/// `git push <url>` targets a URL, not a configured remote name; the URL itself
/// must pass the filters.
#[test]
fn push_to_literal_excluded_url_sends_no_notes() {
    let (local, gitlab) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    let github = TestRepo::new_bare_with_daemon_scope(DaemonTestScope::NoDaemon);
    set_remote(&local, "origin", GITLAB_URL, gitlab.path());
    local
        .git_og(&[
            "config",
            &format!("url.{}.insteadOf", github.path().to_str().unwrap()),
            GITHUB_URL,
        ])
        .unwrap();
    set_repository_filters(&local, &[], &["https://github.com/*"]);

    let mut file = local.filename("ai.txt");
    file.set_contents(vec!["line from an agent".ai()]);
    commit_all(&local, "agent change");
    local.git(&["push", GITHUB_URL, "HEAD:main"]).unwrap();
    local.sync_daemon_force();

    assert_eq!(
        notes_refs(&github),
        "",
        "notes must not be pushed to an excluded URL given on the command line"
    );
}

/// git pushes to `pushurl` when one is set, so the filters must check it, not
/// only `url`.
#[test]
fn push_via_pushurl_outside_allowlist_sends_no_notes() {
    let (local, gitlab) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    let github = TestRepo::new_bare_with_daemon_scope(DaemonTestScope::NoDaemon);
    set_remote(&local, "origin", GITLAB_URL, gitlab.path());
    local
        .git_og(&["config", "remote.origin.pushurl", GITHUB_URL])
        .unwrap();
    local
        .git_og(&[
            "config",
            &format!("url.{}.insteadOf", github.path().to_str().unwrap()),
            GITHUB_URL,
        ])
        .unwrap();
    set_repository_filters(&local, &["https://gitlab.example.com/*"], &[]);

    let mut file = local.filename("ai.txt");
    file.set_contents(vec!["line from an agent".ai()]);
    commit_all(&local, "agent change");
    local.git(&["push", "origin", "HEAD:main"]).unwrap();
    local.sync_daemon_force();

    assert!(
        github
            .git_og(&["rev-parse", "--verify", "refs/heads/main"])
            .is_ok(),
        "precondition: the branch push went to the pushurl"
    );
    assert_eq!(
        notes_refs(&github),
        "",
        "notes must not be pushed to a pushurl outside allow_repositories"
    );
}

fn file_mtime_secs(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .min(u32::MAX as u64) as u32
}

/// Transcript streaming keeps recording SessionEvents for excluded repos (they
/// are only dropped at upload). Post-commit attribution recovery must not turn
/// them into an AI-attributed note for the excluded repo.
#[test]
fn excluded_repository_commit_does_not_recover_ai_session_into_note() {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let local =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    local
        .git_og(&["remote", "add", "origin", GITHUB_URL])
        .unwrap();
    set_repository_filters(&local, &[], &["https://github.com/*"]);
    local
        .git(&["commit", "--allow-empty", "-m", "initial"])
        .unwrap();

    let file_path = local.path().join("generated.txt");
    fs::write(&file_path, "generated by an agent\n").unwrap();
    let external_session_id = "cursor-session-in-excluded-repo";
    let session_id = generate_session_id(external_session_id, "cursor");
    let event = MetricEvent::from_values_with_timestamp(
        SessionEventValues::with_ids(
            json!({ "type": "assistant", "session_id": external_session_id }),
            Some("event-excluded".to_string()),
            None,
            Some("tool-use-excluded".to_string()),
        ),
        EventAttributes::with_version("test")
            .tool("cursor")
            .model("claude-sonnet-5")
            .external_session_id(external_session_id)
            .session_id(&session_id)
            .trace_id("trace-excluded")
            .repo_url("https://github.com/acme/filtered")
            .to_sparse(),
        Some(file_mtime_secs(&file_path)),
    );
    MetricsDatabase::open_at_path(Path::new(&metrics_db_path))
        .unwrap()
        .insert_events(&[serde_json::to_string(&event).unwrap()])
        .unwrap();

    let sha = commit_all(&local, "uncheckpointed agent change in excluded repo");

    let note = local.read_authorship_note(&sha);
    assert_eq!(
        note,
        None,
        "no note may be written for an excluded repo; recovered session {session_id} present: {}",
        note.as_deref().is_some_and(|n| n.contains(&session_id))
    );
}

/// A tracked repository with an AI-authored commit on `feature` (which has a
/// note) and one extra commit on the default branch; the repository is then
/// excluded. Returns (repo, default branch, feature commit).
fn repo_excluded_after_noted_feature_commit() -> (TestRepo, TestRepo, String, String) {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    fs::write(local.path().join("README.md"), "base\n").unwrap();
    commit_all(&local, "base");
    let main_branch = local.current_branch();

    local.git(&["checkout", "-b", "feature"]).unwrap();
    local
        .filename("feature.txt")
        .set_contents(vec!["agent feature line".ai()]);
    let feature = commit_all(&local, "agent feature");
    assert!(
        local.read_authorship_note(&feature).is_some(),
        "precondition: the feature commit got a note while the repository was tracked"
    );

    local.git(&["checkout", &main_branch]).unwrap();
    fs::write(local.path().join("main.txt"), "main moves on\n").unwrap();
    commit_all(&local, "main moves on");

    set_repository_filters(&local, &[], &["https://github.com/*"]);
    (local, upstream, main_branch, feature)
}

#[test]
fn excluded_repository_cherry_pick_copies_no_note() {
    let (local, _upstream, _main, feature) = repo_excluded_after_noted_feature_commit();

    local.git(&["cherry-pick", &feature]).unwrap();
    let picked = head_sha(&local);

    assert_ne!(picked, feature);
    assert_eq!(
        local.read_authorship_note(&picked),
        None,
        "cherry-pick in an excluded repository must not copy the source note"
    );
}

#[test]
fn excluded_repository_rebase_copies_no_note() {
    let (local, _upstream, main_branch, feature) = repo_excluded_after_noted_feature_commit();

    local.git(&["checkout", "feature"]).unwrap();
    local.git(&["rebase", &main_branch]).unwrap();
    let rebased = head_sha(&local);

    assert_ne!(rebased, feature);
    assert_eq!(
        local.read_authorship_note(&rebased),
        None,
        "rebase in an excluded repository must not carry the note to the rewritten commit"
    );
}

/// Give `bare` a commit carrying a `refs/notes/ai` note, written with raw git
/// (`git_og`) so the seed does not depend on git-ai. Returns the noted commit.
fn seed_remote_with_note(bare: &TestRepo) -> String {
    let seed = TestRepo::new_with_daemon_scope(DaemonTestScope::NoDaemon);
    fs::write(seed.path().join("seed.txt"), "seed\n").unwrap();
    seed.git_og(&["add", "-A"]).unwrap();
    seed.git_og(&["commit", "-q", "-m", "seed"]).unwrap();
    seed.git_og(&["notes", "--ref=ai", "add", "-m", "seeded note", "HEAD"])
        .unwrap();
    seed.git_og(&[
        "push",
        "-q",
        bare.path().to_str().unwrap(),
        "HEAD:refs/heads/main",
        "refs/notes/ai:refs/notes/ai",
    ])
    .unwrap();
    assert!(!notes_refs(bare).is_empty());
    head_sha(&seed)
}

/// Pull from origin = GITHUB_URL (transport: `upstream`) after seeding a note
/// there. Returns the notes refs the local repo ends up with.
fn notes_refs_after_pull(exclude: &[&str]) -> String {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    seed_remote_with_note(&upstream);
    set_repository_filters(&local, &[], exclude);

    local.git(&["pull", "origin", "main"]).unwrap();
    local.sync_daemon_force();
    notes_refs(&local)
}

#[test]
fn pull_into_allowed_repository_fetches_notes() {
    assert_ne!(
        notes_refs_after_pull(&["https://gitlab.example.com/*"]),
        "",
        "positive control: pull in a tracked repository fetches refs/notes/ai"
    );
}

#[test]
fn excluded_repository_pull_fetches_no_notes() {
    assert_eq!(
        notes_refs_after_pull(&["https://github.com/*"]),
        "",
        "pull in an excluded repository must not fetch refs/notes/ai"
    );
}

/// Clone GITHUB_URL (transport: a seeded bare repo; the `insteadOf` is set in
/// the new clone with `--config`, so later fetches resolve it too). Returns the
/// notes refs of the clone.
fn notes_refs_after_clone(exclude: &[&str]) -> String {
    let (local, _upstream) =
        TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    let bare = TestRepo::new_bare_with_daemon_scope(DaemonTestScope::NoDaemon);
    seed_remote_with_note(&bare);
    set_repository_filters(&local, &[], exclude);

    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("clone");
    local
        .git(&[
            "clone",
            &format!(
                "--config=url.{}.insteadOf={}",
                bare.path().display(),
                GITHUB_URL
            ),
            GITHUB_URL,
            target.to_str().unwrap(),
        ])
        .unwrap();
    notes_refs(&TestRepo::new_at_path_with_daemon_scope(
        &target,
        DaemonTestScope::NoDaemon,
    ))
}

#[test]
fn clone_of_allowed_repository_fetches_notes() {
    assert_ne!(
        notes_refs_after_clone(&["https://gitlab.example.com/*"]),
        "",
        "positive control: cloning a tracked repository fetches refs/notes/ai"
    );
}

#[test]
fn excluded_repository_clone_fetches_no_notes() {
    assert_eq!(
        notes_refs_after_clone(&["https://github.com/*"]),
        "",
        "cloning an excluded repository must not fetch refs/notes/ai"
    );
}

/// Cherry-pick a commit whose note exists only on origin. Rewrites fetch
/// missing source notes from every remote; returns the notes refs the local
/// repo ends up with.
fn notes_refs_after_cherry_pick_of_remote_noted_commit(exclude: &[&str]) -> String {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    let noted = seed_remote_with_note(&upstream);
    local.git_og(&["fetch", "origin", "main"]).unwrap();
    assert_eq!(notes_refs(&local), "", "precondition: no notes yet");
    set_repository_filters(&local, &[], exclude);

    fs::write(local.path().join("local.txt"), "local\n").unwrap();
    commit_all(&local, "local base");
    local.git(&["cherry-pick", &noted]).unwrap();
    local.sync_daemon_force();
    notes_refs(&local)
}

#[test]
fn cherry_pick_in_allowed_repository_fetches_missing_source_note() {
    assert_ne!(
        notes_refs_after_cherry_pick_of_remote_noted_commit(&["https://gitlab.example.com/*"]),
        "",
        "positive control: a rewrite fetches the source note from origin"
    );
}

#[test]
fn excluded_repository_cherry_pick_fetches_no_source_notes() {
    assert_eq!(
        notes_refs_after_cherry_pick_of_remote_noted_commit(&["https://github.com/*"]),
        "",
        "a rewrite in an excluded repository must not fetch notes from its remotes"
    );
}

/// A repository allowed via its GitLab origin must not fetch source notes from
/// a GitHub remote the allowlist does not cover.
#[test]
fn cherry_pick_fetches_no_source_notes_from_remote_outside_allowlist() {
    let (local, gitlab) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    let github = TestRepo::new_bare_with_daemon_scope(DaemonTestScope::NoDaemon);
    set_remote(&local, "origin", GITLAB_URL, gitlab.path());
    set_remote(&local, "upstream", GITHUB_URL, github.path());
    let noted = seed_remote_with_note(&github);
    local.git_og(&["fetch", "upstream", "main"]).unwrap();
    set_repository_filters(&local, &["https://gitlab.example.com/*"], &[]);

    fs::write(local.path().join("local.txt"), "local\n").unwrap();
    commit_all(&local, "local base");
    local.git(&["cherry-pick", &noted]).unwrap();
    local.sync_daemon_force();

    let refs = notes_refs(&local);
    assert!(
        !refs.contains("ai-remote/upstream"),
        "notes must not be fetched from a remote outside allow_repositories: {refs}"
    );
    assert_eq!(
        local.read_authorship_note(&head_sha(&local)),
        None,
        "the cherry-pick must not pick up the note held only by the rejected remote"
    );
}

/// Merge `key: value` into the git-ai config the CLI and the daemon read.
fn set_config_value(repo: &TestRepo, key: &str, value: serde_json::Value) {
    let mut homes = vec![repo.test_home_path().clone(), repo.daemon_home_path()];
    homes.dedup();
    for home in homes {
        let config_path = home.join(".git-ai/config.json");
        let mut config: serde_json::Map<String, serde_json::Value> =
            fs::read_to_string(&config_path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        config.insert(key.to_string(), value.clone());
        fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
    }
}

const NATIVE_FETCH_HINT: &str = "git fetch origin refs/notes/ai:refs/notes/ai";

#[test]
fn fetch_notes_in_excluded_repository_is_skipped_with_hint() {
    let (local, upstream) = filtered_repo_with_remote();
    seed_remote_with_note(&upstream);

    let output = local.git_ai(&["fetch-notes"]).unwrap();

    assert!(output.contains("Skipped"), "{output}");
    assert!(
        output.contains("exclude_repositories pattern 'https://github.com/*'"),
        "the hint names the matching pattern: {output}"
    );
    assert!(
        output.contains("remove 'https://github.com/*' from exclude_repositories"),
        "the hint offers editing the filter: {output}"
    );
    assert!(
        output.contains(NATIVE_FETCH_HINT),
        "the hint offers the native git fetch: {output}"
    );
    assert_eq!(notes_refs(&local), "", "nothing was fetched");
}

#[test]
fn fetch_notes_outside_allowlist_hints_at_adding_the_url() {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    seed_remote_with_note(&upstream);
    set_repository_filters(&local, &["https://gitlab.example.com/*"], &[]);

    let output = local.git_ai(&["fetch-notes", "origin"]).unwrap();

    assert!(
        output.contains(&format!(
            "git ai config --add allow_repositories {GITHUB_URL}"
        )),
        "{output}"
    );
    assert!(output.contains(NATIVE_FETCH_HINT), "{output}");
    assert_eq!(notes_refs(&local), "");
}

#[test]
fn fetch_notes_json_reports_skipped() {
    let (local, upstream) = filtered_repo_with_remote();
    seed_remote_with_note(&upstream);

    let output = local.git_ai(&["fetch-notes", "--json"]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();

    assert_eq!(parsed["status"], "skipped", "{output}");
    assert_eq!(parsed["remote"], "origin");
    assert_eq!(notes_refs(&local), "");
}

#[test]
fn fetch_notes_with_http_backend_hints_only_at_the_filters() {
    let (local, _upstream) = filtered_repo_with_remote();
    // Unroutable on purpose: the skip must happen before any backend request.
    set_config_value(
        &local,
        "notes_backend",
        json!({"kind": "http", "backend_url": "http://127.0.0.1:9"}),
    );

    let output = local.git_ai(&["fetch-notes"]).unwrap();

    assert!(output.contains("remove 'https://github.com/*'"), "{output}");
    assert!(
        !output.contains("git fetch"),
        "HTTP-backend notes are not in refs, so no native fetch is offered: {output}"
    );
}

#[test]
fn fetch_authorship_notes_machine_command_reports_skipped() {
    let (local, upstream) = filtered_repo_with_remote();
    seed_remote_with_note(&upstream);

    let request = json!({"remote_name": "origin"}).to_string();
    let output = local
        .git_ai(&["fetch-authorship-notes", "--json", &request])
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();

    assert_eq!(parsed["notes_existence"], "skipped", "{output}");
    assert_eq!(notes_refs(&local), "");
}

/// Reads are not gated: notes the user fetched with plain git (the hint's
/// second option) show up in `git ai blame` of an excluded repository.
#[test]
fn natively_fetched_notes_are_readable_in_excluded_repository() {
    let (tracked, shared) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    tracked
        .filename("ai.txt")
        .set_contents(vec!["line from an agent".ai()]);
    let sha = commit_all(&tracked, "agent change");
    tracked.git(&["push", "origin", "HEAD:main"]).unwrap();
    tracked.sync_daemon_force();
    assert!(
        tracked
            .read_authorship_note_in_git_dir(shared.path(), &sha)
            .is_some(),
        "precondition: the tracked repository pushed its note"
    );

    let (local, _unused) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, shared.path());
    set_repository_filters(&local, &[], &["https://github.com/*"]);
    local.git_og(&["fetch", "origin", "main"]).unwrap();
    local.git_og(&["reset", "--hard", "FETCH_HEAD"]).unwrap();

    local.git_ai(&["fetch-notes"]).unwrap();
    assert_eq!(
        notes_refs(&local),
        "",
        "precondition: git-ai fetched nothing"
    );

    local
        .git_og(&["fetch", "origin", "refs/notes/ai:refs/notes/ai"])
        .unwrap();
    local
        .filename("ai.txt")
        .assert_lines_and_blame(vec!["line from an agent".ai()]);
}

/// Run `git ai notes migrate` in a repository with a local note, against an
/// HTTP backend that is unroutable on purpose. Returns the command output,
/// whether it succeeded or not.
fn notes_migrate_output(exclude: &[&str]) -> String {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    fs::write(local.path().join("a.txt"), "a\n").unwrap();
    let sha = commit_all(&local, "commit with a note");
    local
        .git_og(&[
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            "existing note",
            &sha,
        ])
        .unwrap();
    set_repository_filters(&local, &[], exclude);
    set_config_value(
        &local,
        "notes_backend",
        json!({"kind": "http", "backend_url": "http://127.0.0.1:9"}),
    );
    set_config_value(&local, "api_key", json!("test-key"));

    local
        .git_ai(&["notes", "migrate"])
        .unwrap_or_else(|output| output)
}

#[test]
fn notes_migrate_in_allowed_repository_uploads() {
    let output = notes_migrate_output(&["https://gitlab.example.com/*"]);
    assert!(
        output.contains("Found 1 note(s)"),
        "positive control: migrate reads the note and tries to upload it: {output}"
    );
}

#[test]
fn notes_migrate_in_excluded_repository_uploads_nothing() {
    let output = notes_migrate_output(&["https://github.com/*"]);
    assert!(output.contains("Skipped migrating notes"), "{output}");
    assert!(
        output.contains("remove 'https://github.com/*' from exclude_repositories"),
        "the hint says how to change the filters: {output}"
    );
    assert!(
        !output.contains("Listing notes") && !output.contains("chunk"),
        "nothing is read or uploaded: {output}"
    );
}

/// With filters set, a checkpoint for a repository git-ai cannot resolve (here
/// a `.git` file pointing nowhere) is skipped: the filters cannot be checked,
/// so it is treated as not allowed.
#[test]
fn checkpoint_in_unresolvable_repository_is_skipped_when_filters_are_set() {
    let (local, _upstream) =
        TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_repository_filters(&local, &[], &["https://github.com/*"]);

    let broken = tempfile::tempdir().unwrap();
    fs::write(
        broken.path().join(".git"),
        format!("gitdir: {}\n", broken.path().join("missing").display()),
    )
    .unwrap();
    let file = broken.path().join("a.txt");
    fs::write(&file, "agent line\n").unwrap();

    let output = local
        .git_ai(&["checkpoint", "mock_ai", file.to_str().unwrap()])
        .unwrap_or_else(|output| output);

    assert!(
        output.contains("Skipping checkpoint"),
        "an unresolvable repository must not pass the filters: {output}"
    );
}

/// Rebase an AI-authored `feature` commit after setting `exclude` (origin =
/// GITHUB_URL). Returns the RewriteCommitted metrics persisted for the rebase.
fn rewrite_metrics_after_rebase(exclude: &[&str]) -> Vec<MetricEvent> {
    let (_metrics_db_dir, metrics_db_path) = isolated_metrics_db_path();
    let local =
        TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_METRICS_DB_PATH", metrics_db_path.as_str())]);
    local
        .git_og(&["remote", "add", "origin", GITHUB_URL])
        .unwrap();
    fs::write(local.path().join("README.md"), "base\n").unwrap();
    commit_all(&local, "base");
    let main_branch = local.current_branch();

    local.git(&["checkout", "-b", "feature"]).unwrap();
    // A real preset: mock presets are excluded from commit metrics.
    let feature_file = local.path().join("feature.txt");
    codex_checkpoint(
        &local,
        &feature_file,
        "codex-rebase",
        "PreToolUse",
        "tool-1",
    );
    fs::write(&feature_file, "agent feature line\n").unwrap();
    codex_checkpoint(
        &local,
        &feature_file,
        "codex-rebase",
        "PostToolUse",
        "tool-1",
    );
    let feature = commit_all(&local, "agent feature");
    local.git(&["checkout", &main_branch]).unwrap();
    fs::write(local.path().join("main.txt"), "main moves on\n").unwrap();
    commit_all(&local, "main moves on");
    assert!(
        local.read_authorship_note(&feature).is_some(),
        "precondition: the feature commit got a note while the repository was tracked"
    );
    // The daemon handles commits asynchronously: finish them before excluding.
    local.sync_daemon_force();

    set_repository_filters(&local, &[], exclude);
    local.git(&["checkout", "feature"]).unwrap();
    local.git(&["rebase", &main_branch]).unwrap();
    local.sync_daemon_force();

    // Rewrite metrics are built on a background task after the rewrite is
    // processed, so poll; an excluded repository waits out the deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let events: Vec<MetricEvent> = MetricsDatabase::open_at_path(Path::new(&metrics_db_path))
            .unwrap()
            .get_metric_history(0, None, &[MetricEventId::RewriteCommitted as u16])
            .unwrap()
            .into_iter()
            .map(|record| record.event)
            .collect();
        if !events.is_empty() || std::time::Instant::now() >= deadline {
            return events;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn rebase_in_allowed_repository_records_rewrite_metrics() {
    assert!(
        !rewrite_metrics_after_rebase(&["https://gitlab.example.com/*"]).is_empty(),
        "positive control: a rebase in a tracked repository records RewriteCommitted"
    );
}

#[test]
fn excluded_repository_rebase_records_no_rewrite_metrics() {
    let events = rewrite_metrics_after_rebase(&["https://github.com/*"]);
    assert!(
        events.is_empty(),
        "a rebase in an excluded repository must not record RewriteCommitted: {events:?}"
    );
}

/// Squash-merge a fork PR whose commit carries a git-ai note and run
/// `git ai ci local merge` with `--fork-clone-url
/// https://github.com/contributor/filtered.git` (transport: the fork repo).
/// The upstream repo's origin is GITLAB_URL. Returns the CI output and the
/// notes refs the upstream repo ends up with.
fn ci_fork_import_with_filters(exclude: &[&str]) -> (String, String) {
    ci_fork_import(exclude, "https://github.com/contributor/filtered.git", true)
}

/// [`ci_fork_import_with_filters`] with `--fork-clone-url fork_url`, whose
/// transport is the fork repo only if `route_to_fork`.
fn ci_fork_import(exclude: &[&str], fork_url: &str, route_to_fork: bool) -> (String, String) {
    let upstream = TestRepo::new_with_daemon_scope(DaemonTestScope::Dedicated);
    fs::write(upstream.path().join("app.js"), "// App v1\n").unwrap();
    upstream.git_og(&["add", "-A"]).unwrap();
    upstream.git_og(&["commit", "-q", "-m", "base"]).unwrap();
    upstream.git_og(&["branch", "-M", "main"]).unwrap();
    let base_sha = head_sha(&upstream);
    set_remote(&upstream, "origin", GITLAB_URL, upstream.path());

    let fork = TestRepo::new();
    fork.git_og(&["fetch", "-q", upstream.path().to_str().unwrap(), "main"])
        .unwrap();
    fork.git_og(&["checkout", "-q", "-b", "main", "FETCH_HEAD"])
        .unwrap();
    let mut fork_file = fork.filename("app.js");
    fork_file.set_contents(lines!["// App v1", "// AI feature".ai()]);
    let fork_head_sha = fork.stage_all_and_commit("AI feature").unwrap().commit_sha;
    assert!(fork.read_authorship_note(&fork_head_sha).is_some());

    upstream
        .git_og(&[
            "fetch",
            "-q",
            fork.path().to_str().unwrap(),
            "main:refs/fork/main",
        ])
        .unwrap();
    if route_to_fork {
        upstream
            .git_og(&[
                "config",
                &format!("url.{}.insteadOf", fork.path().to_str().unwrap()),
                fork_url,
            ])
            .unwrap();
    }
    fs::write(upstream.path().join("app.js"), "// App v1\n// AI feature\n").unwrap();
    upstream.git_og(&["add", "-A"]).unwrap();
    upstream
        .git_og(&["commit", "-q", "-m", "Merge fork PR via squash"])
        .unwrap();
    let merge_sha = head_sha(&upstream);
    set_repository_filters(&upstream, &[], exclude);

    let output = upstream
        .git_ai(&[
            "ci",
            "local",
            "merge",
            "--merge-commit-sha",
            &merge_sha,
            "--base-ref",
            "main",
            "--head-ref",
            "main",
            "--head-sha",
            &fork_head_sha,
            "--base-sha",
            &base_sha,
            "--fork-clone-url",
            fork_url,
            "--skip-fetch-notes",
            "--skip-fetch-base",
            "--skip-push",
        ])
        .unwrap_or_else(|err| err);
    (output, notes_refs(&upstream))
}

#[test]
fn ci_imports_notes_from_allowed_fork() {
    let (output, refs) = ci_fork_import_with_filters(&["https://gitlab.example.com/fork/*"]);
    assert!(
        refs.lines().any(|r| r == "refs/notes/ai-remote/fork")
            && refs.lines().any(|r| r == "refs/notes/ai"),
        "positive control: CI fetches the fork's notes and rewrites them; refs:\n{}\noutput:\n{}",
        refs,
        output
    );
}

#[test]
fn ci_imports_no_notes_from_excluded_fork() {
    let (output, refs) = ci_fork_import_with_filters(&["https://github.com/*"]);
    assert_eq!(
        refs, "",
        "CI must not fetch or import notes from a fork URL the filters reject; output:\n{}",
        output
    );
    assert!(
        output.contains("https://github.com/contributor/filtered.git matches exclude_repositories pattern 'https://github.com/*'"),
        "expected the skip to name the rule, got:\n{}",
        output
    );
}

/// Commit in a repository whose origin has two `url` values: GITHUB_URL, then
/// GITLAB_URL (git fetches from the first and pushes to both). Returns the
/// commit's authorship note.
fn note_with_two_origin_urls(exclude: &[&str]) -> Option<String> {
    let (local, upstream) = TestRepo::new_with_remote_with_daemon_scope(DaemonTestScope::Dedicated);
    set_remote(&local, "origin", GITHUB_URL, upstream.path());
    local
        .git_og(&["config", "--add", "remote.origin.url", GITLAB_URL])
        .unwrap();
    set_repository_filters(&local, &[], exclude);

    fs::write(local.path().join("human.txt"), "written by a human\n").unwrap();
    let sha = commit_all(&local, "change with two origin urls");
    local.read_authorship_note(&sha)
}

#[test]
fn commit_with_two_unexcluded_origin_urls_writes_note() {
    assert!(
        note_with_two_origin_urls(&["https://other.example.com/*"]).is_some(),
        "positive control: a repository none of whose URLs is excluded gets a note"
    );
}

#[test]
fn commit_writes_no_note_when_any_origin_url_is_excluded() {
    assert_eq!(
        note_with_two_origin_urls(&["https://github.com/*"]),
        None,
        "every url of a remote counts, not only the last one git config lists"
    );
}

#[test]
fn ci_fork_fetch_failure_does_not_print_url_credentials() {
    // Port 9 (discard) on loopback refuses the connection: the fetch fails
    // without leaving the machine.
    let (output, _refs) = ci_fork_import(
        &[],
        "https://x-access-token:SECRET@127.0.0.1:9/fork.git",
        false,
    );
    assert!(
        output.contains("Failed to fetch fork notes"),
        "expected the fetch to fail, got:\n{}",
        output
    );
    assert!(
        !output.contains("SECRET"),
        "CI output must not contain the fork URL's credentials:\n{}",
        output
    );
}
