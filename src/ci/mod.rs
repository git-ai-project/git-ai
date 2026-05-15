//! CI integration module for git-ai.
//!
//! Detects CI environments (GitHub Actions, GitLab CI), computes attribution
//! reports for pull request diffs, and optionally posts PR comments.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::core::authorship_log::{AuthorshipLog, LineRange};

// ---------------------------------------------------------------------------
// CI Environment Detection
// ---------------------------------------------------------------------------

/// The CI provider that is running the current build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiProvider {
    GitHubActions,
    GitLabCi,
    Unknown,
}

/// Context about the CI environment: provider, repository, PR, and commit info.
#[derive(Debug, Clone)]
pub struct CiContext {
    pub provider: CiProvider,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: Option<u64>,
    pub commit_sha: String,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
}

/// Detect the current CI environment from environment variables.
///
/// Returns `None` if we are not running inside a recognized CI system.
pub fn detect_ci() -> Option<CiContext> {
    if let Some(ctx) = detect_github_actions() {
        return Some(ctx);
    }
    if let Some(ctx) = detect_gitlab_ci() {
        return Some(ctx);
    }
    None
}

fn detect_github_actions() -> Option<CiContext> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return None;
    }

    let repository = std::env::var("GITHUB_REPOSITORY").unwrap_or_default();
    let (owner, name) = split_owner_repo(&repository);
    let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_default();
    let github_ref = std::env::var("GITHUB_REF").unwrap_or_default();
    let base_ref = std::env::var("GITHUB_BASE_REF")
        .ok()
        .filter(|s| !s.is_empty());
    let head_ref = std::env::var("GITHUB_HEAD_REF")
        .ok()
        .filter(|s| !s.is_empty());

    // Try to extract PR number from GITHUB_REF (refs/pull/123/merge)
    let mut pr_number = parse_pr_number_from_ref(&github_ref);

    // Fallback: try to read the event payload JSON
    if pr_number.is_none()
        && let Ok(event_path) = std::env::var("GITHUB_EVENT_PATH")
    {
        pr_number = parse_pr_number_from_event_file(&event_path);
    }

    Some(CiContext {
        provider: CiProvider::GitHubActions,
        repo_owner: owner,
        repo_name: name,
        pr_number,
        commit_sha,
        base_ref,
        head_ref,
    })
}

fn detect_gitlab_ci() -> Option<CiContext> {
    if std::env::var("GITLAB_CI").as_deref() != Ok("true") {
        return None;
    }

    let project_path = std::env::var("CI_PROJECT_PATH").unwrap_or_default();
    let (owner, name) = split_owner_repo(&project_path);
    let commit_sha = std::env::var("CI_COMMIT_SHA").unwrap_or_default();
    let pr_number = std::env::var("CI_MERGE_REQUEST_IID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let base_ref = std::env::var("CI_MERGE_REQUEST_TARGET_BRANCH_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let head_ref = std::env::var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME")
        .ok()
        .filter(|s| !s.is_empty());

    Some(CiContext {
        provider: CiProvider::GitLabCi,
        repo_owner: owner,
        repo_name: name,
        pr_number,
        commit_sha,
        base_ref,
        head_ref,
    })
}

/// Split "owner/repo" into (owner, repo). Handles nested GitLab groups by using
/// the first path segment as owner and the rest as repo name.
fn split_owner_repo(s: &str) -> (String, String) {
    if let Some(slash_pos) = s.find('/') {
        (s[..slash_pos].to_string(), s[slash_pos + 1..].to_string())
    } else {
        (String::new(), s.to_string())
    }
}

/// Parse PR number from a GitHub ref like "refs/pull/123/merge".
fn parse_pr_number_from_ref(github_ref: &str) -> Option<u64> {
    let parts: Vec<&str> = github_ref.split('/').collect();
    // Expected: ["refs", "pull", "123", "merge"]
    if parts.len() >= 4 && parts[0] == "refs" && parts[1] == "pull" {
        parts[2].parse::<u64>().ok()
    } else {
        None
    }
}

/// Parse PR number from the GitHub event JSON file (e.g., pull_request.number).
fn parse_pr_number_from_event_file(path: &str) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    // pull_request events have .pull_request.number or .number at top level
    value
        .get("pull_request")
        .and_then(|pr| pr.get("number"))
        .and_then(|n| n.as_u64())
        .or_else(|| value.get("number").and_then(|n| n.as_u64()))
}

// ---------------------------------------------------------------------------
// Attribution Report
// ---------------------------------------------------------------------------

/// Per-file attribution breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttribution {
    pub path: String,
    pub ai_lines: usize,
    pub human_lines: usize,
    pub untracked_lines: usize,
}

/// Aggregate attribution report across all files in a PR diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionReport {
    pub total_lines: usize,
    pub ai_lines: usize,
    pub human_lines: usize,
    pub untracked_lines: usize,
    pub files: Vec<FileAttribution>,
}

