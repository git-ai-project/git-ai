//! Benchmark for post-commit stats slow paths.
//!
//! This benchmark reproduces the commit-time slowdown caused by stats computation
//! on commits with many changed hunks.
//!
//! Run with:
//! `cargo test benchmark_stats_hunk_density_hotspot -- --ignored --nocapture`

use git_ai::authorship::diff_ai_accepted::diff_ai_accepted_stats;
use git_ai::authorship::post_commit::estimate_stats_cost_for_head;
use git_ai::authorship::stats::{get_git_diff_stats, stats_for_commit_stats};
use git_ai::git::find_repository_in_path;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[derive(Debug)]
struct StatsBreakdown {
    git_numstat: Duration,
    diff_ai_accepted: Duration,
    total_stats: Duration,
}

#[derive(Debug)]
struct CommitPerfBreakdown {
    pre_command_ms: u64,
    git_ms: u64,
    post_command_ms: u64,
    total_ms: u64,
}

fn run_git(repo_path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .expect("failed to execute git command");

    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_lines(path: &Path, line_count: usize) {
    let content = (1..=line_count)
        .map(|i| format!("line {}\n", i))
        .collect::<String>();
    fs::write(path, content).expect("failed to write file");
}

fn mutate_file_with_scattered_replacements(path: &Path, up_to_line: usize, every_n: usize) {
    let content = fs::read_to_string(path).expect("failed to read file");
    let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();

    for (idx, line) in lines.iter_mut().enumerate() {
        let line_no = idx + 1;
        if line_no <= up_to_line && line_no % every_n == 0 {
            line.push_str(" changed");
        }
    }

    let new_content = lines.join("\n") + "\n";
    fs::write(path, new_content).expect("failed to write mutated file");
}

fn append_block(path: &Path, lines: usize) {
    let mut content = fs::read_to_string(path).expect("failed to read file");
    for i in 1..=lines {
        content.push_str(&format!("new {}\n", i));
    }
    fs::write(path, content).expect("failed to append block");
}

fn setup_repo_with_many_changed_files(file_count: usize) -> TempDir {
    let tmp = TempDir::new().expect("failed to create tempdir");
    let repo = tmp.path();

    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.name", "Perf User"]);
    run_git(repo, &["config", "user.email", "perf@example.com"]);

    for i in 1..=file_count {
        fs::write(repo.join(format!("f{:05}.txt", i)), "base\n").expect("failed to write file");
    }
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "initial"]);

    for i in 1..=file_count {
        fs::write(
            repo.join(format!("f{:05}.txt", i)),
            format!("base\nchanged {}\n", i),
        )
        .expect("failed to write changed file");
    }
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "thousands-of-files-workload"]);

    tmp
}

fn setup_repo_and_commit(case: &str) -> TempDir {
    let tmp = TempDir::new().expect("failed to create tempdir");
    let repo = tmp.path();

    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.name", "Perf User"]);
    run_git(repo, &["config", "user.email", "perf@example.com"]);

    match case {
        // Many files, one contiguous added block per file (low hunk density)
        "many_files_contiguous" => {
            for i in 1..=80 {
                let path = repo.join(format!("f{}.txt", i));
                write_lines(&path, 200);
            }
            run_git(repo, &["add", "-A"]);
            run_git(repo, &["commit", "-q", "-m", "initial"]);

            for i in 1..=80 {
                let path = repo.join(format!("f{}.txt", i));
                append_block(&path, 20);
            }
        }
        // Many files, many one-line replacements per file (high hunk density)
        "many_files_scattered" => {
            for i in 1..=60 {
                let path = repo.join(format!("m{}.txt", i));
                write_lines(&path, 240);
            }
            run_git(repo, &["add", "-A"]);
            run_git(repo, &["commit", "-q", "-m", "initial"]);

            for i in 1..=60 {
                let path = repo.join(format!("m{}.txt", i));
                mutate_file_with_scattered_replacements(&path, 200, 4);
            }
        }
        _ => panic!("unknown benchmark case: {}", case),
    }

    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "workload"]);
    tmp
}

