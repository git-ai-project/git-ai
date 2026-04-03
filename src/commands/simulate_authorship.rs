//! Dev-only command: simulate AI authorship across commits on a branch.
//!
//! Walks a commit range and attaches synthetic authorship notes to each commit,
//! making the repository look as if various AI tools were used.  Useful for
//! building realistic test fixtures.
//!
//! This module is compiled only in debug builds (`#[cfg(debug_assertions)]`).

use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::authorship::authorship_log_serialization::{
    AUTHORSHIP_LOG_VERSION, AttestationEntry, AuthorshipLog, AuthorshipMetadata, FileAttestation,
    generate_short_hash,
};
use crate::authorship::working_log::AgentId;
use crate::git::refs::notes_add_batch;
use crate::git::repository::{Repository, exec_git};
use std::collections::BTreeMap;

/// AI tools we rotate through when synthesising notes.
const TOOLS: &[(&str, &str)] = &[
    ("cursor", "claude-3.5-sonnet"),
    ("windsurf", "claude-3.5-sonnet"),
    ("claude", "claude-sonnet-4-20250514"),
    ("github-copilot", "gpt-4o"),
    ("codex", "o3"),
    ("amp", "claude-sonnet-4-20250514"),
];

pub fn handle_simulate_authorship(args: &[String]) {
    if args.is_empty() {
        print_usage();
        std::process::exit(1);
    }

    let mut repo_path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut base: Option<String> = None;
    let mut count: Option<usize> = None;
    let mut ai_ratio: f64 = 0.7; // fraction of commits that get AI authorship
    let mut dry_run = false;
    let mut seed: u64 = 42;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --repo requires a value");
                    std::process::exit(1);
                }
                repo_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--branch" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --branch requires a value");
                    std::process::exit(1);
                }
                branch = Some(args[i + 1].clone());
                i += 2;
            }
            "--base" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --base requires a value");
                    std::process::exit(1);
                }
                base = Some(args[i + 1].clone());
                i += 2;
            }
            "--count" | "-n" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --count requires a value");
                    std::process::exit(1);
                }
                count = Some(args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Error: --count must be a positive integer");
                    std::process::exit(1);
                }));
                i += 2;
            }
            "--ai-ratio" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --ai-ratio requires a value");
                    std::process::exit(1);
                }
                ai_ratio = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Error: --ai-ratio must be a number between 0.0 and 1.0");
                    std::process::exit(1);
                });
                if !(0.0..=1.0).contains(&ai_ratio) {
                    eprintln!("Error: --ai-ratio must be between 0.0 and 1.0");
                    std::process::exit(1);
                }
                i += 2;
            }
            "--seed" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --seed requires a value");
                    std::process::exit(1);
                }
                seed = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Error: --seed must be an integer");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let branch = branch.unwrap_or_else(|| {
        eprintln!("Error: --branch is required");
        std::process::exit(1);
    });

    // Resolve repository
    let repo = if let Some(path) = repo_path {
        match crate::git::find_repository_in_path(&path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to open repository at {}: {}", path, e);
                std::process::exit(1);
            }
        }
    } else {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        match crate::git::find_repository_in_path(&cwd) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to open repository: {}", e);
                std::process::exit(1);
            }
        }
    };

    // Build rev-list to get commits
    let commits = list_commits(&repo, &branch, base.as_deref(), count);
    if commits.is_empty() {
        eprintln!("No commits found on branch '{}'", branch);
        std::process::exit(1);
    }

    eprintln!(
        "Processing {} commits on '{}' (ai-ratio={}, seed={})",
        commits.len(),
        branch,
        ai_ratio,
        seed
    );

    let mut rng = SimpleRng::new(seed);
    let mut note_entries: Vec<(String, String)> = Vec::new();
    let mut annotated = 0usize;
    let mut skipped = 0usize;

    for (idx, commit_sha) in commits.iter().enumerate() {
        // Decide whether this commit gets AI authorship
        if rng.next_f64() > ai_ratio {
            skipped += 1;
            continue;
        }

        // Get the diff stats for this commit (files + line counts)
        let file_stats = diff_numstat_for_commit(&repo, commit_sha);
        if file_stats.is_empty() {
            skipped += 1;
            continue;
        }

        // Pick a tool for this commit
        let tool_idx = rng.next_usize() % TOOLS.len();
        let (tool, model) = TOOLS[tool_idx];
        let session_id = format!("sim-{}-{}", commit_sha.get(..8).unwrap_or(commit_sha), idx);

        // Get the author email from the commit
        let author_email = commit_author_email(&repo, commit_sha);

        // Build the authorship note
        let note = build_authorship_note(
            commit_sha,
            &file_stats,
            tool,
            model,
            &session_id,
            author_email.as_deref(),
        );

        if dry_run {
            eprintln!(
                "  [dry-run] {} → {} ({} files)",
                &commit_sha[..8.min(commit_sha.len())],
                tool,
                file_stats.len()
            );
        }

        note_entries.push((commit_sha.clone(), note));
        annotated += 1;
    }

    if dry_run {
        eprintln!(
            "Dry run complete: {} would be annotated, {} skipped",
            annotated, skipped
        );
        return;
    }

    if note_entries.is_empty() {
        eprintln!("No commits selected for annotation.");
        return;
    }

    // Write all notes in a single batch
    match notes_add_batch(&repo, &note_entries) {
        Ok(()) => {
            eprintln!("Done: {} commits annotated, {} skipped", annotated, skipped);
        }
        Err(e) => {
            eprintln!("Failed to write notes: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Usage: git-ai simulate-authorship --branch <branch> [options]");
    eprintln!();
    eprintln!("Simulate AI authorship across commits for testing.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --branch <name>      Branch to process (required)");
    eprintln!("  --repo <path>        Path to repository (default: current directory)");
    eprintln!("  --base <ref>         Only process commits after this ref (exclusive)");
    eprintln!("  --count, -n <N>      Limit to N most recent commits");
    eprintln!("  --ai-ratio <0.0-1.0> Fraction of commits to annotate (default: 0.7)");
    eprintln!("  --seed <N>           Random seed for reproducibility (default: 42)");
    eprintln!("  --dry-run            Show what would be done without writing notes");
    eprintln!("  --help, -h           Show this help");
}

/// List commit SHAs in topological order (oldest first).
fn list_commits(
    repo: &Repository,
    branch: &str,
    base: Option<&str>,
    count: Option<usize>,
) -> Vec<String> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--reverse".to_string()); // oldest first
    if let Some(n) = count {
        args.push(format!("-n{}", n));
    }
    if let Some(base_ref) = base {
        args.push(format!("{}..{}", base_ref, branch));
    } else {
        args.push(branch.to_string());
    }

    match exec_git(&args) {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        }
        Err(e) => {
            eprintln!("Failed to list commits: {}", e);
            std::process::exit(1);
        }
    }
}

