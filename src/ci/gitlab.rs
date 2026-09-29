use crate::ci::ci_context::{CiContext, CiEvent, CiRunOptions, CiRunResult};
use crate::error::GitAiError;
use crate::git::refs::{AI_AUTHORSHIP_FORK_TRACKING_REF, copy_missing_notes_for_commits_from_ref};
use crate::git::repository::exec_git;
use crate::git::repository::{CommitRange, Repository, find_repository_in_path};
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::path::PathBuf;

const GITLAB_CI_TEMPLATE_YAML: &str = include_str!("workflow_templates/gitlab.yaml");

/// GitLab Merge Request from API response (list endpoint)
#[derive(Debug, Clone, Deserialize)]
struct GitLabMergeRequest {
    iid: u64,
    title: Option<String>,
    source_branch: String,
    target_branch: String,
    sha: String,
    merge_commit_sha: Option<String>,
    squash_commit_sha: Option<String>,
    squash: Option<bool>,
    source_project_id: u64,
    target_project_id: u64,
}

impl GitLabMergeRequest {
    /// Whether this MR is what *produced* `commit_sha`.
    ///
    /// The commit lookup endpoint also returns merge requests that merely
    /// contain the commit, so matching on containment would write authorship
    /// against a commit the MR did not produce.
    fn produced_commit(&self, commit_sha: &str) -> bool {
        self.merge_commit_sha.as_deref() == Some(commit_sha)
            || self.squash_commit_sha.as_deref() == Some(commit_sha)
    }
}

/// GitLab Project API response (minimal fields for fork detection)
#[derive(Debug, Clone, Deserialize)]
struct GitLabProject {
    http_url_to_repo: String,
}

/// Subset of the single-MR endpoint we need. The list endpoint we already
/// hit does NOT include `diff_refs`; this struct deserializes the single-MR
/// response so we can pull the right SHA out for `CiEvent::Merge.base_sha`.
///
/// GitLab notes that `diff_refs` "is empty when the merge request is created,
/// and populates asynchronously," hence the outer `Option`.
#[derive(Debug, Clone, Deserialize)]
struct GitLabMergeRequestDetails {
    diff_refs: Option<GitLabDiffRefs>,
}

/// The `diff_refs` object has three SHAs with subtly different semantics
/// (paraphrased from <https://docs.gitlab.com/api/merge_requests/>):
///
/// - `base_sha`: the **merge-base** of the source and target branches —
///   `git merge-base source target` — the historical fork point. For a
///   long-lived branch on a fast-moving target this is far behind the
///   current target tip.
/// - `start_sha`: the **target branch commit used as the starting point for
///   the diff**. Documented as "usually the same as `base_sha`," but in
///   practice it tracks the target tip at diff render time, not the
///   common ancestor. **This is the semantic match for GitHub's
///   `pull_request.base.sha`** — i.e. "the target tip the MR was opened
///   against."
/// - `head_sha`: the source branch tip. We already get this from `mr.sha`
///   on the list endpoint, so we don't deserialize it here.
///
/// The #1473 retain filter in `CiContext::run_with_options` computes
/// `base_sha..merge_commit_sha` to find commits the MR introduced. For a
/// squash-on-linear-main scenario with `main = B0→B1→B2→B3` and squash
/// commit `S`, the filter needs a range that yields exactly `{S}`. Using
/// `base_sha` (the merge-base `B0`) gives `{B1,B2,B3,S}` and lets the walk
/// match the PR commit count again — recreating the original #1473 bug.
/// Using `start_sha` (the target tip `B3`) gives `{S}` and squash is
/// correctly detected.
///
/// So: we prefer `start_sha`; `base_sha` is a fallback for the edge cases
/// where GitLab returns the former as null but the latter populated.
#[derive(Debug, Clone, Deserialize)]
struct GitLabDiffRefs {
    base_sha: Option<String>,
    start_sha: Option<String>,
}

/// One entry from `GET /projects/:id/merge_requests/:iid/versions`, newest first.
///
/// GitLab records a new diff version every time an MR's source branch changes,
/// including the server-side rewrite its "Rebase" button performs. This list is
/// the only record of the pre-rewrite SHAs.
///
/// - `head_commit_sha`: the source-branch tip for that version.
/// - `base_commit_sha`: the merge-base of source and target for that version —
///   the `onto` the rebase replayed commits on.
#[derive(Debug, Clone, Deserialize)]
struct GitLabMergeRequestVersion {
    head_commit_sha: String,
    base_commit_sha: Option<String>,
}

struct GitLabApi {
    api_url: String,
    auth_header_name: &'static str,
    auth_token: String,
    project_id: String,
}

impl GitLabApi {
    /// Read the GitLab auth token, preferring `GITLAB_TOKEN` (explicitly
    /// configured with API + write scopes) over the auto-provided
    /// `CI_JOB_TOKEN`.
    ///
    /// `CI_JOB_TOKEN` is not on GitLab's allowlist for the merge-request
    /// endpoints this module calls, so it is a fallback that mostly buys a
    /// clearer error than an unset-variable panic.
    fn from_env(project_id: String) -> Result<Self, GitAiError> {
        let (auth_header_name, auth_token) = if let Ok(token) = std::env::var("GITLAB_TOKEN") {
            println!("  Auth: GITLAB_TOKEN");
            ("PRIVATE-TOKEN", token)
        } else if let Ok(token) = std::env::var("CI_JOB_TOKEN") {
            println!("  Auth: CI_JOB_TOKEN");
            ("JOB-TOKEN", token)
        } else {
            return Err(GitAiError::Generic(
                "Neither GITLAB_TOKEN nor CI_JOB_TOKEN environment variable is set".to_string(),
            ));
        };
        let api_url = std::env::var("CI_API_V4_URL").map_err(|_| {
            GitAiError::Generic("CI_API_V4_URL environment variable not set".to_string())
        })?;
        Ok(Self {
            api_url,
            auth_header_name,
            auth_token,
            project_id,
        })
    }

    fn get(&self, endpoint: &str) -> Result<crate::http::Response, String> {
        gitlab_api_get(endpoint, self.auth_header_name, &self.auth_token)
    }

    /// The error is a description rather than a discarded `None`: stdout is the
    /// only diagnostic channel a CI job has.
    fn get_project_json(&self, suffix: &str) -> Result<String, String> {
        let endpoint = format!("{}/projects/{}{}", self.api_url, self.project_id, suffix);
        let resp = self
            .get(&endpoint)
            .map_err(|e| format!("request failed: {}", e))?;
        if resp.status_code != 200 {
            return Err(format!("status {}", resp.status_code));
        }
        Ok(String::from_utf8_lossy(resp.as_bytes()).into_owned())
    }

    /// Fetch the SHA we want to feed into `CiEvent::Merge.base_sha` (the
    /// target-branch starting point of the MR), preferring
    /// `diff_refs.start_sha` over `diff_refs.base_sha`. See [`GitLabDiffRefs`].
    fn fetch_mr_base_sha(&self, iid: u64) -> Option<String> {
        let body = self
            .get_project_json(&format!("/merge_requests/{}", iid))
            .ok()?;
        let diff_refs = serde_json::from_str::<GitLabMergeRequestDetails>(&body)
            .ok()?
            .diff_refs?;

        // Prefer start_sha (target tip at diff render = GitHub's pull_request.base.sha).
        // Fall back to base_sha (merge-base) only if start_sha is null/missing; that
        // produces a wider range that weakens but does not invert the retain filter.
        if let Some(sha) = diff_refs.start_sha {
            Some(sha)
        } else if let Some(sha) = diff_refs.base_sha {
            println!(
                "[GitLab CI] Note: diff_refs.start_sha missing for MR !{}; \
                 using diff_refs.base_sha (merge-base) as fallback. \
                 The #1473 retain filter may be weakened for this MR.",
                iid
            );
            Some(sha)
        } else {
            None
        }
    }

