//! Benchmark for post-commit stats slow paths.
//!
//! This benchmark reproduces the commit-time slowdown caused by stats computation
//! on commits with many changed hunks.
//!
//! Run with:
//! `cargo test benchmark_stats_hunk_density_hotspot -- --ignored --nocapture`

use git_ai::authorship::diff_ai_accepted::diff_ai_accepted_stats;
use git_ai::authorship::stats::{get_git_diff_stats, stats_for_commit_stats};
use git_ai::git::find_repository_in_path;
use serde_json::json;
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

fn plain_git_bin() -> String {
    if let Ok(custom) = std::env::var("GIT_AI_BENCH_REAL_GIT_BIN")
        && !custom.trim().is_empty()
    {
        return custom;
    }

    let candidates = [
        "/usr/bin/git",
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/bin/git",
    ];
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }

    panic!(
        "Unable to locate a plain git binary. Set GIT_AI_BENCH_REAL_GIT_BIN to an absolute path."
    )
}

fn run_git(repo_path: &Path, args: &[&str]) {
    let output = Command::new(plain_git_bin())
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

fn run_git_output(repo_path: &Path, args: &[&str]) -> String {
    let output = Command::new(plain_git_bin())
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

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write_lines(path: &Path, line_count: usize) {
    let content = (1..=line_count)
        .map(|i| format!("line {}\n", i))
        .collect::<String>();
    fs::write(path, content).expect("failed to write file");
}

fn write_pathspec_heavy_ai_checkpoint(repo_path: &Path, base_sha: &str, files: &[String]) {
    let working_log_dir = repo_path.join(".git/ai/working_logs").join(base_sha);
    fs::create_dir_all(&working_log_dir).expect("failed to create working log directory");

    let entries: Vec<serde_json::Value> = files
        .iter()
        .map(|file| {
            json!({
                "file": file,
                "blob_sha": "benchmark",
                "attributions": [],
                "line_attributions": []
            })
        })
        .collect();

    let checkpoint = json!({
        "kind": "AiAgent",
        "diff": "benchmark-pathspec-hotspot",
        "author": "Perf User",
        "entries": entries,
        "timestamp": 1_771_090_570u64,
        "transcript": serde_json::Value::Null,
        "agent_id": {
            "tool": "codex",
            "id": "benchmark-session",
            "model": "gpt-5"
        },
        "agent_metadata": {},
        "line_stats": {
            "additions": 0,
            "deletions": 0,
            "additions_sloc": 0,
            "deletions_sloc": 0
        },
        "api_version": "checkpoint/1.0.0",
        "git_ai_version": "benchmark"
    });

    fs::write(
        working_log_dir.join("checkpoints.jsonl"),
        format!("{}\n", checkpoint),
    )
    .expect("failed to write checkpoints.jsonl");
}

fn setup_repo_for_ai_pathspec_post_commit_hotspot(
    file_count: usize,
    lines_per_file: usize,
) -> TempDir {
    let tmp = TempDir::new().expect("failed to create tempdir");
    let repo = tmp.path();

    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.name", "Perf User"]);
    run_git(repo, &["config", "user.email", "perf@example.com"]);

    fs::write(repo.join("README.md"), "seed\n").expect("failed to write seed file");
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "initial"]);

    let base_sha = run_git_output(repo, &["rev-parse", "HEAD"]);

    let mut files = Vec::with_capacity(file_count);
    for i in 1..=file_count {
        let rel = format!("f{:05}.txt", i);
        write_lines(&repo.join(&rel), lines_per_file);
        files.push(rel);
    }

    // Seed AI checkpoint metadata so pre-commit tracks many AI-touched files.
    write_pathspec_heavy_ai_checkpoint(repo, &base_sha, &files);

    run_git(repo, &["add", "-A"]);
    tmp
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

fn percentile_f64(values: &[f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[rank]
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

fn benchmark_commit_with_plain_git(repo_path: &Path, message: &str) -> u64 {
    let start = Instant::now();
    let output = Command::new(plain_git_bin())
        .arg("-C")
        .arg(repo_path)
        .arg("commit")
        .arg("-m")
        .arg(message)
        .output()
        .expect("failed to execute git commit");

    assert!(
        output.status.success(),
        "plain git commit failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    start.elapsed().as_millis() as u64
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

#[test]
#[ignore] // Run manually or in CI benchmark workflow.
fn benchmark_commit_post_command_ai_pathspec_hotspot() {
    const DEFAULT_FILE_COUNT: usize = 136;
    // Keep file fanout aligned with the regression case, but use enough per-file payload
    // so plain git commit time is not dominated by process-launch noise.
    const DEFAULT_LINES_PER_FILE: usize = 10_000;
    const DEFAULT_RUNS: usize = 3;
    const DEFAULT_MAX_SLOWDOWN_RATIO: f64 = 2.0;

    let file_count = std::env::var("GIT_AI_BENCH_PATHSPEC_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_FILE_COUNT);
    let lines_per_file = std::env::var("GIT_AI_BENCH_PATHSPEC_LINES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LINES_PER_FILE);
    let runs = std::env::var("GIT_AI_BENCH_RATIO_RUNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_RUNS);
    let max_slowdown_ratio = std::env::var("GIT_AI_BENCH_MAX_SLOWDOWN_RATIO")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MAX_SLOWDOWN_RATIO);

    assert!(runs > 0, "GIT_AI_BENCH_RATIO_RUNS must be >= 1");

    let mut plain_git_runs_ms = Vec::with_capacity(runs);
    let mut git_ai_runs_ms = Vec::with_capacity(runs);
    let mut slowdown_ratios = Vec::with_capacity(runs);
    let mut last_perf = None;

    for idx in 0..runs {
        let plain_repo = setup_repo_for_ai_pathspec_post_commit_hotspot(file_count, lines_per_file);
        let git_ai_repo =
            setup_repo_for_ai_pathspec_post_commit_hotspot(file_count, lines_per_file);

        let plain_git_ms =
            benchmark_commit_with_plain_git(plain_repo.path(), "pathspec hotspot plain");
        let perf = benchmark_commit_with_git_ai(git_ai_repo.path(), "pathspec hotspot git-ai");

        let slowdown_ratio = perf.total_ms as f64 / (plain_git_ms as f64).max(1.0);
        println!(
            "run {} -> plain={}ms git-ai={}ms ratio={:.3}x",
            idx + 1,
            plain_git_ms,
            perf.total_ms,
            slowdown_ratio
        );

        plain_git_runs_ms.push(plain_git_ms as f64);
        git_ai_runs_ms.push(perf.total_ms as f64);
        slowdown_ratios.push(slowdown_ratio);
        last_perf = Some(perf);
    }

    let median_slowdown_ratio = percentile_f64(&slowdown_ratios, 0.50);
    let p95_slowdown_ratio = percentile_f64(&slowdown_ratios, 0.95);
    let median_plain_git_ms = percentile_f64(&plain_git_runs_ms, 0.50);
    let median_git_ai_ms = percentile_f64(&git_ai_runs_ms, 0.50);
    let perf = last_perf.expect("at least one benchmark run must execute");

    println!("\n=== Commit Benchmark: AI Pathspec Hotspot ===");
    println!("plain_git_bin:         {}", plain_git_bin());
    println!("files:                 {}", file_count);
    println!("lines_per_file:        {}", lines_per_file);
    println!("runs:                  {}", runs);
    println!("plain_git_median:      {:.0}ms", median_plain_git_ms);
    println!("git_ai_median:         {:.0}ms", median_git_ai_ms);
    println!("slowdown_ratio_median: {:.3}x", median_slowdown_ratio);
    println!("slowdown_ratio_p95:    {:.3}x", p95_slowdown_ratio);
    println!("last_pre_command:      {}ms", perf.pre_command_ms);
    println!("last_git_command:      {}ms", perf.git_ms);
    println!("last_post_command:     {}ms", perf.post_command_ms);
    println!("last_total:            {}ms", perf.total_ms);
    println!("max_slowdown_ratio:    {:.3}x", max_slowdown_ratio);

    assert!(
        median_slowdown_ratio <= max_slowdown_ratio,
        "git-ai median slowdown {:.3}x exceeded max {:.3}x (median git-ai={:.0}ms, median plain={:.0}ms, p95 slowdown={:.3}x, runs={})",
        median_slowdown_ratio,
        max_slowdown_ratio,
        median_git_ai_ms,
        median_plain_git_ms,
        p95_slowdown_ratio,
        runs
    );
}
