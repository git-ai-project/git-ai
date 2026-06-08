use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::git::notes_api::read_note;
use crate::git::repository::Repository;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AwaitStatsOptions {
    // Polling options
    timeout_ms: u64,
    interval_ms: u64,

    // Output format options
    json: bool,

    // Commit selection
    commit: String,

    // Suppress expected timeout noise for callers that poll externally
    quiet: bool,
}

#[derive(Debug, Clone)]
pub struct AwaitStatsRuntimeOptions {
    pub timeout_ms: u64,
    pub interval_ms: u64,
    pub json: bool,
    pub commit: String,
    pub quiet: bool,
    pub is_interactive: bool,
}

impl Default for AwaitStatsOptions {
    // Build the default await-stats configuration used when no flags are provided.
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            interval_ms: DEFAULT_INTERVAL_MS,
            json: false,
            commit: "HEAD".to_string(),
            quiet: false,
        }
    }
}

#[derive(Debug)]
pub enum AwaitStatsError {
    // CLI argument errors
    BadArgs(String),
    // Revision did not resolve to a commit
    BadCommit(String),
    // Authorship note was not created before the timeout elapsed
    Timeout { commit_sha: String, timeout_ms: u64 },
    // Note exists, but cannot be parsed or converted to stats
    CorruptNote(String),
    // Lower-level git-ai error
    GitAi(GitAiError),
}

impl AwaitStatsError {
    // Map await-stats failures to the command's public process exit codes.
    fn exit_code(&self) -> i32 {
        match self {
            Self::Timeout { .. } => 1,
            Self::BadArgs(_) | Self::BadCommit(_) => 2,
            Self::CorruptNote(_) => 3,
            Self::GitAi(_) => 1,
        }
    }
}

impl From<GitAiError> for AwaitStatsError {
    // Wrap shared git-ai errors in the await-stats-specific error type.
    fn from(err: GitAiError) -> Self {
        Self::GitAi(err)
    }
}

// Run await-stats as a CLI command and terminate the process with the right exit code.
pub fn handle_await_stats(args: &[String]) -> ! {
    match run(args) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            if !matches!(err, AwaitStatsError::Timeout { .. }) || !is_quiet_timeout(args) {
                print_error(&err);
            }
            std::process::exit(err.exit_code());
        }
    }
}

// Parse CLI arguments and execute await-stats without exiting the process.
pub fn run(args: &[String]) -> Result<(), AwaitStatsError> {
    let options = parse_options(args)?;
    run_with_options(&options)
}

// Wait for the requested commit's authorship note and print its stats.
fn run_with_options(options: &AwaitStatsOptions) -> Result<(), AwaitStatsError> {
    let repo = find_repository(&Vec::<String>::new()).map_err(AwaitStatsError::GitAi)?;
    run_with_repo(
        &repo,
        &AwaitStatsRuntimeOptions {
            timeout_ms: options.timeout_ms,
            interval_ms: options.interval_ms,
            json: options.json,
            commit: options.commit.clone(),
            quiet: options.quiet,
            is_interactive: true,
        },
    )
}

pub fn run_with_repo(
    repo: &Repository,
    options: &AwaitStatsRuntimeOptions,
) -> Result<(), AwaitStatsError> {
    let commit_sha = resolve_commit(repo, &options.commit)?;

    // The post-commit hook writes authorship stats asynchronously, so wait until
    // the note appears before trying to render the final commit stats.
    let raw_note = wait_for_authorship_note(
        repo,
        &commit_sha,
        Duration::from_millis(options.timeout_ms),
        Duration::from_millis(options.interval_ms),
    )?
    .ok_or_else(|| AwaitStatsError::Timeout {
        commit_sha: commit_sha.clone(),
        timeout_ms: options.timeout_ms,
    })?;

    // Validate the note separately so corrupt authorship data reports as a note
    // problem instead of looking like a stats rendering failure.
    validate_authorship_note(&raw_note)?;
    render_stats_for_commit(repo, &commit_sha, options.json, options.is_interactive)
}

// Convert raw CLI flags into validated await-stats options.
fn parse_options(args: &[String]) -> Result<AwaitStatsOptions, AwaitStatsError> {
    let mut options = AwaitStatsOptions::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--timeout" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| AwaitStatsError::BadArgs("--timeout requires a value".into()))?;
                options.timeout_ms = parse_u64_option("--timeout", value)?;
                i += 2;
            }
            "--interval" => {
                let value = args.get(i + 1).ok_or_else(|| {
                    AwaitStatsError::BadArgs("--interval requires a value".into())
                })?;
                options.interval_ms = parse_u64_option("--interval", value)?;
                if options.interval_ms == 0 {
                    return Err(AwaitStatsError::BadArgs(
                        "--interval must be greater than 0".to_string(),
                    ));
                }
                i += 2;
            }
            "--commit" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| AwaitStatsError::BadArgs("--commit requires a value".into()))?;
                options.commit = value.clone();
                i += 2;
            }
            "--json" => {
                options.json = true;
                i += 1;
            }
            "--quiet" => {
                options.quiet = true;
                i += 1;
            }
            other => {
                return Err(AwaitStatsError::BadArgs(format!(
                    "Unknown await-stats argument: {}",
                    other
                )));
            }
        }
    }

    Ok(options)
}

// Parse a millisecond option value as an unsigned integer.
fn parse_u64_option(flag: &str, value: &str) -> Result<u64, AwaitStatsError> {
    value
        .parse::<u64>()
        .map_err(|_| AwaitStatsError::BadArgs(format!("{} requires a non-negative integer", flag)))
}