    /// Resolve the merge request a commit belongs to, straight from the commit.
    ///
    /// Preferred over scanning recently merged MRs: it takes the SHA the caller
    /// already has, so it needs no time window and cannot be pushed out of a
    /// capped page of results. That matters for backfills, where the merge may
    /// be arbitrarily far in the past.
    fn find_mr_by_commit(&self, commit_sha: &str) -> CommitLookup {
        // per_page is explicit: the endpoint also returns merge requests that
        // merely contain the commit, and on a busy default branch the producing
        // MR can fall off GitLab's default page of 20.
        let suffix = format!(
            "/repository/commits/{}/merge_requests?per_page=100",
            commit_sha
        );
        let body = match self.get_project_json(&suffix) {
            Ok(body) => body,
            Err(e) => {
                println!(
                    "[GitLab CI] Commit lookup unavailable ({}); falling back to recent-merge scan",
                    e
                );
                return CommitLookup::Unavailable;
            }
        };
        let merge_requests: Vec<GitLabMergeRequest> = match serde_json::from_str(&body) {
            Ok(merge_requests) => merge_requests,
            Err(e) => {
                println!(
                    "[GitLab CI] Could not parse commit lookup response ({}); \
                     falling back to recent-merge scan",
                    e
                );
                return CommitLookup::Unavailable;
            }
        };
        match merge_requests
            .into_iter()
            .find(|mr| mr.produced_commit(commit_sha))
        {
            Some(mr) => CommitLookup::Found(Box::new(mr)),
            None => CommitLookup::NoMatch,
        }
    }

    /// Scan merge requests merged inside a recent window for one whose merge or
    /// squash commit is `commit_sha`.
    ///
    /// Only reached when the commit lookup is unavailable — a server too old to
    /// serve it, or a transport/permission failure. It is bounded twice over, by
    /// `GIT_AI_CI_LOOKBACK_MINUTES` and by a single unpaginated page, so a merge
    /// outside either bound is simply not found.
    ///
    /// `GIT_AI_CI_LOOKBACK_MINUTES` bounds this scan only; the commit lookup
    /// matches an MR of any age.
    fn find_mr_by_lookback(
        &self,
        commit_sha: &str,
    ) -> Result<Option<GitLabMergeRequest>, GitAiError> {
        let lookback_minutes = std::env::var("GIT_AI_CI_LOOKBACK_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        let cutoff = Utc::now() - Duration::minutes(lookback_minutes);
        let cutoff_str = cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let suffix = format!(
            "/merge_requests?state=merged&updated_after={}&order_by=updated_at&sort=desc&per_page=100",
            cutoff_str
        );
        println!(
            "[GitLab CI] Scanning merge requests updated after {}",
            cutoff_str
        );

        let body = self
            .get_project_json(&suffix)
            .map_err(|e| GitAiError::Generic(format!("GitLab API request failed: {}", e)))?;
        let merge_requests: Vec<GitLabMergeRequest> = serde_json::from_str(&body).map_err(|e| {
            GitAiError::Generic(format!("Failed to parse GitLab API response: {}", e))
        })?;

        println!(
            "[GitLab CI] Found {} recently merged MRs",
            merge_requests.len()
        );
        if merge_requests.len() >= 100 {
            println!(
                "[GitLab CI] Warning: the recent-merge window returned a full page; \
                 a matching MR may have been cut off. Narrow GIT_AI_CI_LOOKBACK_MINUTES."
            );
        }

        for mr in &merge_requests {
            println!(
                "[GitLab CI] MR !{}: \"{}\"",
                mr.iid,
                mr.title.as_deref().unwrap_or("(no title)")
            );
            println!("    source_branch: {}", mr.source_branch);
            println!("    target_branch: {}", mr.target_branch);
            println!("    sha (head): {}", mr.sha);
            println!(
                "    merge_commit_sha: {}",
                mr.merge_commit_sha.as_deref().unwrap_or("(none)")
            );
            println!(
                "    squash_commit_sha: {}",
                mr.squash_commit_sha.as_deref().unwrap_or("(none)")
            );
            println!("    squash: {:?}", mr.squash);
            println!(
                "    produced CI_COMMIT_SHA? {}",
                mr.produced_commit(commit_sha)
            );
        }

        let matched = merge_requests
            .into_iter()
            .find(|mr| mr.produced_commit(commit_sha));
        if let Some(mr) = matched.as_ref() {
            println!("[GitLab CI] Found matching MR !{} in recent merges", mr.iid);
        }
        Ok(matched)
    }

    /// Diff versions for an MR, newest first.
    ///
    /// Paginated: the *oldest* page holds the revisions that still carry the
    /// original notes, so stopping at the first page would silently recover
    /// nothing for a long-lived MR. Past the cap the history is reported as
    /// unreadable rather than replayed from a truncated middle.
    fn mr_versions(&self, iid: u64) -> Result<Vec<GitLabMergeRequestVersion>, String> {
        const PER_PAGE: usize = 100;
        const MAX_PAGES: usize = 20;

        let mut all: Vec<GitLabMergeRequestVersion> = Vec::new();
        for page in 1..=MAX_PAGES {
            let body = self.get_project_json(&format!(
                "/merge_requests/{}/versions?per_page={}&page={}",
                iid, PER_PAGE, page
            ))?;
            let versions: Vec<GitLabMergeRequestVersion> = serde_json::from_str(&body)
                .map_err(|e| format!("could not parse versions: {}", e))?;
            let page_len = versions.len();
            all.extend(versions);
            if page_len < PER_PAGE {
                return Ok(all);
            }
        }
        Err(format!(
            "more than {} diff versions; refusing to replay a truncated history",
            PER_PAGE * MAX_PAGES
        ))
    }

    /// Clone URL of an arbitrary project, used to reach a fork's commits.
    ///
    /// The URL comes from the authenticated API rather than from the fork
    /// owner, so it always points at this GitLab instance.
    fn project_clone_url(&self, project_id: u64) -> Result<String, String> {
        let endpoint = format!("{}/projects/{}", self.api_url, project_id);
        let resp = self
            .get(&endpoint)
            .map_err(|e| format!("request failed: {}", e))?;
        if resp.status_code != 200 {
            return Err(format!("status {}", resp.status_code));
        }
        let body = String::from_utf8_lossy(resp.as_bytes());
        serde_json::from_str::<GitLabProject>(&body)
            .map(|project| project.http_url_to_repo)
            .map_err(|e| format!("could not parse project: {}", e))
    }
}

/// `NoMatch` and `Unavailable` are kept apart so the lookback scan only runs
/// when the lookup could not answer. A commit that genuinely has no merge
/// request — an ordinary push to the default branch — would otherwise pay a
/// full `state=merged` scan on every pipeline.
#[derive(Debug)]
enum CommitLookup {
    Found(Box<GitLabMergeRequest>),
    NoMatch,
    Unavailable,
}

/// Replace the `user:secret@` part of every URL in `text` with `***:***@`.
///
/// Credentialed remote URLs are passed to git as positional arguments, and
/// `GitAiError::GitCliError` renders the full argv, so an ordinary clone failure
/// would otherwise print the token into the job log.
fn redact_url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme_at) = rest.find("://") {
        let after_scheme = scheme_at + 3;
        let authority_end = rest[after_scheme..]
            .find(|c: char| c == '/' || c == '"' || c == '\'' || c.is_whitespace())
            .map(|i| after_scheme + i)
            .unwrap_or(rest.len());
        let authority = &rest[after_scheme..authority_end];
        if let Some(at) = authority.rfind('@') {
            out.push_str(&rest[..after_scheme]);
            out.push_str("***:***@");
            out.push_str(&authority[at + 1..]);
        } else {
            out.push_str(&rest[..authority_end]);
        }
        rest = &rest[authority_end..];
    }
    out.push_str(rest);
    out
}

/// Run a git command, stripping credentials out of any error before it can
/// reach the job log. Every git invocation in this module takes a credentialed
/// URL, so use this rather than `exec_git` directly.
fn exec_git_redacted(args: &[String]) -> Result<(), GitAiError> {
    exec_git(args)
        .map(|_| ())
        .map_err(|e| GitAiError::Generic(redact_url_credentials(&e.to_string())))
}

/// Build and send an authenticated GET to a GitLab REST endpoint.
fn gitlab_api_get(
    endpoint: &str,
    auth_header_name: &str,
    auth_token: &str,
) -> Result<crate::http::Response, String> {
    let agent = crate::http::build_agent(Some(30));
    let request = agent
        .get(endpoint)
        .header(auth_header_name, auth_token)
        .header(
            "User-Agent",
            &format!("git-ai/{}", env!("CARGO_PKG_VERSION")),
        );
    crate::http::send(request)
}