fn benchmark_stats(repo_path: &Path) -> StatsBreakdown {
    let repo = find_repository_in_path(repo_path.to_str().expect("non-utf8 path"))
        .expect("failed to open repository");

    let head_sha = repo
        .head()
        .expect("failed to get HEAD")
        .target()
        .expect("failed to resolve HEAD target");

    let parent_sha = repo
        .find_commit(head_sha.clone())
        .expect("failed to find HEAD commit")
        .parent(0)
        .expect("failed to find parent")
        .id();

    let git_numstat_start = Instant::now();
    let _git_numstat = get_git_diff_stats(&repo, &head_sha, &[]).expect("git numstat failed");
    let git_numstat = git_numstat_start.elapsed();

    let diff_ai_start = Instant::now();
    let _diff_ai = diff_ai_accepted_stats(&repo, &parent_sha, &head_sha, Some(&parent_sha), &[])
        .expect("diff_ai_accepted_stats failed");
    let diff_ai_accepted = diff_ai_start.elapsed();

    let total_stats_start = Instant::now();
    let _stats = stats_for_commit_stats(&repo, &head_sha, &[]).expect("stats_for_commit_stats");
    let total_stats = total_stats_start.elapsed();

    StatsBreakdown {
        git_numstat,
        diff_ai_accepted,
        total_stats,
    }
}

fn percentile_ms(durations: &[Duration], percentile: f64) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<Duration> = durations.to_vec();
    sorted.sort_unstable();
    let rank = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[rank].as_secs_f64() * 1000.0
}

fn git_ai_bin() -> String {
    std::env::var("CARGO_BIN_EXE_git-ai")
        .unwrap_or_else(|_| format!("{}/target/debug/git-ai", env!("CARGO_MANIFEST_DIR")))
}