// Resolve a revision string to the commit SHA whose authorship note should be read.
pub fn resolve_commit(repo: &Repository, rev: &str) -> Result<String, AwaitStatsError> {
    // Peel tags and other revision objects to the commit that owns the note.
    repo.revparse_single(rev)
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|_| AwaitStatsError::BadCommit(rev.to_string()))
}

// Poll the git-ai notes namespace until the target commit's authorship note appears.
pub fn wait_for_authorship_note(
    repo: &Repository,
    commit_sha: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<Option<String>, AwaitStatsError> {
    let start = Instant::now();

    loop {
        // The note may already exist when await-stats is called after a fast hook.
        if let Some(note) = read_note(repo, commit_sha) {
            return Ok(Some(note));
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Ok(None);
        }

        // Sleep only until the timeout boundary, so short timeouts do not
        // overshoot by a full polling interval.
        let remaining = timeout.saturating_sub(elapsed);
        thread::sleep(interval.min(remaining));
    }
}

pub fn validate_authorship_note(raw_note: &str) -> Result<(), AwaitStatsError> {
    AuthorshipLog::deserialize_from_string(raw_note)
        .map(|_| ())
        .map_err(|err| AwaitStatsError::CorruptNote(err.to_string()))
}

pub fn render_stats_for_commit(
    repo: &Repository,
    commit_sha: &str,
    json: bool,
    is_interactive: bool,
) -> Result<(), AwaitStatsError> {
    let effective_patterns = effective_ignore_patterns(repo, &[], &[]);
    let stats = stats_for_commit_stats(repo, commit_sha, &effective_patterns)
        .map_err(|err| AwaitStatsError::CorruptNote(err.to_string()))?;

    if json {
        let json = serde_json::to_string(&stats).map_err(GitAiError::from)?;
        println!("{}", json);
    } else {
        write_stats_to_terminal(&stats, is_interactive);
    }

    Ok(())
}

// Detect whether timeout output should be suppressed before parsed options are available.
fn is_quiet_timeout(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--quiet")
}

// Print the user-facing error message for an await-stats failure.
fn print_error(err: &AwaitStatsError) {
    match err {
        AwaitStatsError::BadArgs(msg) => {
            eprintln!("{}", msg);
            eprintln!("Run 'git ai await-stats --help' for usage.");
        }
        AwaitStatsError::BadCommit(rev) => {
            eprintln!("No commit found: {}", rev);
        }
        AwaitStatsError::Timeout {
            commit_sha,
            timeout_ms,
        } => {
            eprintln!(
                "[git-ai] timed out waiting for authorship note on commit {} ({}ms)",
                short_sha(commit_sha),
                timeout_ms
            );
        }
        AwaitStatsError::CorruptNote(msg) => {
            eprintln!("Failed to read authorship note: {}", msg);
        }
        AwaitStatsError::GitAi(err) => {
            eprintln!("await-stats failed: {}", err);
        }
    }
}

// Return the short display form of a commit SHA.
fn short_sha(commit_sha: &str) -> &str {
    &commit_sha[..commit_sha.len().min(8)]
}

// Print await-stats usage information to stderr.
fn print_help() {
    eprintln!("git ai await-stats - Wait for commit authorship note, then print stats");
    eprintln!();
    eprintln!("Usage: git ai await-stats [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --timeout <ms>      Maximum wait time (default: 5000)");
    eprintln!("  --interval <ms>     Poll interval (default: 100)");
    eprintln!("  --commit <rev>      Commit to await (default: HEAD)");
    eprintln!("  --json              Output stats as JSON");
    eprintln!("  --quiet             Suppress timeout output");
    eprintln!("  -h, --help          Show this help message");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Convert string slices into owned CLI argument strings for parser tests.
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    // Verify that omitted flags use the command's documented defaults.
    fn parse_defaults() {
        assert_eq!(
            parse_options(&[]).unwrap(),
            AwaitStatsOptions {
                timeout_ms: 5_000,
                interval_ms: 100,
                json: false,
                commit: "HEAD".to_string(),
                quiet: false,
            }
        );
    }

    #[test]
    // Verify that all supported flags are accepted and stored in options.
    fn parse_all_options() {
        assert_eq!(
            parse_options(&args(&[
                "--timeout",
                "8000",
                "--interval",
                "25",
                "--json",
                "--commit",
                "HEAD~1",
                "--quiet",
            ]))
            .unwrap(),
            AwaitStatsOptions {
                timeout_ms: 8_000,
                interval_ms: 25,
                json: true,
                commit: "HEAD~1".to_string(),
                quiet: true,
            }
        );
    }

    #[test]
    // Verify that flags requiring values reject missing arguments.
    fn parse_rejects_missing_values() {
        assert!(matches!(
            parse_options(&args(&["--timeout"])),
            Err(AwaitStatsError::BadArgs(_))
        ));
        assert!(matches!(
            parse_options(&args(&["--interval"])),
            Err(AwaitStatsError::BadArgs(_))
        ));
        assert!(matches!(
            parse_options(&args(&["--commit"])),
            Err(AwaitStatsError::BadArgs(_))
        ));
    }

    #[test]
    // Verify that the poll interval cannot be zero.
    fn parse_rejects_zero_interval() {
        assert!(matches!(
            parse_options(&args(&["--interval", "0"])),
            Err(AwaitStatsError::BadArgs(_))
        ));
    }

    #[test]
    // Verify that unknown flags fail with a bad-arguments error.
    fn parse_rejects_unknown_flags() {
        assert!(matches!(
            parse_options(&args(&["--wat"])),
            Err(AwaitStatsError::BadArgs(_))
        ));
    }
}