/// Credentialed remotes for a CI clone: `CI_JOB_TOKEN` to read, `GITLAB_TOKEN`
/// to write.
struct CiCloneUrls {
    fetch: String,
    push: String,
}

fn build_ci_clone_urls(server_url: &str, project_path: &str) -> CiCloneUrls {
    let clone_url = format!("{}/{}.git", server_url, project_path);
    let scheme = if server_url.starts_with("https") {
        "https"
    } else {
        "http"
    };
    let server_host = server_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let fetch = if let Ok(job_token) = std::env::var("CI_JOB_TOKEN") {
        println!("[GitLab CI] Using CI_JOB_TOKEN for clone/fetch");
        clone_url.replace(
            server_url,
            &format!("{}://gitlab-ci-token:{}@{}", scheme, job_token, server_host),
        )
    } else {
        println!("[GitLab CI] Warning: CI_JOB_TOKEN not available, clone may fail");
        clone_url.clone()
    };

    if let Ok(gitlab_token) = std::env::var("GITLAB_TOKEN") {
        println!("[GitLab CI] Using GITLAB_TOKEN for push (write_repository scope)");
        CiCloneUrls {
            push: clone_url.replace(
                server_url,
                &format!("{}://oauth2:{}@{}", scheme, gitlab_token, server_host),
            ),
            fetch,
        }
    } else {
        println!("[GitLab CI] Warning: GITLAB_TOKEN not set - push will likely fail");
        println!("[GitLab CI] Create a Project Access Token with write_repository scope");
        CiCloneUrls {
            push: fetch.clone(),
            fetch,
        }
    }
}

/// Attach the job token to a project URL served by this GitLab instance.
///
/// The prefix has to end on an authority boundary: `https://gitlab.example.evil`
/// starts with `https://gitlab.example` and would otherwise be handed the token.
fn authenticate_project_url(url: String, server_url: &str) -> String {
    let scheme = if server_url.starts_with("https") {
        "https"
    } else {
        "http"
    };
    let server_url = server_url.trim_end_matches('/');
    let server_host = server_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let Some(path) = url.strip_prefix(server_url) else {
        return url;
    };
    if !path.is_empty() && !path.starts_with('/') {
        return url;
    }
    match std::env::var("CI_JOB_TOKEN") {
        Ok(job_token) => format!(
            "{}://gitlab-ci-token:{}@{}{}",
            scheme, job_token, server_host, path
        ),
        Err(_) => url,
    }
}

/// GitLab keeps an MR's commits at `refs/merge-requests/:iid/head` even after
/// the source branch is deleted, which is the only way a post-merge job can
/// still reach them.
fn prepare_ci_clone(
    clone_dir: &str,
    urls: &CiCloneUrls,
    target_branch: &str,
    iid: u64,
) -> Result<(), GitAiError> {
    println!("[GitLab CI] Cloning repository...");
    exec_git_redacted(&[
        "clone".to_string(),
        "--branch".to_string(),
        target_branch.to_string(),
        urls.fetch.clone(),
        clone_dir.to_string(),
    ])?;

    println!("[GitLab CI] Setting origin URL for push...");
    exec_git_redacted(&[
        "-C".to_string(),
        clone_dir.to_string(),
        "remote".to_string(),
        "set-url".to_string(),
        "origin".to_string(),
        urls.push.clone(),
    ])?;

    println!(
        "[GitLab CI] Fetching MR commits from refs/merge-requests/{}/head...",
        iid
    );
    exec_git_redacted(&[
        "-C".to_string(),
        clone_dir.to_string(),
        "fetch".to_string(),
        urls.fetch.clone(),
        format!("refs/merge-requests/{}/head:refs/gitlab/mr/{}", iid, iid),
    ])
}

/// Distinct source-branch heads for an MR, oldest first.
///
/// GitLab returns diff versions newest-first and cuts a new one whenever the
/// *target* branch moves too, which leaves the source tip unchanged. Collapsing
/// those keeps one entry per actual source-branch state, carrying the newest
/// base for that head — consecutive entries are then exactly the rewrite hops.
fn revision_chain_from_versions(
    versions: &[GitLabMergeRequestVersion],
) -> Vec<GitLabMergeRequestVersion> {
    let mut chain: Vec<GitLabMergeRequestVersion> = Vec::new();
    for version in versions.iter().rev() {
        if chain
            .last()
            .is_some_and(|last| last.head_commit_sha == version.head_commit_sha)
        {
            chain.pop();
        }
        chain.push(version.clone());
    }
    chain
}

/// Make `commit_sha` resolvable in the clone, fetching it by SHA when it is not
/// reachable from any ref.
///
/// The pre-rewrite commits are unreachable from every branch once GitLab
/// rewrites the source branch, so they can only come back by SHA — and a server
/// may refuse to serve them at all, which is why the error is returned rather
/// than swallowed.
fn ensure_commit_fetched(
    clone_dir: &str,
    commit_sha: &str,
    fetch_url: &str,
    ref_name: &str,
) -> Result<(), GitAiError> {
    let present = exec_git(&[
        "-C".to_string(),
        clone_dir.to_string(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        format!("{}^{{commit}}", commit_sha),
    ])
    .is_ok();
    if present {
        return Ok(());
    }
    exec_git_redacted(&[
        "-C".to_string(),
        clone_dir.to_string(),
        "fetch".to_string(),
        "--no-tags".to_string(),
        fetch_url.to_string(),
        format!("{}:{}", commit_sha, ref_name),
    ])
}

/// Commits a diff version introduced, falling back to its head alone when the
/// recorded base is no longer resolvable in the clone.
fn revision_commits(repo: &Repository, version: &GitLabMergeRequestVersion) -> Vec<String> {
    let head = version.head_commit_sha.clone();
    let Some(base) = version.base_commit_sha.clone() else {
        return vec![head];
    };
    CommitRange::new_infer_refname(repo, base, head.clone(), None)
        .map(|range| range.all_commits())
        .unwrap_or_else(|_| vec![head])
}

/// Copy the fork's notes for a pre-rewrite revision into the clone's own
/// authorship ref.
///
/// A fork MR's attribution exists only in the fork, and the merge handler
/// imports it scoped to the final head, so without this every hop would replay a
/// revision that looks unattributed.
fn import_fork_notes_for_revision(repo: &Repository, version: &GitLabMergeRequestVersion) {
    let commits = revision_commits(repo, version);
    match copy_missing_notes_for_commits_from_ref(repo, AI_AUTHORSHIP_FORK_TRACKING_REF, &commits) {
        Ok(0) => {}
        Ok(copied) => println!(
            "[GitLab CI] Imported {} fork authorship note(s) for pre-rewrite revision {}",
            copied, version.head_commit_sha
        ),
        Err(e) => println!(
            "[GitLab CI] Could not import fork authorship notes for revision {}: {}",
            version.head_commit_sha, e
        ),
    }
}

/// Notes stay local: the caller pushes once, after every hop has landed.
fn replay_rewrite_hop(
    clone_dir: &str,
    previous: &GitLabMergeRequestVersion,
    current: &GitLabMergeRequestVersion,
    target_branch: &str,
    fetch_url: &str,
) -> Result<CiRunResult, GitAiError> {
    let hop_repo = find_repository_in_path(clone_dir)?;
    let context = CiContext::with_repository(
        hop_repo,
        CiEvent::Sync {
            previous_head_sha: previous.head_commit_sha.clone(),
            head_sha: current.head_commit_sha.clone(),
            base_ref: target_branch.to_string(),
            base_sha: current.base_commit_sha.clone().unwrap_or_default(),
            previous_base_sha: previous.base_commit_sha.clone(),
            previous_head_fetch_remote: Some(fetch_url.to_string()),
        },
    );
    context.run_with_options(CiRunOptions {
        skip_fetch_notes: true,
        skip_fetch_base: true,
        skip_fetch_fork_notes: true,
        skip_fetch_sync_refs: false,
        skip_push: true,
    })
}

/// What `recover_rewritten_mr_notes` needs to know about the merge request.
struct RewriteRecovery<'a> {
    clone_dir: &'a str,
    iid: u64,
    merge_commit_sha: &'a str,
    target_branch: &'a str,
    /// Where pre-rewrite commits are fetched from: the fork for a fork MR,
    /// since the target project never had them.
    fetch_url: &'a str,
    fork_clone_url: Option<&'a str>,
    skip_push: bool,
}

