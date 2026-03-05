use crate::ci::ci_context::{CiContext, CiEvent};
use crate::error::GitAiError;
use crate::git::repository::exec_git;
use crate::git::repository::find_repository_in_path;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const GITHUB_CI_TEMPLATE_YAML: &str = include_str!("workflow_templates/github.yaml");

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct GithubCiEventPayload {
    #[serde(default)]
    pull_request: Option<GithubCiPullRequest>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct GithubCiPullRequest {
    number: u32,
    base: GithubCiPullRequestReference,
    head: GithubCiPullRequestReference,
    merged: bool,
    merge_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct GithubCiPullRequestReference {
    #[serde(rename = "ref")]
    ref_name: String,
    sha: String,
    repo: GithubCiRepository,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct GithubCiRepository {
    clone_url: String,
}

pub fn get_github_ci_context() -> Result<Option<CiContext>, GitAiError> {
    let env_event_name = std::env::var("GITHUB_EVENT_NAME").unwrap_or_default();
    let env_event_path = std::env::var("GITHUB_EVENT_PATH").unwrap_or_default();

    if env_event_name != "pull_request" {
        return Ok(None);
    }

    let event_payload =
        serde_json::from_str::<GithubCiEventPayload>(&std::fs::read_to_string(env_event_path)?)
            .unwrap_or_default();
    if event_payload.pull_request.is_none() {
        return Ok(None);
    }

    let pull_request = event_payload.pull_request.unwrap();

    if !pull_request.merged || pull_request.merge_commit_sha.is_none() {
        return Ok(None);
    }

    let pr_number = pull_request.number;
    let head_ref = pull_request.head.ref_name;
    let head_sha = pull_request.head.sha;
    let base_ref = pull_request.base.ref_name;
    let clone_url = pull_request.base.repo.clone_url.clone();

    // Detect fork: if head repo URL differs from base repo URL, this is a fork PR
    let fork_clone_url = if pull_request.head.repo.clone_url != pull_request.base.repo.clone_url {
        let fork_url = pull_request.head.repo.clone_url.clone();
        println!(
            "Detected fork PR: head repo {} differs from base repo {}",
            fork_url, clone_url
        );
        Some(fork_url)
    } else {
        None
    };

    let clone_dir = "git-ai-ci-clone".to_string();

    // Authenticate the clone URL with GITHUB_TOKEN if available
    let authenticated_url = if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        // Replace https://github.com/ with https://x-access-token:TOKEN@github.com/
        // Supports both public and enterprise github instances.
        format!(
            "https://x-access-token:{}@{}",
            token,
            clone_url.strip_prefix("https://").unwrap_or(&clone_url)
        )
    } else {
        clone_url
    };

    // Clone the repo
    exec_git(&[
        "clone".to_string(),
        "--branch".to_string(),
        base_ref.clone(),
        authenticated_url.clone(),
        clone_dir.clone(),
    ])?;

    // Fetch PR commits using GitHub's special PR refs
    // This is necessary because the PR branch may be deleted after merge
    // but GitHub keeps the commits accessible via pull/{number}/head
    // We store the fetched commits in a local ref to ensure they're kept
    exec_git(&[
        "-C".to_string(),
        clone_dir.clone(),
        "fetch".to_string(),
        authenticated_url.clone(),
        format!("pull/{}/head:refs/github/pr/{}", pr_number, pr_number),
    ])?;

    // Authenticate the fork clone URL if this is a fork PR
    let authenticated_fork_url = fork_clone_url.map(|fork_url| {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            fork_url.replace(
                "https://github.com/",
                &format!("https://x-access-token:{}@github.com/", token),
            )
        } else {
            fork_url
        }
    });

    let repo = find_repository_in_path(&clone_dir.clone())?;

    Ok(Some(CiContext {
        repo,
        event: CiEvent::Merge {
            merge_commit_sha: pull_request.merge_commit_sha.unwrap(),
            head_ref: head_ref.clone(),
            head_sha: head_sha.clone(),
            base_ref: base_ref.clone(),
            base_sha: pull_request.base.sha.clone(),
            fork_clone_url: authenticated_fork_url,
        },
        temp_dir: PathBuf::from(clone_dir),
    }))
}

/// Install or update the GitHub Actions workflow in the current repository
/// Writes the embedded template to .github/workflows/git-ai.yaml at the repo root
pub fn install_github_ci_workflow() -> Result<PathBuf, GitAiError> {
    // Discover repository at current working directory
    let repo = find_repository_in_path(".")?;
    let workdir = repo.workdir()?;

    // Ensure destination directory exists
    let workflows_dir = workdir.join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir)
        .map_err(|e| GitAiError::Generic(format!("Failed to create workflows dir: {}", e)))?;

    // Write template
    let dest_path = workflows_dir.join("git-ai.yaml");
    fs::write(&dest_path, GITHUB_CI_TEMPLATE_YAML)
        .map_err(|e| GitAiError::Generic(format!("Failed to write workflow file: {}", e)))?;

    Ok(dest_path)
}