fn benchmark_commit_with_git_ai(repo_path: &Path, message: &str) -> CommitPerfBreakdown {
    let output = Command::new(git_ai_bin())
        .arg("-C")
        .arg(repo_path)
        .arg("commit")
        .arg("-m")
        .arg(message)
        .env("GIT_AI", "git")
        .env("GIT_AI_DEBUG_PERFORMANCE", "2")
        .output()
        .expect("failed to execute git-ai commit");

    assert!(
        output.status.success(),
        "git-ai commit failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let perf_json_line = combined
        .lines()
        .find(|line| line.contains("[git-ai (perf-json)]"))
        .expect("missing perf-json output from git-ai commit");

    let json_start = perf_json_line
        .find('{')
        .expect("perf-json line missing JSON payload");
    let perf_value: serde_json::Value =
        serde_json::from_str(&perf_json_line[json_start..]).expect("invalid perf JSON");

    CommitPerfBreakdown {
        pre_command_ms: perf_value["pre_command_duration_ms"].as_u64().unwrap_or(0),
        git_ms: perf_value["git_duration_ms"].as_u64().unwrap_or(0),
        post_command_ms: perf_value["post_command_duration_ms"].as_u64().unwrap_or(0),
        total_ms: perf_value["total_duration_ms"].as_u64().unwrap_or(0),
    }
}

#[test]
#[ignore] // Run manually; this is intentionally expensive.
fn benchmark_stats_hunk_density_hotspot() {
    let contiguous_repo = setup_repo_and_commit("many_files_contiguous");
    let scattered_repo = setup_repo_and_commit("many_files_scattered");

    let contiguous = benchmark_stats(contiguous_repo.path());
    let scattered = benchmark_stats(scattered_repo.path());

    println!("\n=== Stats Benchmark: Contiguous Changes ===");
    println!(
        "git numstat:          {:>8.2}ms",
        contiguous.git_numstat.as_secs_f64() * 1000.0
    );
    println!(
        "diff_ai_accepted:     {:>8.2}ms",
        contiguous.diff_ai_accepted.as_secs_f64() * 1000.0
    );
    println!(
        "total stats_for_commit_stats: {:>8.2}ms",
        contiguous.total_stats.as_secs_f64() * 1000.0
    );

    println!("\n=== Stats Benchmark: Scattered Changes ===");
    println!(
        "git numstat:          {:>8.2}ms",
        scattered.git_numstat.as_secs_f64() * 1000.0
    );
    println!(
        "diff_ai_accepted:     {:>8.2}ms",
        scattered.diff_ai_accepted.as_secs_f64() * 1000.0
    );
    println!(
        "total stats_for_commit_stats: {:>8.2}ms",
        scattered.total_stats.as_secs_f64() * 1000.0
    );

    // Sanity check: the diff_ai_accepted hotspot should dominate in the scattered case.
    assert!(scattered.diff_ai_accepted > contiguous.diff_ai_accepted);

    // stats_for_commit_stats no longer uses diff_ai_accepted, so total_stats may be very close
    // between contiguous and scattered workloads. Keep a broad upper bound to catch regressions.
    assert!(contiguous.total_stats.as_secs_f64() * 1000.0 < 500.0);
    assert!(scattered.total_stats.as_secs_f64() * 1000.0 < 500.0);
}

#[test]
#[ignore] // Run manually; this is intentionally expensive.
fn benchmark_commit_post_command_hunk_density_hotspot() {
    // Setup and stage contiguous case (without committing workload yet)
    let contiguous_repo = TempDir::new().expect("failed to create tempdir");
    let contiguous_path = contiguous_repo.path();
    run_git(contiguous_path, &["init", "-q"]);
    run_git(contiguous_path, &["config", "user.name", "Perf User"]);
    run_git(
        contiguous_path,
        &["config", "user.email", "perf@example.com"],
    );
    for i in 1..=80 {
        write_lines(&contiguous_path.join(format!("f{}.txt", i)), 200);
    }
    run_git(contiguous_path, &["add", "-A"]);
    run_git(contiguous_path, &["commit", "-q", "-m", "initial"]);
    for i in 1..=80 {
        append_block(&contiguous_path.join(format!("f{}.txt", i)), 20);
    }
    run_git(contiguous_path, &["add", "-A"]);

    // Setup and stage scattered case
    let scattered_repo = TempDir::new().expect("failed to create tempdir");
    let scattered_path = scattered_repo.path();
    run_git(scattered_path, &["init", "-q"]);
    run_git(scattered_path, &["config", "user.name", "Perf User"]);
    run_git(
        scattered_path,
        &["config", "user.email", "perf@example.com"],
    );
    for i in 1..=60 {
        write_lines(&scattered_path.join(format!("m{}.txt", i)), 240);
    }
    run_git(scattered_path, &["add", "-A"]);
    run_git(scattered_path, &["commit", "-q", "-m", "initial"]);
    for i in 1..=60 {
        mutate_file_with_scattered_replacements(
            &scattered_path.join(format!("m{}.txt", i)),
            200,
            4,
        );
    }
    run_git(scattered_path, &["add", "-A"]);

    let contiguous_perf = benchmark_commit_with_git_ai(contiguous_path, "contiguous");
    let scattered_perf = benchmark_commit_with_git_ai(scattered_path, "scattered");

    println!("\n=== Commit Benchmark: Contiguous Changes ===");
    println!("pre_command:  {}ms", contiguous_perf.pre_command_ms);
    println!("git command:  {}ms", contiguous_perf.git_ms);
    println!("post_command: {}ms", contiguous_perf.post_command_ms);
    println!("total:        {}ms", contiguous_perf.total_ms);

    println!("\n=== Commit Benchmark: Scattered Changes ===");
    println!("pre_command:  {}ms", scattered_perf.pre_command_ms);
    println!("git command:  {}ms", scattered_perf.git_ms);
    println!("post_command: {}ms", scattered_perf.post_command_ms);
    println!("total:        {}ms", scattered_perf.total_ms);

    assert!(contiguous_perf.total_ms > 0);
    assert!(scattered_perf.total_ms > 0);
}

#[test]
#[ignore] // Run manually; this is intentionally expensive.
fn benchmark_stats_thousands_changed_files_fast_path() {
    const DEFAULT_FILE_COUNT: usize = 3_000;
    const DEFAULT_RUNS: usize = 5;
    const DEFAULT_MAX_AVG_MS: f64 = 3_000.0;

    let file_count = std::env::var("GIT_AI_BENCH_FILE_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_FILE_COUNT);
    let runs_count = std::env::var("GIT_AI_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_RUNS);

    let max_avg_ms = std::env::var("GIT_AI_BENCH_MAX_AVG_MS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MAX_AVG_MS);

    let tmp = setup_repo_with_many_changed_files(file_count);
    let repo = find_repository_in_path(tmp.path().to_str().expect("non-utf8 path"))
        .expect("failed to open repository");
    let head_sha = repo
        .head()
        .expect("failed to get HEAD")
        .target()
        .expect("failed to resolve HEAD target");

    // Warm-up to avoid one-time setup noise.
    let warmup_stats = stats_for_commit_stats(&repo, &head_sha, &[]).expect("warmup stats failed");
    assert_eq!(
        warmup_stats.git_diff_added_lines, file_count as u32,
        "expected one added line per changed file"
    );

    let mut runs = Vec::with_capacity(runs_count);
    for _ in 0..runs_count {
        let start = Instant::now();
        let stats = stats_for_commit_stats(&repo, &head_sha, &[]).expect("stats_for_commit_stats");
        let elapsed = start.elapsed();
        assert_eq!(stats.git_diff_added_lines, file_count as u32);
        runs.push(elapsed);
    }

    let total: Duration = runs.iter().copied().sum();
    let avg = total / runs_count as u32;
    let avg_ms = avg.as_secs_f64() * 1000.0;
    let p95_ms = percentile_ms(&runs, 0.95);
    let max_ms = runs
        .iter()
        .max()
        .copied()
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        * 1000.0;

    println!("\n=== Stats Benchmark: Thousands of Changed Files ===");
    println!("files_changed: {}", file_count);
    println!("runs: {}", runs_count);
    println!("avg_ms: {:.2}", avg_ms);
    println!("p95_ms: {:.2}", p95_ms);
    println!("max_ms: {:.2}", max_ms);
    println!("max_avg_budget_ms: {:.2}", max_avg_ms);

    assert!(
        avg_ms <= max_avg_ms,
        "stats_for_commit_stats average {:.2}ms exceeded budget {:.2}ms on {} changed files",
        avg_ms,
        max_avg_ms,
        file_count
    );
}

/// Build a repo that matches the user-reported hang pattern:
/// many files deleted, each with many lines, very few (or no) additions.
/// Mirrors the ~124 files / ~91K deletions / ~22 additions from:
/// 537f4caca27837aa7aa729dbdbcf954fdab856ff (~/projects/monorepo)
fn setup_repo_with_mass_deletion(file_count: usize, lines_per_file: usize) -> TempDir {
    let tmp = TempDir::new().expect("failed to create tempdir");
    let repo = tmp.path();

    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.name", "Perf User"]);
    run_git(repo, &["config", "user.email", "perf@example.com"]);

    // Create many files with many lines each
    for i in 1..=file_count {
        let content = (1..=lines_per_file)
            .map(|l| format!("file{:03} line {:04}\n", i, l))
            .collect::<String>();
        fs::write(repo.join(format!("f{:03}.txt", i)), content).expect("failed to write file");
    }
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "initial"]);

    // Delete all files — this is the commit that triggers the hang
    for i in 1..=file_count {
        fs::remove_file(repo.join(format!("f{:03}.txt", i))).expect("failed to delete file");
    }
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "mass-deletion"]);

    tmp
}