/// Walk an already-merged MR's diff-version history and replay every rewrite of
/// its source branch, so the notes written against the *original* commits end up
/// on the commits the merge actually consumed.
///
/// This is what makes the merge-time job self-healing. A rebase performed from
/// the merge request UI rewrites the branch server-side with no git-ai in the
/// loop, so by merge time `base..head` holds only commits that never had notes
/// and there is nothing to carry onto the squash commit. The version history is
/// the only record of the pre-rewrite SHAs, and it outlives the merge.
///
/// Best-effort throughout: a hop whose commits the server will no longer serve,
/// or that is not a clean rebase, is skipped rather than failing the run. The
/// push is the exception - the recovered notes exist only in a clone that
/// teardown deletes, so a silent failure there is unrecoverable attribution.
fn recover_rewritten_mr_notes(
    api: &GitLabApi,
    recovery: RewriteRecovery<'_>,
) -> Result<(), GitAiError> {
    let RewriteRecovery {
        clone_dir,
        iid,
        merge_commit_sha,
        target_branch,
        fetch_url,
        fork_clone_url,
        skip_push,
    } = recovery;
    let versions = match api.mr_versions(iid) {
        Ok(versions) => versions,
        Err(e) => {
            println!(
                "[GitLab CI] Could not read MR !{} diff versions ({}); skipping rebase recovery",
                iid, e
            );
            return Ok(());
        }
    };

    let chain = revision_chain_from_versions(&versions);
    if chain.len() < 2 {
        return Ok(());
    }

    let repo = match find_repository_in_path(clone_dir) {
        Ok(repo) => repo,
        Err(e) => {
            println!(
                "[GitLab CI] Could not open clone for rebase recovery: {}",
                e
            );
            return Ok(());
        }
    };

    if let Err(e) = crate::git::sync_authorship::fetch_authorship_notes(&repo, "origin") {
        println!(
            "[GitLab CI] Could not fetch authorship notes for rebase recovery: {}",
            redact_url_credentials(&e.to_string())
        );
        return Ok(());
    }

    // Already carried forward on an earlier run of this job - nothing to redo.
    if crate::git::notes_api::read_authorship_v3(&repo, merge_commit_sha).is_ok() {
        return Ok(());
    }

    println!(
        "[GitLab CI] Recovering attribution for merge request !{}: \
         source branch was rewritten {} time(s) while open",
        iid,
        chain.len() - 1
    );

    let fork_notes_available = match fork_clone_url {
        Some(fork_url) => match CiContext::fetch_fork_notes(&repo, fork_url) {
            Ok(available) => available,
            Err(e) => {
                println!(
                    "[GitLab CI] Could not fetch fork authorship notes for rebase recovery: {}",
                    redact_url_credentials(&e.to_string())
                );
                false
            }
        },
        None => false,
    };

    let mut rewrote_any = false;
    'hops: for pair in chain.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);

        for (sha, label) in [
            (&previous.head_commit_sha, "Pre-rewrite"),
            (&current.head_commit_sha, "Rewritten"),
        ] {
            if let Err(e) = ensure_commit_fetched(
                clone_dir,
                sha,
                fetch_url,
                &format!("refs/git-ai/gitlab/mr/{}/rev/{}", iid, sha),
            ) {
                println!(
                    "[GitLab CI] {} commit {} is no longer available ({}); \
                     stopping recovery for MR !{}",
                    label, sha, e, iid
                );
                break 'hops;
            }
        }

        if fork_notes_available {
            import_fork_notes_for_revision(&repo, previous);
        }

        match replay_rewrite_hop(clone_dir, previous, current, target_branch, fetch_url) {
            Ok(CiRunResult::SyncAuthorshipRewritten { commit_count }) => {
                println!(
                    "[GitLab CI] Rewrite hop {} -> {}: moved {} commit(s)",
                    previous.head_commit_sha, current.head_commit_sha, commit_count
                );
                rewrote_any = true;
            }
            Ok(CiRunResult::SkippedExistingSyncNotes) => {
                println!(
                    "[GitLab CI] Rewrite hop {} -> {}: already carried forward",
                    previous.head_commit_sha, current.head_commit_sha
                );
            }
            // A version is cut for every source-branch change, not just a
            // rebase. An ordinary push leaves the old notes reachable from the
            // new head, so later hops can still have a rewrite to replay.
            Ok(result) => {
                println!(
                    "[GitLab CI] Rewrite hop {} -> {} moved no attribution ({:?})",
                    previous.head_commit_sha, current.head_commit_sha, result
                );
            }
            Err(e) => {
                println!(
                    "[GitLab CI] Rewrite hop {} -> {} failed ({}); stopping recovery for MR !{}",
                    previous.head_commit_sha,
                    current.head_commit_sha,
                    redact_url_credentials(&e.to_string()),
                    iid
                );
                break;
            }
        }
    }

    // The merge run only pushes when it actually rewrites: it returns before the
    // push for a simple merge commit and for a fast-forward. Those are exactly
    // the cases where the recovery above is the only thing that produced notes,
    // so push here rather than leaving them in a clone that teardown deletes.
    if rewrote_any
        && !skip_push
        && let Err(e) = repo.push_authorship("origin")
    {
        return Err(GitAiError::Generic(format!(
            "could not push recovered attribution for MR !{}: {}",
            iid,
            redact_url_credentials(&e.to_string())
        )));
    }

    Ok(())
}