/// Compute an attribution report for the diff between `base` and `head`.
///
/// `repo_path` must point to the root of a git repository. This function shells
/// out to git to determine changed files, parse diffs, and read authorship notes.
pub fn compute_report(
    repo_path: &Path,
    base: &str,
    head: &str,
) -> Result<AttributionReport, String> {
    let range = format!("{}..{}", base, head);

    // 1. Get list of changed files
    let changed_files = git_in(repo_path, &["diff", "--name-only", &range])?;
    let file_paths: Vec<&str> = changed_files.lines().filter(|l| !l.is_empty()).collect();

    if file_paths.is_empty() {
        return Ok(AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        });
    }

    // 2. Get the added/modified line numbers per file from the diff
    let diff_output = git_in(repo_path, &["diff", "-U0", &range])?;
    let added_lines_by_file = parse_diff_added_lines(&diff_output);

    // 3. Read authorship note for the head commit
    let note_content = git_in(repo_path, &["notes", "--ref=ai", "show", head]);
    let authorship = note_content
        .ok()
        .and_then(|s| AuthorshipLog::deserialize_from_string(&s).ok());

    // Build a lookup: file_path -> { hash -> set of line numbers }
    let mut file_attestations: HashMap<&str, Vec<(&str, Vec<u32>)>> = HashMap::new();
    if let Some(ref log) = authorship {
        for file_att in &log.attestations {
            let mut entries_expanded = Vec::new();
            for entry in &file_att.entries {
                let lines: Vec<u32> = entry
                    .line_ranges
                    .iter()
                    .flat_map(LineRange::expand)
                    .collect();
                entries_expanded.push((entry.hash.as_str(), lines));
            }
            file_attestations.insert(&file_att.file_path, entries_expanded);
        }
    }

    // Determine which hashes are AI (prompt/session) vs human
    let (ai_hashes, human_hashes) = authorship
        .as_ref()
        .map(|log| classify_hashes(log))
        .unwrap_or_default();

    // 4. For each file, classify the added lines
    let mut report = AttributionReport {
        total_lines: 0,
        ai_lines: 0,
        human_lines: 0,
        untracked_lines: 0,
        files: Vec::new(),
    };

    for file_path in &file_paths {
        let added_lines = match added_lines_by_file.get(*file_path) {
            Some(lines) => lines,
            None => continue,
        };

        if added_lines.is_empty() {
            continue;
        }

        let mut file_ai = 0usize;
        let mut file_human = 0usize;
        let mut file_untracked = 0usize;

        let attestation_entries = file_attestations.get(*file_path);

        for &line_num in added_lines {
            let mut classified = false;

            if let Some(entries) = attestation_entries {
                for (hash, lines) in entries {
                    if lines.contains(&line_num) {
                        if ai_hashes.contains(hash) {
                            file_ai += 1;
                        } else if human_hashes.contains(hash) {
                            file_human += 1;
                        } else {
                            file_untracked += 1;
                        }
                        classified = true;
                        break;
                    }
                }
            }

            if !classified {
                file_untracked += 1;
            }
        }

        report.files.push(FileAttribution {
            path: file_path.to_string(),
            ai_lines: file_ai,
            human_lines: file_human,
            untracked_lines: file_untracked,
        });

        report.ai_lines += file_ai;
        report.human_lines += file_human;
        report.untracked_lines += file_untracked;
    }

    report.total_lines = report.ai_lines + report.human_lines + report.untracked_lines;
    Ok(report)
}

/// Parse unified diff output (with -U0) to extract which line numbers were added
/// in each file. Returns a map of file_path -> sorted vec of added line numbers.
fn parse_diff_added_lines(diff: &str) -> HashMap<String, Vec<u32>> {
    let mut result: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = Some(path.to_string());
        } else if line.starts_with("@@ ") {
            // Parse hunk header like "@@ -a,b +c,d @@" -- we want +c,d (new file side)
            if let Some(ref file) = current_file
                && let Some((start, count)) = parse_hunk_header_new_side(line)
            {
                let lines = result.entry(file.clone()).or_default();
                for i in 0..count {
                    lines.push(start + i);
                }
            }
        }
    }

    result
}

/// Parse the new-side ('+' side) of a hunk header.
/// Handles: "@@ -a,b +c,d @@", "@@ -a +c @@", "@@ -a,b +c @@"
fn parse_hunk_header_new_side(line: &str) -> Option<(u32, u32)> {
    let plus_idx = line.find(" +")?;
    let after_plus = &line[plus_idx + 2..];
    let end_idx = after_plus.find(' ').unwrap_or(after_plus.len());
    let range_str = &after_plus[..end_idx];

    if let Some(comma_idx) = range_str.find(',') {
        let start: u32 = range_str[..comma_idx].parse().ok()?;
        let count: u32 = range_str[comma_idx + 1..].parse().ok()?;
        Some((start, count))
    } else {
        let start: u32 = range_str.parse().ok()?;
        // No comma means a single line was added
        Some((start, 1))
    }
}

// ---------------------------------------------------------------------------
// Report Formatting
// ---------------------------------------------------------------------------

