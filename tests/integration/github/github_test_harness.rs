use crate::repos::test_repo::TestRepo;
use std::process::Command;
use std::sync::OnceLock;

/// Merge strategy for pull requests
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum MergeStrategy {
    /// Squash all commits into one
    Squash,
    /// Create a merge commit
    Merge,
    /// Rebase and merge
    Rebase,
}

static GH_CLI_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Check if GitHub CLI is available and authenticated
pub fn is_gh_cli_available() -> bool {
    *GH_CLI_AVAILABLE.get_or_init(|| {
        let version_check = Command::new("gh").arg("--version").output();

        if version_check.is_err() || !version_check.unwrap().status.success() {
            return false;
        }

        let auth_check = Command::new("gh").args(["auth", "status"]).output();

        auth_check.is_ok() && auth_check.unwrap().status.success()
    })
}

/// GitHub test repository wrapper that extends TestRepo with GitHub operations
pub struct GitHubTestRepo {
    pub repo: TestRepo,
    pub github_repo_name: String,
    pub github_owner: String,
}

impl GitHubTestRepo {
    /// Create a new GitHub test repository with a name derived from the test
    /// Returns None if gh CLI is not available
    pub fn new(test_name: &str) -> Option<Self> {
        if !is_gh_cli_available() {
            println!("⏭️  Skipping GitHub test - gh CLI not available or not authenticated");
            return None;
        }

        let repo = TestRepo::new();
        let repo_name = generate_repo_name(test_name);

        let owner = match get_authenticated_user() {
            Some(user) => user,
            None => {
                println!("⏭️  Skipping GitHub test - could not get authenticated user");
                return None;
            }
        };

        Some(Self {
            repo,
            github_repo_name: repo_name,
            github_owner: owner,
        })
    }

    /// Initialize the repository and create it on GitHub
    pub fn create_on_github(&self) -> Result<(), String> {
        let repo_path = self.repo.path();

        // Create initial commit (required for gh repo create)
        std::fs::write(repo_path.join("README.md"), "# GitHub Test Repository\n")
            .map_err(|e| format!("Failed to create README: {}", e))?;

        self.repo
            .git(&["add", "."])
            .map_err(|e| format!("Failed to add files: {}", e))?;

        self.repo
            .git(&["commit", "-m", "Initial commit"])
            .map_err(|e| format!("Failed to create initial commit: {}", e))?;

        // Create GitHub repository
        let output = Command::new("gh")
            .args([
                "repo",
                "create",
                &self.github_repo_name,
                "--public",
                "--source",
                repo_path.to_str().unwrap(),
                "--push",
            ])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to execute gh repo create: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to create GitHub repository:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        println!(
            "✅ Created GitHub repository: {}/{}",
            self.github_owner, self.github_repo_name
        );
        Ok(())
    }

    /// Create a new branch
    pub fn create_branch(&self, branch_name: &str) -> Result<(), String> {
        self.repo.git(&["checkout", "-b", branch_name]).map(|_| ())
    }

    /// Push current branch to GitHub
    pub fn push_branch(&self, branch_name: &str) -> Result<(), String> {
        self.repo
            .git(&["push", "--set-upstream", "origin", branch_name])
            .map(|_| ())
    }

    /// Create a pull request
    pub fn create_pr(&self, title: &str, body: &str) -> Result<String, String> {
        let repo_path = self.repo.path();

        let output = Command::new("gh")
            .args(["pr", "create", "--title", title, "--body", body])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to execute gh pr create: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to create PR:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let pr_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        println!("✅ Created pull request: {}", pr_url);
        Ok(pr_url)
    }

    /// Merge a pull request with the specified strategy
    pub fn merge_pr(&self, pr_number: &str, strategy: MergeStrategy) -> Result<(), String> {
        let repo_path = self.repo.path();

        let strategy_flag = match strategy {
            MergeStrategy::Squash => "--squash",
            MergeStrategy::Merge => "--merge",
            MergeStrategy::Rebase => "--rebase",
        };

        let output = Command::new("gh")
            .args(["pr", "merge", pr_number, strategy_flag, "--delete-branch"])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to execute gh pr merge: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to merge PR:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        println!(
            "✅ Merged pull request #{} using {:?} strategy",
            pr_number, strategy
        );
        Ok(())
    }

    /// Get the PR number from a PR URL
    pub fn extract_pr_number(&self, pr_url: &str) -> Option<String> {
        pr_url.split('/').next_back().map(|s| s.to_string())
    }

