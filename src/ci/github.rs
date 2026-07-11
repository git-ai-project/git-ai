use crate::ci::ci_context::{CiContext, CiEvent};
use crate::error::GitAiError;
use crate::git::repository::exec_git;
use crate::git::repository::find_repository_in_path;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const GITHUB_CI_TEMPLATE_YAML: &str = include_str!("workflow_templates/github.yaml");
const MAX_GITHUB_EVENT_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct GithubCiEventPayload {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    after: Option<String>,
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

    let event_payload = read_github_event_payload(std::path::Path::new(&env_event_path))?;
    if event_payload.pull_request.is_none() {
        return Ok(None);
    }

    let pull_request = event_payload.pull_request.unwrap();

    let pr_number = pull_request.number;
    let head_ref = pull_request.head.ref_name.clone();
    let head_sha = pull_request.head.sha.clone();
    let base_ref = pull_request.base.ref_name.clone();
    let base_sha = pull_request.base.sha.clone();
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
        authenticate_clone_url(&clone_url, &token)
    } else {
        clone_url
    };

    // Authenticate the fork clone URL if this is a fork PR.
    let authenticated_fork_url = fork_clone_url.map(|fork_url| {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            authenticate_clone_url(&fork_url, &token)
        } else {
            fork_url
        }
    });

    if pull_request.merged
        && let Some(merge_commit_sha) = pull_request.merge_commit_sha
    {
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

        let repo = find_repository_in_path(&clone_dir.clone())?;

        return Ok(Some(CiContext {
            repo,
            event: CiEvent::Merge {
                merge_commit_sha,
                head_ref: head_ref.clone(),
                head_sha: head_sha.clone(),
                base_ref: base_ref.clone(),
                base_sha,
                fork_clone_url: authenticated_fork_url,
            },
            temp_dir: PathBuf::from(clone_dir),
        }));
    }

    if event_payload.action.as_deref() != Some("synchronize") {
        return Ok(None);
    }

    let previous_head_sha = match event_payload.before.as_deref() {
        Some(before)
            if !before.is_empty() && before != "0000000000000000000000000000000000000000" =>
        {
            before.to_string()
        }
        _ => return Ok(None),
    };
    let current_head_sha = event_payload
        .after
        .clone()
        .filter(|sha| !sha.is_empty() && sha != "0000000000000000000000000000000000000000")
        .unwrap_or(head_sha.clone());

    // Clone the base branch at its current tip and fetch both the current PR
    // head and the previous head from the synchronize payload. For a normal PR
    // push, the previous head is already reachable from the current PR ref. For
    // a non-fast-forward UI rebase, fetching by SHA keeps the old commits
    // available long enough for the local rebase rewrite command.
    exec_git(&[
        "clone".to_string(),
        "--branch".to_string(),
        base_ref.clone(),
        authenticated_url.clone(),
        clone_dir.clone(),
    ])?;

    exec_git(&[
        "-C".to_string(),
        clone_dir.clone(),
        "fetch".to_string(),
        authenticated_url.clone(),
        format!("pull/{}/head:refs/github/pr/{}", pr_number, pr_number),
    ])?;

    let previous_head_is_local = exec_git(&[
        "-C".to_string(),
        clone_dir.clone(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        format!("{}^{{commit}}", previous_head_sha),
    ])
    .is_ok();

    if !previous_head_is_local {
        let previous_head_fetch_url = authenticated_fork_url
            .as_ref()
            .unwrap_or(&authenticated_url);
        let previous_head_ref = format!("refs/git-ai/github/pr/{}/previous-head", pr_number);
        exec_git(&[
            "-C".to_string(),
            clone_dir.clone(),
            "fetch".to_string(),
            previous_head_fetch_url.clone(),
            previous_head_sha.clone(),
        ])?;
        exec_git(&[
            "-C".to_string(),
            clone_dir.clone(),
            "update-ref".to_string(),
            previous_head_ref,
            previous_head_sha.clone(),
        ])?;
    }

    let repo = find_repository_in_path(&clone_dir.clone())?;

    Ok(Some(CiContext {
        repo,
        event: CiEvent::Sync {
            previous_head_sha,
            head_sha: current_head_sha,
            base_ref,
            base_sha,
            previous_base_sha: None,
            previous_head_fetch_remote: Some(
                authenticated_fork_url
                    .as_ref()
                    .unwrap_or(&authenticated_url)
                    .clone(),
            ),
        },
        temp_dir: PathBuf::from(clone_dir),
    }))
}

fn read_github_event_payload(path: &std::path::Path) -> Result<GithubCiEventPayload, GitAiError> {
    let content = crate::utils::read_text_file_with_limit(
        path,
        MAX_GITHUB_EVENT_PAYLOAD_BYTES,
        "GitHub event payload",
    )?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn authenticate_clone_url(clone_url: &str, token: &str) -> String {
    format!(
        "https://x-access-token:{}@{}",
        token,
        clone_url.strip_prefix("https://").unwrap_or(clone_url)
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authenticate_clone_url_supports_github_dot_com() {
        assert_eq!(
            authenticate_clone_url("https://github.com/acme/repo.git", "token"),
            "https://x-access-token:token@github.com/acme/repo.git"
        );
    }

    #[test]
    fn test_authenticate_clone_url_supports_enterprise_hosts() {
        assert_eq!(
            authenticate_clone_url("https://github.example.com/acme/repo.git", "token"),
            "https://x-access-token:token@github.example.com/acme/repo.git"
        );
    }

    #[test]
    fn test_github_synchronize_payload_deserializes_before_after() {
        let json = r#"{
            "action": "synchronize",
            "before": "1111111111111111111111111111111111111111",
            "after": "2222222222222222222222222222222222222222",
            "pull_request": {
                "number": 42,
                "base": {
                    "ref": "main",
                    "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "repo": { "clone_url": "https://github.com/org/repo.git" }
                },
                "head": {
                    "ref": "feature",
                    "sha": "2222222222222222222222222222222222222222",
                    "repo": { "clone_url": "https://github.com/fork/repo.git" }
                },
                "merged": false,
                "merge_commit_sha": null
            }
        }"#;

        let payload: GithubCiEventPayload = serde_json::from_str(json).unwrap();

        assert_eq!(payload.action.as_deref(), Some("synchronize"));
        assert_eq!(
            payload.before.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(
            payload.after.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        let pull_request = payload.pull_request.expect("pull_request");
        assert_eq!(pull_request.number, 42);
        assert!(!pull_request.merged);
        assert_eq!(pull_request.base.ref_name, "main");
        assert_eq!(pull_request.head.ref_name, "feature");
    }

    #[test]
    fn oversized_github_event_payload_is_rejected_before_parsing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("event.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_GITHUB_EVENT_PAYLOAD_BYTES + 1).unwrap();

        let error = read_github_event_payload(&path).expect_err("payload must be rejected");
        assert!(error.to_string().contains("byte limit"));
    }
}