/// Regression test: the stats-cost estimator must flag a mass-deletion commit
/// (many files deleted, no additions) as expensive so the post-commit hook
/// skips the full stats pass.
///
/// Previously `estimate_stats_cost_for_head` only counted *added* lines, so a
/// commit deleting 100 files × 750 lines (= 75 000 deletions, 0 additions) was
/// classified as cheap.  The hook then ran `get_diff_with_line_numbers` which
/// had to parse and allocate storage for all 75 000 deleted-content lines,
/// causing a multi-second hang that users perceived as git-ai freezing.
#[test]
fn estimate_stats_cost_skips_mass_deletion_commit() {
    // 100 files × 750 lines ≈ 75 000 deleted lines, zero additions.
    // Matches the scale of the real-world hang (537f4cac in ~/projects/monorepo).
    let tmp = setup_repo_with_mass_deletion(100, 750);
    let repo = find_repository_in_path(tmp.path().to_str().expect("non-utf8 path"))
        .expect("failed to open repository");

    let head_sha = repo
        .head()
        .expect("failed to get HEAD")
        .target()
        .expect("failed to resolve HEAD target");

    let estimate = estimate_stats_cost_for_head(&repo, &head_sha, &[])
        .expect("estimate_stats_cost_for_head should succeed");

    assert!(
        estimate.should_skip(),
        "expected estimate_stats_cost_for_head to flag a 75K-deletion commit as \
         expensive (should_skip=true), but got should_skip=false. \
         The estimator only counted added lines and missed the large deletion cost."
    );
}

/// Performance guard: post-commit stats for a mass-deletion commit must
/// complete within 2 seconds now that the fast path is taken.
#[test]
#[ignore] // Run manually: cargo test benchmark_stats_mass_deletion_fast_path -- --ignored --nocapture
fn benchmark_stats_mass_deletion_fast_path() {
    const FILE_COUNT: usize = 100;
    const LINES_PER_FILE: usize = 750;
    const MAX_MS: f64 = 2_000.0;

    let tmp = setup_repo_with_mass_deletion(FILE_COUNT, LINES_PER_FILE);
    let repo = find_repository_in_path(tmp.path().to_str().expect("non-utf8 path"))
        .expect("failed to open repository");

    let head_sha = repo
        .head()
        .expect("failed to get HEAD")
        .target()
        .expect("failed to resolve HEAD target");

    let ignore_patterns: Vec<String> = vec![];

    let start = Instant::now();
    let _stats = stats_for_commit_stats(&repo, &head_sha, &ignore_patterns)
        .expect("stats_for_commit_stats should succeed");
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    println!(
        "\n=== Stats Benchmark: Mass File Deletion ({} files × {} lines) ===",
        FILE_COUNT, LINES_PER_FILE
    );
    println!("elapsed: {:.2}ms", elapsed_ms);
    println!("budget:  {:.2}ms", MAX_MS);

    assert!(
        elapsed_ms <= MAX_MS,
        "stats_for_commit_stats took {:.2}ms for mass-deletion commit, expected <= {:.2}ms",
        elapsed_ms,
        MAX_MS
    );
}