    /// Get the default branch name from the remote repository
    pub fn get_default_branch(&self) -> Result<String, String> {
        let repo_path = self.repo.path();
        let full_repo = format!("{}/{}", self.github_owner, self.github_repo_name);

        let output = Command::new("gh")
            .args([
                "repo",
                "view",
                &full_repo,
                "--json",
                "defaultBranchRef",
                "--jq",
                ".defaultBranchRef.name",
            ])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to get default branch: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to get default branch:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Install the GitHub CI workflow in the repository
    pub fn install_github_ci_workflow(&self) -> Result<(), String> {
        // Use git-ai to install the workflow
        let output = self
            .repo
            .git_ai(&["ci", "github", "install"])
            .map_err(|e| format!("Failed to install CI workflow: {}", e))?;

        println!("✅ Installed GitHub CI workflow");
        println!("{}", output);

        // Commit and push the workflow file
        self.repo
            .git(&["add", ".github/workflows/git-ai.yaml"])
            .map_err(|e| format!("Failed to add workflow file: {}", e))?;

        self.repo
            .git(&["commit", "-m", "Add git-ai CI workflow"])
            .map_err(|e| format!("Failed to commit workflow: {}", e))?;

        self.repo
            .git(&["push"])
            .map_err(|e| format!("Failed to push workflow: {}", e))?;

        println!("✅ Committed and pushed CI workflow");
        Ok(())
    }

    /// Get the logs for a specific workflow run
    pub fn get_workflow_logs(&self, run_id: &str) -> Result<String, String> {
        let repo_path = self.repo.path();
        let full_repo = format!("{}/{}", self.github_owner, self.github_repo_name);

        let output = Command::new("gh")
            .args(["run", "view", run_id, "--repo", &full_repo, "--log"])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to get workflow logs: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to get workflow logs:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Wait for GitHub Actions workflow runs to complete for a specific PR
    /// Returns an error if any workflow fails
    pub fn wait_for_workflows(&self, timeout_seconds: u64) -> Result<(), String> {
        let repo_path = self.repo.path();
        let full_repo = format!("{}/{}", self.github_owner, self.github_repo_name);

        println!(
            "⏳ Waiting for GitHub Actions workflows to complete (timeout: {}s)...",
            timeout_seconds
        );

        use std::time::{Duration, Instant};
        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_seconds);

        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "Timeout waiting for workflows to complete after {}s",
                    timeout_seconds
                ));
            }

            // Get all workflow runs for the repository
            let output = Command::new("gh")
                .args([
                    "run",
                    "list",
                    "--repo",
                    &full_repo,
                    "--json",
                    "status,conclusion,name,databaseId",
                    "--limit",
                    "10",
                ])
                .current_dir(repo_path)
                .output()
                .map_err(|e| format!("Failed to list workflow runs: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "Failed to list workflow runs:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let runs_json = String::from_utf8_lossy(&output.stdout);
            let runs: Vec<serde_json::Value> = serde_json::from_str(&runs_json)
                .map_err(|e| format!("Failed to parse workflow runs JSON: {}", e))?;

            // Check if there are any runs
            if runs.is_empty() {
                println!("   No workflow runs found yet, waiting...");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }

            // Check status of all runs
            let mut all_completed = true;
            let mut any_failed = false;
            let mut failed_run_ids = Vec::new();

            for run in &runs {
                let status = run["status"].as_str().unwrap_or("unknown");
                let name = run["name"].as_str().unwrap_or("unknown");
                let run_id = run["databaseId"].as_u64().unwrap_or(0);

                if status != "completed" {
                    all_completed = false;
                    println!("   Workflow '{}': {}", name, status);
                }

                if status == "completed" {
                    let conclusion = run["conclusion"].as_str().unwrap_or("unknown");
                    if conclusion != "success" {
                        any_failed = true;
                        failed_run_ids.push(run_id.to_string());
                        println!(
                            "   ❌ Workflow '{}' failed with conclusion: {}",
                            name, conclusion
                        );
                    } else {
                        println!("   ✅ Workflow '{}' completed successfully", name);
                    }
                }
            }

            if all_completed {
                if any_failed {
                    // Fetch and display logs for failed workflows
                    for run_id in &failed_run_ids {
                        println!("\n📋 Logs for failed workflow run {}:", run_id);
                        match self.get_workflow_logs(run_id) {
                            Ok(logs) => {
                                // Print last 100 lines of logs
                                let lines: Vec<&str> = logs.lines().collect();
                                let start_line = if lines.len() > 100 {
                                    lines.len() - 100
                                } else {
                                    0
                                };
                                for line in &lines[start_line..] {
                                    println!("{}", line);
                                }
                            }
                            Err(e) => println!("Failed to fetch logs: {}", e),
                        }
                    }
                    return Err("One or more workflows failed".to_string());
                }
                println!("✅ All workflows completed successfully");
                return Ok(());
            }

            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Checkout default branch and pull latest changes from remote
    pub fn checkout_and_pull_default_branch(&self) -> Result<(), String> {
        let default_branch = self.get_default_branch()?;
        self.repo.git(&["checkout", &default_branch])?;
        self.repo.git(&["pull", "origin", &default_branch])?;
        println!("✅ Checked out and pulled latest {} branch", default_branch);
        Ok(())
    }

    /// Delete the GitHub repository
    pub fn delete_from_github(&self) -> Result<(), String> {
        let full_repo = format!("{}/{}", self.github_owner, self.github_repo_name);

        let output = Command::new("gh")
            .args(["repo", "delete", &full_repo, "--yes"])
            .output()
            .map_err(|e| format!("Failed to execute gh repo delete: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to delete GitHub repository:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        println!("✅ Deleted GitHub repository: {}", full_repo);
        Ok(())
    }
}

impl Drop for GitHubTestRepo {
    fn drop(&mut self) {
        if std::env::var("GIT_AI_TEST_NO_CLEANUP").is_ok() {
            eprintln!(
                "⚠️  Cleanup disabled - repository preserved: {}/{}",
                self.github_owner, self.github_repo_name
            );
            eprintln!(
                "   URL: https://github.com/{}/{}",
                self.github_owner, self.github_repo_name
            );
            return;
        }

        if let Err(e) = self.delete_from_github() {
            eprintln!("⚠️  Failed to cleanup GitHub repository: {}", e);
            eprintln!(
                "   Manual cleanup required: {}/{}",
                self.github_owner, self.github_repo_name
            );
        }
    }
}

/// Generate a unique repository name for testing based on test name
fn generate_repo_name(test_name: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Sanitize test name: lowercase, replace special chars with hyphens
    let sanitized_name = test_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();

    // Truncate if too long (GitHub has a 100 char limit for repo names)
    let max_name_len = 50;
    let truncated_name = if sanitized_name.len() > max_name_len {
        &sanitized_name[..max_name_len]
    } else {
        &sanitized_name
    };

    format!("git-ai-{}-{}", truncated_name, timestamp)
}

/// Get the authenticated GitHub user
fn get_authenticated_user() -> Option<String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