/// Format an attribution report as a markdown table suitable for a PR comment.
///
/// The output includes a hidden HTML marker (`<!-- git-ai-attribution -->`) used
/// to identify and update existing comments.
pub fn format_markdown_report(report: &AttributionReport) -> String {
    let mut out = String::new();

    out.push_str("## AI Attribution Report\n");
    out.push_str(COMMENT_MARKER);
    out.push('\n');
    out.push('\n');

    // Summary table
    out.push_str("| Metric | Value |\n");
    out.push_str("|--------|-------|\n");

    let total = report.total_lines;
    let ai_pct = percentage_f64(report.ai_lines, total);

    out.push_str(&format!("| AI Lines | {} |\n", report.ai_lines));
    out.push_str(&format!("| Human Lines | {} |\n", report.human_lines));
    out.push_str(&format!("| AI % | {:.1}% |\n", ai_pct));

    // Per-file breakdown (only if there are files)
    if !report.files.is_empty() {
        out.push_str("\n### Per-file breakdown\n\n");
        out.push_str("| File | AI % | AI Lines | Total Lines |\n");
        out.push_str("|------|------|----------|-------------|\n");

        for file in &report.files {
            let file_total = file.ai_lines + file.human_lines + file.untracked_lines;
            let file_ai_pct = percentage(file.ai_lines, file_total);
            out.push_str(&format!(
                "| {} | {}% | {} | {} |\n",
                file.path, file_ai_pct, file.ai_lines, file_total
            ));
        }
    }

    out
}

/// Legacy format function (delegates to format_markdown_report).
pub fn format_markdown(report: &AttributionReport) -> String {
    format_markdown_report(report)
}