/// File stat from `git diff --numstat`.
struct FileStat {
    path: String,
    additions: u32,
    deletions: u32,
}

/// Get per-file addition/deletion counts for a commit.
fn diff_numstat_for_commit(repo: &Repository, commit_sha: &str) -> Vec<FileStat> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push("--no-renames".to_string());
    args.push(format!("{}^..{}", commit_sha, commit_sha));

    let output = match exec_git(&args) {
        Ok(o) => o,
        // For root commits (no parent), use diff-tree against empty tree
        Err(_) => {
            return diff_numstat_root_commit(repo, commit_sha);
        }
    };

    parse_numstat(&String::from_utf8_lossy(&output.stdout))
}

fn diff_numstat_root_commit(repo: &Repository, commit_sha: &str) -> Vec<FileStat> {
    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--numstat".to_string());
    args.push("--no-renames".to_string());
    args.push("-r".to_string());
    // 4b825dc: the well-known empty tree SHA
    args.push("4b825dc642cb6eb9a060e54bf899d69f82c3b3f0".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(o) => parse_numstat(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    }
}

fn parse_numstat(output: &str) -> Vec<FileStat> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                return None;
            }
            // Binary files show "-" for add/del
            let additions = parts[0].parse::<u32>().unwrap_or(0);
            let deletions = parts[1].parse::<u32>().unwrap_or(0);
            let path = parts[2].to_string();
            if additions == 0 && deletions == 0 {
                return None;
            }
            Some(FileStat {
                path,
                additions,
                deletions,
            })
        })
        .collect()
}

fn commit_author_email(repo: &Repository, commit_sha: &str) -> Option<String> {
    let mut args = repo.global_args_for_exec();
    args.push("log".to_string());
    args.push("-1".to_string());
    args.push("--format=%ae".to_string());
    args.push(commit_sha.to_string());

    exec_git(&args).ok().and_then(|o| {
        let email = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if email.is_empty() { None } else { Some(email) }
    })
}

/// Build a complete authorship note string for one commit.
fn build_authorship_note(
    commit_sha: &str,
    file_stats: &[FileStat],
    tool: &str,
    model: &str,
    session_id: &str,
    author_email: Option<&str>,
) -> String {
    let prompt_hash = generate_short_hash(session_id, tool);

    let mut total_additions: u32 = 0;
    let mut total_deletions: u32 = 0;
    let mut attestations = Vec::new();

    for stat in file_stats {
        total_additions += stat.additions;
        total_deletions += stat.deletions;

        // Attribute all added lines to the AI session
        if stat.additions > 0 {
            let range = if stat.additions == 1 {
                LineRange::Single(1)
            } else {
                LineRange::Range(1, stat.additions)
            };
            let entry = AttestationEntry::new(prompt_hash.clone(), vec![range]);
            let mut file_att = FileAttestation::new(stat.path.clone());
            file_att.add_entry(entry);
            attestations.push(file_att);
        }
    }

    let prompt_record = PromptRecord {
        agent_id: AgentId {
            tool: tool.to_string(),
            id: session_id.to_string(),
            model: model.to_string(),
        },
        human_author: author_email.map(String::from),
        messages: vec![],
        total_additions,
        total_deletions,
        accepted_lines: total_additions,
        overriden_lines: 0,
        messages_url: None,
        custom_attributes: None,
    };

    let mut prompts = BTreeMap::new();
    prompts.insert(prompt_hash, prompt_record);

    let log = AuthorshipLog {
        attestations,
        metadata: AuthorshipMetadata {
            schema_version: AUTHORSHIP_LOG_VERSION.to_string(),
            git_ai_version: Some("simulated".to_string()),
            base_commit_sha: commit_sha.to_string(),
            prompts,
        },
    };

    log.serialize_to_string().unwrap_or_else(|_| String::new())
}

// ---------------------------------------------------------------------------
// Minimal deterministic PRNG (xorshift64) – avoids pulling in `rand` for
// production code while keeping results reproducible.
// ---------------------------------------------------------------------------
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    fn next_usize(&mut self) -> usize {
        self.next_u64() as usize
    }
}