/// Query GitLab API for the MR that produced the current commit and build a
/// merge context for it. Returns None if no matching MR is found (this is not an
/// error - just means this commit wasn't from a merged MR).
pub fn get_gitlab_ci_context(skip_push: bool) -> Result<Option<CiContext>, GitAiError> {
    let project_id = std::env::var("CI_PROJECT_ID").map_err(|_| {
        GitAiError::Generic("CI_PROJECT_ID environment variable not set".to_string())
    })?;
    let commit_sha = std::env::var("CI_COMMIT_SHA").map_err(|_| {
        GitAiError::Generic("CI_COMMIT_SHA environment variable not set".to_string())
    })?;
    let server_url = std::env::var("CI_SERVER_URL").map_err(|_| {
        GitAiError::Generic("CI_SERVER_URL environment variable not set".to_string())
    })?;
    let project_path = std::env::var("CI_PROJECT_PATH").map_err(|_| {
        GitAiError::Generic("CI_PROJECT_PATH environment variable not set".to_string())
    })?;

    println!("[GitLab CI] Environment:");
    println!("  CI_COMMIT_SHA: {}", commit_sha);
    println!("  CI_PROJECT_ID: {}", project_id);
    println!("  CI_PROJECT_PATH: {}", project_path);

    let api = GitLabApi::from_env(project_id)?;

    let mr = match api.find_mr_by_commit(&commit_sha) {
        CommitLookup::Found(mr) => {
            println!(
                "[GitLab CI] Resolved MR !{} directly from commit {}",
                mr.iid, commit_sha
            );
            *mr
        }
        CommitLookup::NoMatch => {
            println!(
                "[GitLab CI] No merge request produced commit {}. Skipping...",
                commit_sha
            );
            return Ok(None);
        }
        CommitLookup::Unavailable => match api.find_mr_by_lookback(&commit_sha)? {
            Some(mr) => mr,
            None => {
                println!(
                    "[GitLab CI] No merge request found for commit {} \
                     (commit lookup unavailable, and nothing in the recent-merge window). \
                     Skipping...",
                    commit_sha
                );
                return Ok(None);
            }
        },
    };

    // Determine which commit SHA to use as the "merge commit" for rewriting
    // If this was a squash merge, CI_COMMIT_SHA might be the squash commit
    // (which is what we want to rewrite authorship TO)
    let effective_merge_sha = if mr.squash_commit_sha.as_ref() == Some(&commit_sha) {
        println!("[GitLab CI] CI_COMMIT_SHA matches squash_commit_sha - this is a squash merge");
        commit_sha.clone()
    } else {
        println!(
            "[GitLab CI] CI_COMMIT_SHA matches merge_commit_sha - checking if this is a squash+merge"
        );
        // If squash was used but we matched on merge_commit_sha,
        // the actual squash commit is in squash_commit_sha
        if let Some(squash_sha) = &mr.squash_commit_sha {
            println!(
                "[GitLab CI] MR has squash_commit_sha={}, will use that for rewriting",
                squash_sha
            );
            squash_sha.clone()
        } else {
            commit_sha.clone()
        }
    };

    println!(
        "[GitLab CI] Effective merge/squash SHA for rewriting: {}",
        effective_merge_sha
    );

    let fork_clone_url = if mr.source_project_id != mr.target_project_id {
        println!(
            "[GitLab CI] Detected fork MR: source project {} differs from target project {}",
            mr.source_project_id, mr.target_project_id
        );
        match api.project_clone_url(mr.source_project_id) {
            Ok(url) => {
                println!("[GitLab CI] Fork clone URL: {}", url);
                Some(authenticate_project_url(url, &server_url))
            }
            Err(e) => {
                println!(
                    "[GitLab CI] Warning: could not query source project ({}), \
                     fork notes may be lost",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    let clone_dir = "git-ai-ci-clone".to_string();
    let urls = build_ci_clone_urls(&server_url, &project_path);
    prepare_ci_clone(&clone_dir, &urls, &mr.target_branch, mr.iid)?;

    let repo = find_repository_in_path(&clone_dir)?;

    // Fetch diff_refs.base_sha from the single-MR endpoint. The list endpoint
    // we hit earlier doesn't include diff_refs; without base_sha the #1473
    // retain filter in CiContext::run_with_options skips, so squash merges on
    // a linear target branch can still be misclassified as rebases. None here
    // -> fall back to empty string (legacy behavior, no protection).
    let base_sha = api.fetch_mr_base_sha(mr.iid).unwrap_or_else(|| {
        println!(
            "[GitLab CI] Warning: could not fetch diff_refs.base_sha for MR !{}; \
             proceeding without the #1473 retain filter (legacy behavior)",
            mr.iid
        );
        String::new()
    });

    println!(
        "[GitLab CI] Created CiContext: merge_commit_sha={}, head_sha={}, head_ref={}, base_ref={}, base_sha={}",
        effective_merge_sha,
        mr.sha,
        mr.source_branch,
        mr.target_branch,
        if base_sha.is_empty() {
            "(unavailable)"
        } else {
            &base_sha
        }
    );

    // An MR whose branch was rebased while open reaches this point with notes
    // still keyed to the pre-rebase SHAs. Replay those rewrites first so the
    // merge mapping below has something to carry onto the merge commit. A fork's
    // pre-rewrite commits live only in the fork, so fetch from there when we
    // have it.
    recover_rewritten_mr_notes(
        &api,
        RewriteRecovery {
            clone_dir: &clone_dir,
            iid: mr.iid,
            merge_commit_sha: &effective_merge_sha,
            target_branch: &mr.target_branch,
            fetch_url: fork_clone_url.as_deref().unwrap_or(&urls.fetch),
            fork_clone_url: fork_clone_url.as_deref(),
            skip_push,
        },
    )?;

    Ok(Some(CiContext {
        repo,
        event: CiEvent::Merge {
            merge_commit_sha: effective_merge_sha,
            head_ref: mr.source_branch.clone(),
            head_sha: mr.sha.clone(),
            base_ref: mr.target_branch.clone(),
            base_sha,
            fork_clone_url,
        },
        temp_dir: PathBuf::from(clone_dir),
    }))
}

/// Print the GitLab CI YAML snippet to stdout for users to copy into their .gitlab-ci.yml
pub fn print_gitlab_ci_yaml() {
    println!("Add the following to your .gitlab-ci.yml:");
    println!();
    println!("{}", GITLAB_CI_TEMPLATE_YAML);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(head: &str, base: &str) -> GitLabMergeRequestVersion {
        GitLabMergeRequestVersion {
            head_commit_sha: head.to_string(),
            base_commit_sha: Some(base.to_string()),
        }
    }

    fn api_for(url: &str, header: &'static str, token: &str) -> GitLabApi {
        GitLabApi {
            api_url: url.to_string(),
            auth_header_name: header,
            auth_token: token.to_string(),
            project_id: "123".to_string(),
        }
    }

    /// Two merge requests for the same commit: !1 merely contains it, !2
    /// produced it as its squash commit.
    const COMMIT_LOOKUP_BODY: &str = r#"[
        {"iid": 1, "title": "contains it", "source_branch": "a", "target_branch": "main",
         "sha": "head1", "merge_commit_sha": null, "squash_commit_sha": "other",
         "squash": true, "source_project_id": 1, "target_project_id": 1},
        {"iid": 2, "title": "produced it", "source_branch": "b", "target_branch": "main",
         "sha": "head2", "merge_commit_sha": null, "squash_commit_sha": "target",
         "squash": true, "source_project_id": 1, "target_project_id": 1}
    ]"#;

    #[test]
    fn test_gitlab_merge_request_version_deserialization() {
        let json = r#"[{
            "id": 110,
            "head_commit_sha": "33e2ee8579fda5bc36accc9c6fbd0b4fefda9e30",
            "base_commit_sha": "eeb57dffe83deb686a60a71c16c32f71046868fd",
            "start_commit_sha": "0b4bc9a49b562e85de7cc9e834518ea6828729b9",
            "created_at": "2026-06-01T12:00:00.000Z",
            "merge_request_id": 105,
            "state": "collected",
            "real_size": "1"
        }]"#;
        let versions: Vec<GitLabMergeRequestVersion> = serde_json::from_str(json).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(
            versions[0].head_commit_sha,
            "33e2ee8579fda5bc36accc9c6fbd0b4fefda9e30"
        );
        assert_eq!(
            versions[0].base_commit_sha.as_deref(),
            Some("eeb57dffe83deb686a60a71c16c32f71046868fd")
        );
    }

    #[test]
    fn test_gitlab_merge_request_version_tolerates_null_base() {
        let json = r#"[{"head_commit_sha": "aaa", "base_commit_sha": null}]"#;
        let versions: Vec<GitLabMergeRequestVersion> = serde_json::from_str(json).unwrap();
        assert!(versions[0].base_commit_sha.is_none());
    }

    /// The commit lookup endpoint also returns MRs that merely *contain* the
    /// commit, so a match must be the MR's own merge or squash commit. These
    /// drive `find_mr_by_commit` rather than restating its predicate, so a
    /// change to the rule fails here.
    #[test]
    fn test_commit_lookup_matches_the_mr_that_produced_the_commit() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/repository/commits/target/merge_requests?per_page=100",
            )
            .with_status(200)
            .with_body(COMMIT_LOOKUP_BODY)
            .create();

        let found = api_for(&server.url(), "PRIVATE-TOKEN", "tok").find_mr_by_commit("target");
        mock.assert();
        match found {
            CommitLookup::Found(mr) => assert_eq!(mr.iid, 2),
            other => panic!("expected the producing MR, got {:?}", other),
        }
    }

    #[test]
    fn test_commit_lookup_accepts_merge_commit_sha() {
        let body = r#"[{"iid": 9, "title": null, "source_branch": "b", "target_branch": "main",
             "sha": "head", "merge_commit_sha": "target", "squash_commit_sha": null,
             "squash": false, "source_project_id": 1, "target_project_id": 1}]"#;
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/repository/commits/target/merge_requests?per_page=100",
            )
            .with_status(200)
            .with_body(body)
            .create();

        let found = api_for(&server.url(), "PRIVATE-TOKEN", "tok").find_mr_by_commit("target");
        mock.assert();
        match found {
            CommitLookup::Found(mr) => assert_eq!(mr.iid, 9),
            other => panic!("expected a match on merge_commit_sha, got {:?}", other),
        }
    }

    /// A commit no MR produced must report `NoMatch`, not `Unavailable`: an
    /// ordinary push to the default branch would otherwise pay a full
    /// recent-merge scan on every pipeline.
    #[test]
    fn test_commit_lookup_rejects_containing_only_mr_without_falling_back() {
        let body = r#"[{"iid": 3, "title": null, "source_branch": "b", "target_branch": "main",
             "sha": "head", "merge_commit_sha": null, "squash_commit_sha": "something-else",
             "squash": true, "source_project_id": 1, "target_project_id": 1}]"#;
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/repository/commits/target/merge_requests?per_page=100",
            )
            .with_status(200)
            .with_body(body)
            .create();

        let found = api_for(&server.url(), "PRIVATE-TOKEN", "tok").find_mr_by_commit("target");
        mock.assert();
        assert!(matches!(found, CommitLookup::NoMatch), "got {:?}", found);
    }

    #[test]
    fn test_commit_lookup_unavailable_on_error_status() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/repository/commits/target/merge_requests?per_page=100",
            )
            .with_status(500)
            .with_body("boom")
            .create();

        let found = api_for(&server.url(), "PRIVATE-TOKEN", "tok").find_mr_by_commit("target");
        mock.assert();
        assert!(
            matches!(found, CommitLookup::Unavailable),
            "got {:?}",
            found
        );
    }

    #[test]
    fn test_commit_lookup_unavailable_on_malformed_body() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/repository/commits/target/merge_requests?per_page=100",
            )
            .with_status(200)
            .with_body("not json")
            .create();

        let found = api_for(&server.url(), "PRIVATE-TOKEN", "tok").find_mr_by_commit("target");
        mock.assert();
        assert!(
            matches!(found, CommitLookup::Unavailable),
            "got {:?}",
            found
        );
    }

    #[test]
    fn test_revision_chain_is_oldest_first() {
        // GitLab returns versions newest-first.
        let versions = vec![
            version("c", "base3"),
            version("b", "base2"),
            version("a", "base1"),
        ];
        let chain = revision_chain_from_versions(&versions);
        let heads: Vec<&str> = chain.iter().map(|v| v.head_commit_sha.as_str()).collect();
        assert_eq!(heads, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_revision_chain_collapses_target_only_versions_keeping_newest_base() {
        // Two versions share head "b": the target branch moved without the
        // source branch being rewritten. Collapse to one hop, newest base wins.
        let versions = vec![
            version("b", "base3"),
            version("b", "base2"),
            version("a", "base1"),
        ];
        let chain = revision_chain_from_versions(&versions);
        let heads: Vec<&str> = chain.iter().map(|v| v.head_commit_sha.as_str()).collect();
        assert_eq!(heads, vec!["a", "b"]);
        assert_eq!(chain[1].base_commit_sha.as_deref(), Some("base3"));
    }

    #[test]
    fn test_revision_chain_single_version_yields_no_hops() {
        let chain = revision_chain_from_versions(&[version("only", "base")]);
        assert_eq!(chain.len(), 1);
        assert!(chain.windows(2).next().is_none());
    }

    #[test]
    fn test_revision_chain_empty_input() {
        assert!(revision_chain_from_versions(&[]).is_empty());
    }

    #[test]
    fn test_revision_chain_multiple_rewrites_produce_consecutive_hops() {
        let versions = vec![
            version("d", "base4"),
            version("c", "base3"),
            version("b", "base2"),
            version("a", "base1"),
        ];
        let chain = revision_chain_from_versions(&versions);
        let hops: Vec<(&str, &str)> = chain
            .windows(2)
            .map(|pair| {
                (
                    pair[0].head_commit_sha.as_str(),
                    pair[1].head_commit_sha.as_str(),
                )
            })
            .collect();
        assert_eq!(hops, vec![("a", "b"), ("b", "c"), ("c", "d")]);
    }

    #[test]
    fn test_gitlab_merge_request_deserialization() {
        let json = r#"{
            "iid": 42,
            "title": "Fix bug",
            "source_branch": "feature/fix",
            "target_branch": "main",
            "sha": "abc123",
            "merge_commit_sha": "def456",
            "squash_commit_sha": null,
            "squash": false,
            "source_project_id": 123,
            "target_project_id": 456
        }"#;
        let mr: GitLabMergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(mr.iid, 42);
        assert_eq!(mr.title, Some("Fix bug".to_string()));
        assert_eq!(mr.source_branch, "feature/fix");
        assert_eq!(mr.target_branch, "main");
        assert_eq!(mr.sha, "abc123");
        assert_eq!(mr.merge_commit_sha, Some("def456".to_string()));
        assert!(mr.squash_commit_sha.is_none());
        assert_eq!(mr.squash, Some(false));
        assert_eq!(mr.source_project_id, 123);
        assert_eq!(mr.target_project_id, 456);
    }

    #[test]
    fn test_gitlab_merge_request_deserialization_with_squash() {
        let json = r#"{
            "iid": 99,
            "title": "Squash merge",
            "source_branch": "feature/squash",
            "target_branch": "main",
            "sha": "head123",
            "merge_commit_sha": "merge456",
            "squash_commit_sha": "squash789",
            "squash": true,
            "source_project_id": 123,
            "target_project_id": 123
        }"#;
        let mr: GitLabMergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(mr.iid, 99);
        assert_eq!(mr.squash_commit_sha, Some("squash789".to_string()));
        assert_eq!(mr.squash, Some(true));
        assert_eq!(mr.source_project_id, 123);
        assert_eq!(mr.target_project_id, 123);
    }

    #[test]
    fn test_gitlab_merge_request_deserialization_minimal() {
        let json = r#"{
            "iid": 1,
            "source_branch": "dev",
            "target_branch": "main",
            "sha": "abc",
            "source_project_id": 999,
            "target_project_id": 999
        }"#;
        let mr: GitLabMergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(mr.iid, 1);
        assert!(mr.title.is_none());
        assert!(mr.merge_commit_sha.is_none());
        assert!(mr.squash_commit_sha.is_none());
        assert!(mr.squash.is_none());
        assert_eq!(mr.source_project_id, 999);
        assert_eq!(mr.target_project_id, 999);
    }

    #[test]
    fn test_gitlab_ci_template_yaml_not_empty() {
        assert!(
            !GITLAB_CI_TEMPLATE_YAML.is_empty(),
            "GitLab CI template YAML should not be empty"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_lookback_minutes_defaults_to_15() {
        unsafe { std::env::remove_var("GIT_AI_CI_LOOKBACK_MINUTES") };
        let lookback = std::env::var("GIT_AI_CI_LOOKBACK_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15i64);
        assert_eq!(lookback, 15);
    }

    #[test]
    #[serial_test::serial]
    fn test_lookback_minutes_reads_env_var() {
        unsafe { std::env::set_var("GIT_AI_CI_LOOKBACK_MINUTES", "4320") };
        let lookback = std::env::var("GIT_AI_CI_LOOKBACK_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15i64);
        unsafe { std::env::remove_var("GIT_AI_CI_LOOKBACK_MINUTES") };
        assert_eq!(lookback, 4320);
    }

    #[test]
    #[serial_test::serial]
    fn test_lookback_minutes_falls_back_on_invalid_value() {
        unsafe { std::env::set_var("GIT_AI_CI_LOOKBACK_MINUTES", "not-a-number") };
        let lookback = std::env::var("GIT_AI_CI_LOOKBACK_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15i64);
        unsafe { std::env::remove_var("GIT_AI_CI_LOOKBACK_MINUTES") };
        assert_eq!(lookback, 15);
    }

    // ---- CiEvent::Merge.base_sha derivation from diff_refs ----
    //
    // We prefer `diff_refs.start_sha` (target tip at diff render, semantically
    // equal to GitHub's `pull_request.base.sha`) over `diff_refs.base_sha`
    // (merge-base). See the `GitLabDiffRefs` docstring for why; tests below
    // pin the preference + fallback + every silently-absorbed failure mode.

    #[test]
    fn test_diff_refs_deserialization_happy() {
        let json = r#"{
            "iid": 42,
            "diff_refs": {
                "base_sha": "0000000000000000000000000000000000000000",
                "head_sha": "1111111111111111111111111111111111111111",
                "start_sha": "2222222222222222222222222222222222222222"
            }
        }"#;
        let details: GitLabMergeRequestDetails = serde_json::from_str(json).unwrap();
        let diff_refs = details.diff_refs.unwrap();
        assert_eq!(
            diff_refs.base_sha,
            Some("0000000000000000000000000000000000000000".to_string())
        );
        assert_eq!(
            diff_refs.start_sha,
            Some("2222222222222222222222222222222222222222".to_string())
        );
    }

    #[test]
    fn test_diff_refs_deserialization_missing_diff_refs() {
        // GitLab notes diff_refs "is empty when the merge request is created,
        // and populates asynchronously"; absorb that as None.
        let json = r#"{"iid": 1}"#;
        let details: GitLabMergeRequestDetails = serde_json::from_str(json).unwrap();
        assert!(details.diff_refs.is_none());
    }

    #[test]
    fn test_diff_refs_deserialization_null_shas() {
        // Both SHAs JSON-null — surface as None on both fields.
        let json = r#"{
            "iid": 7,
            "diff_refs": { "base_sha": null, "start_sha": null }
        }"#;
        let details: GitLabMergeRequestDetails = serde_json::from_str(json).unwrap();
        let diff_refs = details.diff_refs.unwrap();
        assert!(diff_refs.base_sha.is_none());
        assert!(diff_refs.start_sha.is_none());
    }

    /// Happy path: both SHAs present, we MUST pick start_sha. This is the
    /// load-bearing test — picking base_sha here recreates the original
    /// #1473 bug for GitLab squash MRs.
    #[test]
    fn test_fetch_mr_base_sha_prefers_start_sha_over_base_sha() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .match_header("PRIVATE-TOKEN", "test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Distinct values so the assertion can't pass by coincidence.
            .with_body(
                r#"{
                "iid": 42,
                "diff_refs": {
                    "base_sha":  "0000000000000000000000000000000000000000",
                    "start_sha": "2222222222222222222222222222222222222222",
                    "head_sha":  "1111111111111111111111111111111111111111"
                }
            }"#,
            )
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "test-token").fetch_mr_base_sha(42);

        mock.assert();
        assert_eq!(
            result,
            Some("2222222222222222222222222222222222222222".to_string()),
            "must prefer start_sha (target tip) over base_sha (merge-base)"
        );
    }

    /// Fallback: GitLab returns diff_refs with base_sha populated but
    /// start_sha null/missing. Use base_sha and continue; the retain
    /// filter is weakened but not broken.
    #[test]
    fn test_fetch_mr_base_sha_falls_back_to_base_sha_when_start_sha_missing() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .with_status(200)
            .with_body(
                r#"{
                "iid": 42,
                "diff_refs": {
                    "base_sha":  "0000000000000000000000000000000000000000",
                    "start_sha": null
                }
            }"#,
            )
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "tok").fetch_mr_base_sha(42);
        mock.assert();
        assert_eq!(
            result,
            Some("0000000000000000000000000000000000000000".to_string())
        );
    }

    #[test]
    fn test_fetch_mr_base_sha_returns_none_when_both_shas_missing() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .with_status(200)
            .with_body(
                r#"{
                "iid": 42,
                "diff_refs": { "base_sha": null, "start_sha": null }
            }"#,
            )
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "tok").fetch_mr_base_sha(42);
        mock.assert();
        assert!(result.is_none());
    }

    #[test]
    fn test_fetch_mr_base_sha_404_returns_none() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .with_status(404)
            .with_body(r#"{"message": "404 Not Found"}"#)
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "tok").fetch_mr_base_sha(42);
        mock.assert();
        assert!(
            result.is_none(),
            "404 should fall through to None (caller uses empty string)"
        );
    }

    #[test]
    fn test_fetch_mr_base_sha_malformed_body_returns_none() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .with_status(200)
            .with_body("not json")
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "tok").fetch_mr_base_sha(42);
        mock.assert();
        assert!(result.is_none(), "non-JSON body should not panic");
    }

    #[test]
    fn test_fetch_mr_base_sha_missing_diff_refs_returns_none() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .with_status(200)
            .with_body(r#"{"iid": 42}"#)
            .create();

        let result = api_for(&server.url(), "PRIVATE-TOKEN", "tok").fetch_mr_base_sha(42);
        mock.assert();
        assert!(result.is_none());
    }

    #[test]
    fn test_fetch_mr_base_sha_with_job_token_header() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/123/merge_requests/42")
            .match_header("JOB-TOKEN", "ci-job-token-value")
            .with_status(200)
            // start_sha present so the happy path through JOB-TOKEN auth fires.
            .with_body(
                r#"{"diff_refs": {"start_sha": "abc1234567890abcdef1234567890abcdef12345"}}"#,
            )
            .create();

        let result =
            api_for(&server.url(), "JOB-TOKEN", "ci-job-token-value").fetch_mr_base_sha(42);
        mock.assert();
        assert_eq!(
            result,
            Some("abc1234567890abcdef1234567890abcdef12345".to_string())
        );
    }

    // ---- Credential redaction ----

    #[test]
    fn test_redact_url_credentials_strips_userinfo() {
        let text =
            "Git CLI (clone https://gitlab-ci-token:secret-value@gitlab.com/g/p.git dir) failed";
        let redacted = redact_url_credentials(text);
        assert!(!redacted.contains("secret-value"), "{}", redacted);
        assert!(
            redacted.contains("https://***:***@gitlab.com/g/p.git"),
            "{}",
            redacted
        );
    }

    #[test]
    fn test_redact_url_credentials_leaves_plain_urls_alone() {
        let text = "fetched https://gitlab.com/group/project.git ok";
        assert_eq!(redact_url_credentials(text), text);
    }

    #[test]
    fn test_redact_url_credentials_handles_several_urls() {
        let text = "https://oauth2:aaa@host/x.git and https://gitlab-ci-token:bbb@host/y.git";
        let redacted = redact_url_credentials(text);
        assert!(!redacted.contains("aaa"), "{}", redacted);
        assert!(!redacted.contains("bbb"), "{}", redacted);
    }

    #[test]
    fn test_redact_url_credentials_stops_authority_at_newline() {
        let text = "remote: https://oauth2:aaa@host\nfatal: https://gitlab-ci-token:bbb@host/y.git";
        let redacted = redact_url_credentials(text);
        assert!(!redacted.contains("aaa"), "{}", redacted);
        assert!(!redacted.contains("bbb"), "{}", redacted);
    }

    // ---- Auth resolution ----

    fn clear_auth_env() {
        unsafe {
            std::env::remove_var("GITLAB_TOKEN");
            std::env::remove_var("CI_JOB_TOKEN");
        }
        unsafe { std::env::set_var("CI_API_V4_URL", "https://gitlab.example/api/v4") };
    }

    #[test]
    #[serial_test::serial]
    fn test_auth_prefers_gitlab_token_over_job_token() {
        clear_auth_env();
        unsafe {
            std::env::set_var("GITLAB_TOKEN", "private-value");
            std::env::set_var("CI_JOB_TOKEN", "job-value");
        }
        let api = GitLabApi::from_env("1".to_string()).unwrap();
        assert_eq!(api.auth_header_name, "PRIVATE-TOKEN");
        assert_eq!(api.auth_token, "private-value");
        clear_auth_env();
    }

    #[test]
    #[serial_test::serial]
    fn test_auth_falls_back_to_job_token() {
        clear_auth_env();
        unsafe { std::env::set_var("CI_JOB_TOKEN", "job-value") };
        let api = GitLabApi::from_env("1".to_string()).unwrap();
        assert_eq!(api.auth_header_name, "JOB-TOKEN");
        assert_eq!(api.auth_token, "job-value");
        clear_auth_env();
    }

    #[test]
    #[serial_test::serial]
    fn test_auth_errors_when_no_token_is_set() {
        clear_auth_env();
        assert!(GitLabApi::from_env("1".to_string()).is_err());
    }

    #[test]
    fn test_project_clone_url_reads_http_url_to_repo() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/projects/9")
            .with_status(200)
            .with_body(r#"{"http_url_to_repo": "https://gitlab.example/fork/project.git"}"#)
            .create();

        let url = api_for(&server.url(), "PRIVATE-TOKEN", "tok")
            .project_clone_url(9)
            .unwrap();
        mock.assert();
        assert_eq!(url, "https://gitlab.example/fork/project.git");
    }

    #[test]
    fn test_project_clone_url_surfaces_a_failed_lookup() {
        let mut server = mockito::Server::new();
        let mock = server.mock("GET", "/projects/9").with_status(404).create();

        let err = api_for(&server.url(), "PRIVATE-TOKEN", "tok")
            .project_clone_url(9)
            .unwrap_err();
        mock.assert();
        assert!(err.contains("404"), "{}", err);
    }

    // ---- Clone URL construction ----

    #[test]
    #[serial_test::serial]
    fn test_clone_urls_report_a_missing_push_token() {
        clear_auth_env();
        unsafe { std::env::set_var("CI_JOB_TOKEN", "job-value") };
        let urls = build_ci_clone_urls("https://gitlab.example", "group/project");
        assert!(
            urls.fetch.contains("gitlab-ci-token:job-value@"),
            "{}",
            urls.fetch
        );
        clear_auth_env();
    }

    #[test]
    #[serial_test::serial]
    fn test_clone_urls_use_gitlab_token_for_push() {
        clear_auth_env();
        unsafe {
            std::env::set_var("CI_JOB_TOKEN", "job-value");
            std::env::set_var("GITLAB_TOKEN", "push-value");
        }
        let urls = build_ci_clone_urls("https://gitlab.example", "group/project");
        assert!(urls.push.contains("oauth2:push-value@"), "{}", urls.push);
        assert!(
            urls.fetch.contains("gitlab-ci-token:job-value@"),
            "{}",
            urls.fetch
        );
        clear_auth_env();
    }

    /// The fork URL only gets a token when it is served by this GitLab
    /// instance, so a URL on any other host cannot receive one.
    #[test]
    #[serial_test::serial]
    fn test_authenticate_project_url_ignores_a_foreign_host() {
        clear_auth_env();
        unsafe { std::env::set_var("CI_JOB_TOKEN", "job-value") };
        let foreign = authenticate_project_url(
            "https://evil.example/fork/project.git".to_string(),
            "https://gitlab.example",
        );
        assert_eq!(foreign, "https://evil.example/fork/project.git");

        let lookalike = authenticate_project_url(
            "https://gitlab.example.evil.test/fork/project.git".to_string(),
            "https://gitlab.example",
        );
        assert_eq!(
            lookalike, "https://gitlab.example.evil.test/fork/project.git",
            "a host that merely starts with the server URL must not get the token"
        );

        let own = authenticate_project_url(
            "https://gitlab.example/fork/project.git".to_string(),
            "https://gitlab.example",
        );
        assert!(own.contains("gitlab-ci-token:job-value@"), "{}", own);
        clear_auth_env();
    }

    // ---- Fetching commits by SHA ----

    fn init_repo_with_commit() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        exec_git(&["init".to_string(), "-q".to_string(), path.clone()]).unwrap();
        for (key, value) in [("user.email", "t@example.com"), ("user.name", "T")] {
            exec_git(&[
                "-C".to_string(),
                path.clone(),
                "config".to_string(),
                key.to_string(),
                value.to_string(),
            ])
            .unwrap();
        }
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        exec_git(&[
            "-C".to_string(),
            path.clone(),
            "add".to_string(),
            ".".to_string(),
        ])
        .unwrap();
        exec_git(&[
            "-C".to_string(),
            path.clone(),
            "-c".to_string(),
            "commit.gpgsign=false".to_string(),
            "commit".to_string(),
            "-q".to_string(),
            "-m".to_string(),
            "c".to_string(),
        ])
        .unwrap();
        let out = exec_git(&[
            "-C".to_string(),
            path.clone(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ])
        .unwrap();
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (dir, path, sha)
    }

    #[test]
    fn test_ensure_commit_fetched_is_a_no_op_when_the_commit_is_present() {
        let (_dir, path, sha) = init_repo_with_commit();
        // The remote is unreachable on purpose: a present commit must not be fetched.
        let result = ensure_commit_fetched(
            &path,
            &sha,
            "https://unreachable.invalid/x.git",
            "refs/git-ai/test/present",
        );
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn test_ensure_commit_fetched_error_carries_no_credentials() {
        let (_dir, path, _sha) = init_repo_with_commit();
        let missing = "0123456789012345678901234567890123456789";
        let err = ensure_commit_fetched(
            &path,
            missing,
            "https://gitlab-ci-token:super-secret@unreachable.invalid/x.git",
            "refs/git-ai/test/missing",
        )
        .unwrap_err();
        assert!(!err.to_string().contains("super-secret"), "{}", err);
    }

    #[test]
    fn test_ensure_commit_fetched_reports_a_server_that_will_not_serve_the_sha() {
        let (_dir, path, _sha) = init_repo_with_commit();
        let missing = "0123456789012345678901234567890123456789";
        let result = ensure_commit_fetched(
            &path,
            missing,
            "https://unreachable.invalid/x.git",
            "refs/git-ai/test/missing",
        );
        assert!(
            result.is_err(),
            "unreachable remote should surface an error"
        );
    }

    // ---- Diff version pagination ----

    fn versions_page(shas: &[String]) -> String {
        let entries: Vec<String> = shas
            .iter()
            .map(|sha| {
                format!(
                    r#"{{"head_commit_sha": "{}", "base_commit_sha": "base"}}"#,
                    sha
                )
            })
            .collect();
        format!("[{}]", entries.join(","))
    }

    #[test]
    fn test_mr_versions_walks_every_page() {
        let mut server = mockito::Server::new();
        let first_page: Vec<String> = (0..100).map(|i| format!("head{}", i)).collect();
        let page1 = server
            .mock(
                "GET",
                "/projects/123/merge_requests/7/versions?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(versions_page(&first_page))
            .create();
        let page2 = server
            .mock(
                "GET",
                "/projects/123/merge_requests/7/versions?per_page=100&page=2",
            )
            .with_status(200)
            .with_body(versions_page(&["oldest".to_string()]))
            .create();

        let versions = api_for(&server.url(), "PRIVATE-TOKEN", "tok")
            .mr_versions(7)
            .unwrap();
        page1.assert();
        page2.assert();
        assert_eq!(versions.len(), 101);
        assert_eq!(versions.last().unwrap().head_commit_sha, "oldest");
    }

    #[test]
    fn test_mr_versions_stops_on_the_first_short_page() {
        let mut server = mockito::Server::new();
        let page1 = server
            .mock(
                "GET",
                "/projects/123/merge_requests/7/versions?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(versions_page(&["a".to_string(), "b".to_string()]))
            .expect(1)
            .create();

        let versions = api_for(&server.url(), "PRIVATE-TOKEN", "tok")
            .mr_versions(7)
            .unwrap();
        page1.assert();
        assert_eq!(versions.len(), 2);
    }

    // ---- Rebase recovery: the paths that bail before touching the clone ----
    //
    // The clone directory below does not exist, so these also prove recovery
    // returns before opening a repository.

    #[test]
    fn test_recovery_skips_when_versions_cannot_be_read() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/merge_requests/7/versions?per_page=100&page=1",
            )
            .with_status(500)
            .create();

        recover_rewritten_mr_notes(
            &api_for(&server.url(), "PRIVATE-TOKEN", "tok"),
            RewriteRecovery {
                clone_dir: "/nonexistent/git-ai-recovery-test",
                iid: 7,
                merge_commit_sha: "merge-sha",
                target_branch: "main",
                fetch_url: "https://unreachable.invalid/x.git",
                fork_clone_url: None,
                skip_push: true,
            },
        )
        .unwrap();
        mock.assert();
    }

    #[test]
    fn test_recovery_skips_when_the_branch_was_never_rewritten() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "GET",
                "/projects/123/merge_requests/7/versions?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(r#"[{"head_commit_sha": "only", "base_commit_sha": "base"}]"#)
            .create();

        recover_rewritten_mr_notes(
            &api_for(&server.url(), "PRIVATE-TOKEN", "tok"),
            RewriteRecovery {
                clone_dir: "/nonexistent/git-ai-recovery-test",
                iid: 7,
                merge_commit_sha: "merge-sha",
                target_branch: "main",
                fetch_url: "https://unreachable.invalid/x.git",
                fork_clone_url: None,
                skip_push: true,
            },
        )
        .unwrap();
        mock.assert();
    }
}