fn percentage_f64(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

fn percentage(part: usize, total: usize) -> usize {
    (part * 100 + total / 2)
        .checked_div(total)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// GitHub PR Comment
// ---------------------------------------------------------------------------

/// Hidden HTML marker used to identify git-ai comments for upsert behavior.
const COMMENT_MARKER: &str = "<!-- git-ai-attribution -->";

/// Post (or update) a report comment on a GitHub PR using the `gh` CLI.
///
/// If a comment from git-ai already exists (identified by `COMMENT_MARKER`),
/// it is updated in place. Otherwise a new comment is created.
///
/// Requires `GITHUB_TOKEN` env var (already available in GitHub Actions) and
/// the `gh` CLI to be installed.
pub fn post_github_comment(report: &AttributionReport, env: &CiContext) -> Result<(), String> {
    let pr_number = env
        .pr_number
        .ok_or_else(|| "no PR number available in CI context".to_string())?;

    let repo_slug = format!("{}/{}", env.repo_owner, env.repo_name);
    let body = format_markdown_report(report);

    // Check GITHUB_TOKEN
    if std::env::var("GITHUB_TOKEN").is_err() {
        return Err("GITHUB_TOKEN environment variable is not set".to_string());
    }

    // Check if `gh` is available
    let gh_available = Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !gh_available {
        return Err("gh CLI is not installed or not in PATH".to_string());
    }

    // Try to find an existing comment with our marker
    let existing_comment_id = find_existing_github_comment(&repo_slug, pr_number)?;

    let (method, endpoint) = if let Some(comment_id) = existing_comment_id {
        (
            "PATCH",
            format!("repos/{}/issues/comments/{}", repo_slug, comment_id),
        )
    } else {
        (
            "POST",
            format!("repos/{}/issues/{}/comments", repo_slug, pr_number),
        )
    };

    let json_body = serde_json::json!({ "body": body }).to_string();
    let output = Command::new("gh")
        .args(["api", "--method", method, &endpoint, "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(json_body.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(|e| format!("failed to run gh api: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api {} failed: {}", method, stderr.trim()));
    }

    Ok(())
}

/// Search for an existing PR comment containing our marker.
/// Returns the comment ID if found, or None.
fn find_existing_github_comment(repo_slug: &str, pr_number: u64) -> Result<Option<u64>, String> {
    let endpoint = format!(
        "repos/{}/issues/{}/comments?per_page=100",
        repo_slug, pr_number
    );
    let output = Command::new("gh")
        .args(["api", &endpoint])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run gh api: {}", e))?;

    if !output.status.success() {
        // If we can't list comments, just create a new one
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let comments: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse comments JSON: {}", e))?;

    for comment in &comments {
        if let Some(body) = comment.get("body").and_then(|b| b.as_str())
            && body.contains(COMMENT_MARKER)
            && let Some(id) = comment.get("id").and_then(|i| i.as_u64())
        {
            return Ok(Some(id));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// GitLab MR Comment
// ---------------------------------------------------------------------------

/// Post (or update) a report note on a GitLab merge request using `curl`.
///
/// Uses `CI_JOB_TOKEN` for authentication and the GitLab API v4 endpoint.
/// If an existing note with our marker is found, it is updated. Otherwise a
/// new note is created.
pub fn post_gitlab_comment(report: &AttributionReport, env: &CiContext) -> Result<(), String> {
    let mr_iid = env
        .pr_number
        .ok_or_else(|| "no merge request IID available in CI context".to_string())?;

    let api_url = std::env::var("CI_API_V4_URL")
        .map_err(|_| "CI_API_V4_URL environment variable is not set".to_string())?;
    let project_id = std::env::var("CI_PROJECT_ID")
        .map_err(|_| "CI_PROJECT_ID environment variable is not set".to_string())?;
    let job_token = std::env::var("CI_JOB_TOKEN")
        .map_err(|_| "CI_JOB_TOKEN environment variable is not set".to_string())?;

    let body = format_markdown_report(report);

    // Check if curl is available
    let curl_available = Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !curl_available {
        return Err("curl is not installed or not in PATH".to_string());
    }

    // Try to find existing note with our marker
    let existing_note_id = find_existing_gitlab_note(&api_url, &project_id, mr_iid, &job_token)?;

    let (method, url) = if let Some(note_id) = existing_note_id {
        (
            "PUT",
            format!(
                "{}/projects/{}/merge_requests/{}/notes/{}",
                api_url, project_id, mr_iid, note_id
            ),
        )
    } else {
        (
            "POST",
            format!(
                "{}/projects/{}/merge_requests/{}/notes",
                api_url, project_id, mr_iid
            ),
        )
    };

    let token_header = format!("JOB-TOKEN: {}", job_token);
    let json_data = serde_json::json!({ "body": body }).to_string();
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--request",
            method,
            "--header",
            &token_header,
            "--header",
            "Content-Type: application/json",
            "--data",
            &json_data,
            &url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run curl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl {} failed: {}", method, stderr.trim()));
    }

    Ok(())
}

/// Search for an existing MR note containing our marker.
fn find_existing_gitlab_note(
    api_url: &str,
    project_id: &str,
    mr_iid: u64,
    job_token: &str,
) -> Result<Option<u64>, String> {
    let url = format!(
        "{}/projects/{}/merge_requests/{}/notes?per_page=100",
        api_url, project_id, mr_iid
    );
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--header",
            &format!("JOB-TOKEN: {}", job_token),
            &url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run curl: {}", e))?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let notes: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap_or_default();

    for note in &notes {
        if let Some(body) = note.get("body").and_then(|b| b.as_str())
            && body.contains(COMMENT_MARKER)
            && let Some(id) = note.get("id").and_then(|i| i.as_u64())
        {
            return Ok(Some(id));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Batch Mode
// ---------------------------------------------------------------------------

/// Compute an attribution report across a range of commits by reading their
/// authorship notes. This is designed for CI environments where there is no
/// daemon -- it simply parses existing git notes.
///
/// `repo_path` must point to the root of a git repository.
/// `base_ref` and `head_ref` define the commit range (base..head).
pub fn compute_batch_report(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<AttributionReport, String> {
    // Get all commit SHAs in the range
    let range = format!("{}..{}", base_ref, head_ref);
    let log_output = git_in(repo_path, &["log", "--format=%H", &range])?;
    let commits: Vec<&str> = log_output.lines().filter(|l| !l.is_empty()).collect();

    if commits.is_empty() {
        return Ok(AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        });
    }

    // Aggregate per-file stats across all commits
    let mut file_stats: HashMap<String, (usize, usize, usize)> = HashMap::new(); // (ai, human, untracked)

    for commit in &commits {
        let note_result = git_in(repo_path, &["notes", "--ref=ai", "show", commit]);
        let note_content = match note_result {
            Ok(content) => content,
            Err(_) => continue, // No note for this commit, skip
        };

        if note_content.trim().is_empty() {
            continue;
        }

        let authorship = match AuthorshipLog::deserialize_from_string(&note_content) {
            Ok(log) => log,
            Err(_) => continue, // Malformed note, skip
        };

        // Determine AI and human hashes from metadata
        let (ai_hashes, human_hashes) = classify_hashes(&authorship);

        // Aggregate line counts per file
        for file_att in &authorship.attestations {
            let (ai, human, untracked) = file_stats
                .entry(file_att.file_path.clone())
                .or_insert((0, 0, 0));

            for entry in &file_att.entries {
                let line_count: usize = entry
                    .line_ranges
                    .iter()
                    .map(|r| r.line_count() as usize)
                    .sum();

                if ai_hashes.contains(entry.hash.as_str()) {
                    *ai += line_count;
                } else if human_hashes.contains(entry.hash.as_str()) {
                    *human += line_count;
                } else {
                    *untracked += line_count;
                }
            }
        }
    }

    // Build the report
    let mut files: Vec<FileAttribution> = file_stats
        .into_iter()
        .map(|(path, (ai, human, untracked))| FileAttribution {
            path,
            ai_lines: ai,
            human_lines: human,
            untracked_lines: untracked,
        })
        .collect();

    // Sort by path for deterministic output
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let ai_lines: usize = files.iter().map(|f| f.ai_lines).sum();
    let human_lines: usize = files.iter().map(|f| f.human_lines).sum();
    let untracked_lines: usize = files.iter().map(|f| f.untracked_lines).sum();

    Ok(AttributionReport {
        total_lines: ai_lines + human_lines + untracked_lines,
        ai_lines,
        human_lines,
        untracked_lines,
        files,
    })
}

// ---------------------------------------------------------------------------
// Threshold Check
// ---------------------------------------------------------------------------

/// Check whether the report exceeds the given AI percentage threshold.
///
/// Returns `true` if the AI percentage exceeds `max_ai_percent`, meaning
/// the CI step should fail (exit code 1).
pub fn check_threshold(report: &AttributionReport, max_ai_percent: f64) -> bool {
    if report.total_lines == 0 {
        return false;
    }
    let ai_percent = (report.ai_lines as f64 / report.total_lines as f64) * 100.0;
    ai_percent > max_ai_percent
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Classify hashes from an AuthorshipLog into AI and human sets.
fn classify_hashes(
    log: &AuthorshipLog,
) -> (
    std::collections::HashSet<&str>,
    std::collections::HashSet<&str>,
) {
    let mut ai = std::collections::HashSet::new();
    for key in log.metadata.prompts.keys() {
        ai.insert(key.as_str());
    }
    for key in log.metadata.sessions.keys() {
        ai.insert(key.as_str());
    }

    let mut human = std::collections::HashSet::new();
    for key in log.metadata.humans.keys() {
        human.insert(key.as_str());
    }

    (ai, human)
}

fn git_in(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // -- CI Detection Tests --

    /// Global mutex to serialize tests that manipulate environment variables.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to temporarily set env vars for a test, then restore originals.
    struct EnvGuard {
        vars: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        /// Set env vars. Caller MUST hold ENV_MUTEX.
        fn set(vars: &[(&str, &str)]) -> Self {
            let mut originals = Vec::new();
            for (key, value) in vars {
                originals.push((key.to_string(), std::env::var(key).ok()));
                // SAFETY: caller holds ENV_MUTEX, ensuring exclusive access.
                unsafe { std::env::set_var(key, value) };
            }
            EnvGuard { vars: originals }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, original) in &self.vars {
                match original {
                    // SAFETY: caller holds ENV_MUTEX, ensuring exclusive access.
                    Some(val) => unsafe { std::env::set_var(key, val) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }

    #[test]
    fn test_detect_ci_not_in_ci() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Make sure relevant env vars are unset
        let _guard = EnvGuard::set(&[]);
        // SAFETY: we hold ENV_MUTEX.
        unsafe {
            std::env::remove_var("GITHUB_ACTIONS");
            std::env::remove_var("GITLAB_CI");
        }
        assert!(detect_ci().is_none());
    }

    #[test]
    fn test_detect_github_actions() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "octocat/hello-world"),
            ("GITHUB_SHA", "abc123def456"),
            ("GITHUB_REF", "refs/pull/42/merge"),
            ("GITHUB_BASE_REF", "main"),
            ("GITHUB_HEAD_REF", "feature-branch"),
        ]);

        let ctx = detect_ci().expect("should detect GitHub Actions");
        assert_eq!(ctx.provider, CiProvider::GitHubActions);
        assert_eq!(ctx.repo_owner, "octocat");
        assert_eq!(ctx.repo_name, "hello-world");
        assert_eq!(ctx.pr_number, Some(42));
        assert_eq!(ctx.commit_sha, "abc123def456");
        assert_eq!(ctx.base_ref.as_deref(), Some("main"));
        assert_eq!(ctx.head_ref.as_deref(), Some("feature-branch"));
    }

    #[test]
    fn test_detect_gitlab_ci() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Clear GitHub vars to prevent interference
        // SAFETY: we hold ENV_MUTEX.
        unsafe { std::env::remove_var("GITHUB_ACTIONS") };
        let _guard = EnvGuard::set(&[
            ("GITLAB_CI", "true"),
            ("CI_PROJECT_PATH", "mygroup/myproject"),
            ("CI_COMMIT_SHA", "deadbeef1234"),
            ("CI_MERGE_REQUEST_IID", "99"),
            ("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main"),
            ("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "fix/thing"),
        ]);

        let ctx = detect_ci().expect("should detect GitLab CI");
        assert_eq!(ctx.provider, CiProvider::GitLabCi);
        assert_eq!(ctx.repo_owner, "mygroup");
        assert_eq!(ctx.repo_name, "myproject");
        assert_eq!(ctx.pr_number, Some(99));
        assert_eq!(ctx.commit_sha, "deadbeef1234");
        assert_eq!(ctx.base_ref.as_deref(), Some("main"));
        assert_eq!(ctx.head_ref.as_deref(), Some("fix/thing"));
    }

    #[test]
    fn test_parse_pr_number_from_ref() {
        assert_eq!(parse_pr_number_from_ref("refs/pull/123/merge"), Some(123));
        assert_eq!(parse_pr_number_from_ref("refs/pull/1/head"), Some(1));
        assert_eq!(parse_pr_number_from_ref("refs/heads/main"), None);
        assert_eq!(parse_pr_number_from_ref(""), None);
    }

    #[test]
    fn test_split_owner_repo() {
        assert_eq!(
            split_owner_repo("octocat/hello-world"),
            ("octocat".to_string(), "hello-world".to_string())
        );
        assert_eq!(
            split_owner_repo("group/subgroup/project"),
            ("group".to_string(), "subgroup/project".to_string())
        );
        assert_eq!(
            split_owner_repo("standalone"),
            (String::new(), "standalone".to_string())
        );
    }

    // -- Diff Parsing Tests --

    #[test]
    fn test_parse_diff_added_lines() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
+new line 1
 existing
+new line 3
 existing
 existing
diff --git a/src/lib.rs b/src/lib.rs
new file mode 100644
--- /dev/null
+++ b/src/lib.rs
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
";
        let result = parse_diff_added_lines(diff);
        assert_eq!(result.get("src/main.rs"), Some(&vec![1, 2, 3, 4, 5]));
        assert_eq!(result.get("src/lib.rs"), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn test_parse_hunk_header_new_side() {
        assert_eq!(parse_hunk_header_new_side("@@ -1,3 +1,5 @@"), Some((1, 5)));
        assert_eq!(parse_hunk_header_new_side("@@ -0,0 +1,3 @@"), Some((1, 3)));
        assert_eq!(
            parse_hunk_header_new_side("@@ -10,2 +15 @@ context"),
            Some((15, 1))
        );
        assert_eq!(parse_hunk_header_new_side("not a hunk"), None);
    }

    // -- Markdown Formatting Tests --

    #[test]
    fn test_format_markdown_report_structure() {
        let report = AttributionReport {
            total_lines: 200,
            ai_lines: 42,
            human_lines: 158,
            untracked_lines: 0,
            files: vec![FileAttribution {
                path: "src/foo.rs".to_string(),
                ai_lines: 12,
                human_lines: 28,
                untracked_lines: 0,
            }],
        };

        let md = format_markdown_report(&report);

        // Check header and marker
        assert!(md.contains("## AI Attribution Report"));
        assert!(md.contains(COMMENT_MARKER));

        // Check summary table
        assert!(md.contains("| Metric | Value |"));
        assert!(md.contains("|--------|-------|"));
        assert!(md.contains("| AI Lines | 42 |"));
        assert!(md.contains("| Human Lines | 158 |"));
        assert!(md.contains("| AI % | 21.0% |"));

        // Check per-file breakdown
        assert!(md.contains("### Per-file breakdown"));
        assert!(md.contains("| File | AI % | AI Lines | Total Lines |"));
        assert!(md.contains("| src/foo.rs | 30% | 12 | 40 |"));
    }

    #[test]
    fn test_format_markdown_report_empty() {
        let report = AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        };

        let md = format_markdown_report(&report);
        assert!(md.contains("## AI Attribution Report"));
        assert!(md.contains(COMMENT_MARKER));
        assert!(md.contains("| AI Lines | 0 |"));
        assert!(md.contains("| Human Lines | 0 |"));
        assert!(md.contains("| AI % | 0.0% |"));
        // No per-file section for empty reports
        assert!(!md.contains("### Per-file breakdown"));
    }

    #[test]
    fn test_format_markdown_report_multiple_files() {
        let report = AttributionReport {
            total_lines: 100,
            ai_lines: 60,
            human_lines: 40,
            untracked_lines: 0,
            files: vec![
                FileAttribution {
                    path: "src/a.rs".to_string(),
                    ai_lines: 40,
                    human_lines: 10,
                    untracked_lines: 0,
                },
                FileAttribution {
                    path: "src/b.rs".to_string(),
                    ai_lines: 20,
                    human_lines: 30,
                    untracked_lines: 0,
                },
            ],
        };

        let md = format_markdown_report(&report);
        assert!(md.contains("| AI % | 60.0% |"));
        assert!(md.contains("| src/a.rs | 80% | 40 | 50 |"));
        assert!(md.contains("| src/b.rs | 40% | 20 | 50 |"));
    }

    // -- Threshold Tests --

    #[test]
    fn test_check_threshold_below() {
        let report = AttributionReport {
            total_lines: 200,
            ai_lines: 40,
            human_lines: 160,
            untracked_lines: 0,
            files: Vec::new(),
        };
        // 40/200 = 20%, threshold is 25%
        assert!(!check_threshold(&report, 25.0));
    }

    #[test]
    fn test_check_threshold_above() {
        let report = AttributionReport {
            total_lines: 200,
            ai_lines: 60,
            human_lines: 140,
            untracked_lines: 0,
            files: Vec::new(),
        };
        // 60/200 = 30%, threshold is 25%
        assert!(check_threshold(&report, 25.0));
    }

    #[test]
    fn test_check_threshold_exact_boundary() {
        let report = AttributionReport {
            total_lines: 100,
            ai_lines: 50,
            human_lines: 50,
            untracked_lines: 0,
            files: Vec::new(),
        };
        // 50/100 = 50.0%, threshold is exactly 50.0%
        // Should NOT exceed (> not >=)
        assert!(!check_threshold(&report, 50.0));
    }

    #[test]
    fn test_check_threshold_empty_report() {
        let report = AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        };
        // Empty report should never exceed threshold
        assert!(!check_threshold(&report, 0.0));
        assert!(!check_threshold(&report, 50.0));
    }

    #[test]
    fn test_check_threshold_hundred_percent_ai() {
        let report = AttributionReport {
            total_lines: 100,
            ai_lines: 100,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        };
        // 100% AI, threshold is 99%
        assert!(check_threshold(&report, 99.0));
    }

    // -- Batch Aggregation Logic Tests --

    #[test]
    fn test_batch_aggregation_from_multiple_notes() {
        // Test the aggregation logic by simulating what compute_batch_report does
        // internally: parse multiple authorship logs and aggregate stats.

        use crate::core::authorship_log::{
            AgentId, AttestationEntry, AuthorshipLog, FileAttestation, HumanRecord, LineRange,
            Metadata, PromptRecord,
        };

        // Simulate two commits with authorship notes
        let mut log1 = AuthorshipLog::new(Metadata::new("base1".to_string()));
        log1.metadata.prompts.insert(
            "ai_hash_1".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: "sess1".to_string(),
                    model: "claude-3".to_string(),
                },
                human_author: None,
                messages_url: None,
                total_additions: 5,
                total_deletions: 0,
                accepted_lines: 5,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );
        log1.metadata.humans.insert(
            "h_human_hash_1".to_string(),
            HumanRecord {
                author: "dev@example.com".to_string(),
            },
        );
        log1.attestations.push(FileAttestation {
            file_path: "src/main.rs".to_string(),
            entries: vec![
                AttestationEntry {
                    hash: "ai_hash_1".to_string(),
                    line_ranges: vec![LineRange::Range(1, 5)],
                },
                AttestationEntry {
                    hash: "h_human_hash_1".to_string(),
                    line_ranges: vec![LineRange::Range(6, 10)],
                },
            ],
        });

        let mut log2 = AuthorshipLog::new(Metadata::new("base2".to_string()));
        log2.metadata.prompts.insert(
            "ai_hash_2".to_string(),
            PromptRecord {
                agent_id: AgentId {
                    tool: "copilot".to_string(),
                    id: "sess2".to_string(),
                    model: "gpt-4".to_string(),
                },
                human_author: None,
                messages_url: None,
                total_additions: 3,
                total_deletions: 0,
                accepted_lines: 3,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );
        log2.attestations.push(FileAttestation {
            file_path: "src/main.rs".to_string(),
            entries: vec![AttestationEntry {
                hash: "ai_hash_2".to_string(),
                line_ranges: vec![LineRange::Range(11, 13)],
            }],
        });
        log2.attestations.push(FileAttestation {
            file_path: "src/lib.rs".to_string(),
            entries: vec![AttestationEntry {
                hash: "ai_hash_2".to_string(),
                line_ranges: vec![LineRange::Range(1, 7)],
            }],
        });

        // Simulate the aggregation logic from compute_batch_report
        let logs = vec![log1, log2];
        let mut file_stats: HashMap<String, (usize, usize, usize)> = HashMap::new();

        for authorship in &logs {
            let ai_hashes: std::collections::HashSet<&str> = {
                let mut set = std::collections::HashSet::new();
                for key in authorship.metadata.prompts.keys() {
                    set.insert(key.as_str());
                }
                for key in authorship.metadata.sessions.keys() {
                    set.insert(key.as_str());
                }
                set
            };
            let human_hashes: std::collections::HashSet<&str> = {
                let mut set = std::collections::HashSet::new();
                for key in authorship.metadata.humans.keys() {
                    set.insert(key.as_str());
                }
                set
            };

            for file_att in &authorship.attestations {
                let (ai, human, untracked) = file_stats
                    .entry(file_att.file_path.clone())
                    .or_insert((0, 0, 0));
                for entry in &file_att.entries {
                    let line_count: usize = entry
                        .line_ranges
                        .iter()
                        .map(|r| r.line_count() as usize)
                        .sum();
                    if ai_hashes.contains(entry.hash.as_str()) {
                        *ai += line_count;
                    } else if human_hashes.contains(entry.hash.as_str()) {
                        *human += line_count;
                    } else {
                        *untracked += line_count;
                    }
                }
            }
        }

        // Verify aggregated results
        let main_stats = file_stats.get("src/main.rs").unwrap();
        assert_eq!(main_stats.0, 8); // AI: 5 (log1) + 3 (log2)
        assert_eq!(main_stats.1, 5); // Human: 5 (log1)
        assert_eq!(main_stats.2, 0); // Untracked: 0

        let lib_stats = file_stats.get("src/lib.rs").unwrap();
        assert_eq!(lib_stats.0, 7); // AI: 7 (log2)
        assert_eq!(lib_stats.1, 0); // Human: 0
        assert_eq!(lib_stats.2, 0); // Untracked: 0
    }

    #[test]
    fn test_batch_aggregation_untracked_entries() {
        // Test that entries with hashes not in prompts/sessions/humans are counted
        // as untracked.

        use crate::core::authorship_log::{
            AttestationEntry, AuthorshipLog, FileAttestation, LineRange, Metadata,
        };

        let mut log = AuthorshipLog::new(Metadata::new("base".to_string()));
        // No prompts, sessions, or humans in metadata -- all entries are untracked
        log.attestations.push(FileAttestation {
            file_path: "unknown.rs".to_string(),
            entries: vec![AttestationEntry {
                hash: "mystery_hash".to_string(),
                line_ranges: vec![LineRange::Range(1, 20)],
            }],
        });

        let ai_hashes: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let human_hashes: std::collections::HashSet<&str> = std::collections::HashSet::new();

        let mut untracked_total = 0usize;
        for file_att in &log.attestations {
            for entry in &file_att.entries {
                let line_count: usize = entry
                    .line_ranges
                    .iter()
                    .map(|r| r.line_count() as usize)
                    .sum();
                if !ai_hashes.contains(entry.hash.as_str())
                    && !human_hashes.contains(entry.hash.as_str())
                {
                    untracked_total += line_count;
                }
            }
        }

        assert_eq!(untracked_total, 20);
    }

    // -- Legacy format_markdown tests (kept for backward compat) --

    #[test]
    fn test_percentage() {
        assert_eq!(percentage(0, 0), 0);
        assert_eq!(percentage(50, 100), 50);
        assert_eq!(percentage(1, 3), 33);
        assert_eq!(percentage(2, 3), 67);
        assert_eq!(percentage(100, 100), 100);
    }

    #[test]
    fn test_percentage_f64() {
        assert_eq!(percentage_f64(0, 0), 0.0);
        assert!((percentage_f64(50, 100) - 50.0).abs() < f64::EPSILON);
        assert!((percentage_f64(1, 3) - 33.333333333333336).abs() < 0.001);
        assert!((percentage_f64(42, 200) - 21.0).abs() < f64::EPSILON);
    }

    // -- Report Struct Construction Test --

    #[test]
    fn test_attribution_report_aggregation() {
        let files = vec![
            FileAttribution {
                path: "a.rs".to_string(),
                ai_lines: 10,
                human_lines: 5,
                untracked_lines: 2,
            },
            FileAttribution {
                path: "b.rs".to_string(),
                ai_lines: 3,
                human_lines: 7,
                untracked_lines: 1,
            },
        ];

        let total_ai: usize = files.iter().map(|f| f.ai_lines).sum();
        let total_human: usize = files.iter().map(|f| f.human_lines).sum();
        let total_untracked: usize = files.iter().map(|f| f.untracked_lines).sum();
        let total = total_ai + total_human + total_untracked;

        let report = AttributionReport {
            total_lines: total,
            ai_lines: total_ai,
            human_lines: total_human,
            untracked_lines: total_untracked,
            files,
        };

        assert_eq!(report.total_lines, 28);
        assert_eq!(report.ai_lines, 13);
        assert_eq!(report.human_lines, 12);
        assert_eq!(report.untracked_lines, 3);
    }

    // -- GitHub comment posting (unit-testable parts) --

    #[test]
    fn test_post_github_comment_requires_pr_number() {
        let report = AttributionReport {
            total_lines: 10,
            ai_lines: 5,
            human_lines: 5,
            untracked_lines: 0,
            files: Vec::new(),
        };
        let env = CiContext {
            provider: CiProvider::GitHubActions,
            repo_owner: "octocat".to_string(),
            repo_name: "hello".to_string(),
            pr_number: None, // No PR number
            commit_sha: "abc".to_string(),
            base_ref: None,
            head_ref: None,
        };

        let result = post_github_comment(&report, &env);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no PR number"));
    }

    #[test]
    fn test_post_gitlab_comment_requires_mr_iid() {
        let report = AttributionReport {
            total_lines: 10,
            ai_lines: 5,
            human_lines: 5,
            untracked_lines: 0,
            files: Vec::new(),
        };
        let env = CiContext {
            provider: CiProvider::GitLabCi,
            repo_owner: "group".to_string(),
            repo_name: "project".to_string(),
            pr_number: None, // No MR IID
            commit_sha: "abc".to_string(),
            base_ref: None,
            head_ref: None,
        };

        let result = post_gitlab_comment(&report, &env);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no merge request IID"));
    }

    #[test]
    fn test_format_markdown_report_contains_marker() {
        let report = AttributionReport {
            total_lines: 50,
            ai_lines: 25,
            human_lines: 25,
            untracked_lines: 0,
            files: Vec::new(),
        };
        let md = format_markdown_report(&report);
        assert!(md.contains("<!-- git-ai-attribution -->"));
    }
}
