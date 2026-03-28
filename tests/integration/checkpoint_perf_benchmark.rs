//! Checkpoint performance benchmarks for measuring optimization impact.
//!
//! Covers:
//! - Human checkpoints (single file, multi-file)
//! - AI agent checkpoints with file-scoped paths (mock_ai)
//! - Agent checkpoints with accumulated history (multiple rounds)
//! - Agent checkpoints with popular agent fixtures (Claude, Cursor)
//!
//! Run with: cargo test checkpoint_perf_benchmark --release -- --nocapture --ignored

use crate::repos::test_repo::TestRepo;
use crate::test_utils::fixture_path;
use serde_json::json;
use std::fs;
use std::time::{Duration, Instant};

fn median_duration(durations: &[Duration]) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

fn print_stats(label: &str, durations: &[Duration]) {
    let med = median_duration(durations);
    let min = durations.iter().min().unwrap();
    let max = durations.iter().max().unwrap();
    println!(
        "  {:<50} median={:>7.2}ms  min={:>7.2}ms  max={:>7.2}ms",
        label,
        med.as_secs_f64() * 1000.0,
        min.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0,
    );
}

/// Benchmark: AI agent checkpoint on a single file (file-scoped, mock_ai)
fn bench_single_file_ai_checkpoint(repo: &TestRepo, file_name: &str, iteration: usize) -> Duration {
    // Modify the file
    let file_path = repo.path().join(file_name);
    let content = format!(
        "ai generated line iteration {}\nmore code\nfunction foo() {{}}\n",
        iteration
    );
    fs::write(&file_path, content).unwrap();

    let start = Instant::now();
    repo.git_ai(&["checkpoint", "mock_ai", file_name])
        .expect("checkpoint should succeed");
    start.elapsed()
}

/// Benchmark: Human checkpoint on a single file
fn bench_single_file_human_checkpoint(
    repo: &TestRepo,
    file_name: &str,
    iteration: usize,
) -> Duration {
    let file_path = repo.path().join(file_name);
    let content = format!(
        "human edit iteration {}\nsome code\nfunction bar() {{}}\n",
        iteration
    );
    fs::write(&file_path, content).unwrap();

    let start = Instant::now();
    repo.git_ai(&["checkpoint"])
        .expect("checkpoint should succeed");
    start.elapsed()
}

/// Benchmark: AI agent checkpoint on multiple files (file-scoped, mock_ai)
fn bench_multi_file_ai_checkpoint(
    repo: &TestRepo,
    file_count: usize,
    iteration: usize,
) -> Duration {
    let mut file_names = Vec::with_capacity(file_count);
    for i in 0..file_count {
        let name = format!("src/module_{}.rs", i);
        let file_path = repo.path().join(&name);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let content = format!(
            "// Module {} iteration {}\npub fn func_{}() -> i32 {{ {} }}\n",
            i, iteration, i, iteration
        );
        fs::write(&file_path, content).unwrap();
        file_names.push(name);
    }

    let mut args: Vec<&str> = vec!["checkpoint", "mock_ai"];
    for name in &file_names {
        args.push(name);
    }

    let start = Instant::now();
    repo.git_ai(&args).expect("checkpoint should succeed");
    start.elapsed()
}

/// Benchmark: Claude agent checkpoint using real fixture
fn bench_claude_checkpoint(repo: &TestRepo, file_name: &str, iteration: usize) -> Duration {
    let file_path = repo.path().join(file_name);
    let content = format!(
        "claude generated code iteration {}\nconst x = {};\n",
        iteration, iteration
    );
    fs::write(&file_path, content).unwrap();

    let transcript_path = fixture_path("example-claude-code.jsonl");
    let hook_input = json!({
        "cwd": repo.canonical_path().to_string_lossy().to_string(),
        "hook_event_name": "PostToolUse",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "tool_input": {
            "file_path": file_path.to_string_lossy().to_string()
        }
    })
    .to_string();

    let start = Instant::now();
    repo.git_ai(&["checkpoint", "claude", "--hook-input", &hook_input])
        .expect("checkpoint should succeed");
    start.elapsed()
}

#[test]
#[ignore]
fn checkpoint_perf_benchmark_single_file_ai() {
    const WARMUP: usize = 2;
    const ITERATIONS: usize = 10;

    println!("\n=== Single File AI Checkpoint (mock_ai, file-scoped) ===");
    let repo = TestRepo::new();
    let file_name = "target_file.rs";
    fs::write(repo.path().join(file_name), "initial\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();

    // Warmup
    for i in 0..WARMUP {
        bench_single_file_ai_checkpoint(&repo, file_name, i);
    }
    repo.stage_all_and_commit("warmup").unwrap();

    let repo = TestRepo::new();
    fs::write(repo.path().join(file_name), "initial\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();

    let mut durations = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let d = bench_single_file_ai_checkpoint(&repo, file_name, i);
        durations.push(d);
    }
    print_stats("single_file_ai_checkpoint", &durations);
}

#[test]
#[ignore]
fn checkpoint_perf_benchmark_single_file_human() {
    const ITERATIONS: usize = 10;

    println!("\n=== Single File Human Checkpoint ===");
    let repo = TestRepo::new();
    let file_name = "human_file.rs";

    // Need an AI checkpoint first so human checkpoints have work to do
    fs::write(repo.path().join(file_name), "initial ai code\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", file_name]).unwrap();
    repo.stage_all_and_commit("init with ai").unwrap();

    let mut durations = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let d = bench_single_file_human_checkpoint(&repo, file_name, i);
        durations.push(d);
    }
    print_stats("single_file_human_checkpoint", &durations);
}

#[test]
#[ignore]
fn checkpoint_perf_benchmark_multi_file_ai() {
    println!("\n=== Multi-File AI Checkpoint (mock_ai, file-scoped) ===");
    for file_count in [5, 10, 20] {
        let repo = TestRepo::new();
        // Create initial files
        for i in 0..file_count {
            let name = format!("src/module_{}.rs", i);
            let file_path = repo.path().join(&name);
            fs::create_dir_all(file_path.parent().unwrap()).unwrap();
            fs::write(&file_path, format!("// module {}\n", i)).unwrap();
        }
        repo.stage_all_and_commit("init").unwrap();

        const ITERATIONS: usize = 5;
        let mut durations = Vec::with_capacity(ITERATIONS);
        for i in 0..ITERATIONS {
            let d = bench_multi_file_ai_checkpoint(&repo, file_count, i);
            durations.push(d);
        }
        print_stats(
            &format!("multi_file_ai_checkpoint({}files)", file_count),
            &durations,
        );
    }
}

#[test]
#[ignore]
fn checkpoint_perf_benchmark_accumulated_history() {
    println!("\n=== AI Checkpoint with Accumulated History ===");
    let repo = TestRepo::new();
    let file_name = "evolving_file.rs";
    fs::write(repo.path().join(file_name), "initial\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();

    // Build up checkpoint history (5, 10, 20 accumulated checkpoints)
    let mut all_durations: Vec<(usize, Duration)> = Vec::new();
    for i in 0..25 {
        let content = format!("// version {}\npub fn v{}() -> i32 {{ {} }}\n", i, i, i);
        fs::write(repo.path().join(file_name), content).unwrap();

        let start = Instant::now();
        repo.git_ai(&["checkpoint", "mock_ai", file_name])
            .expect("checkpoint should succeed");
        let d = start.elapsed();
        all_durations.push((i + 1, d));
    }

    // Report at milestones
    for &milestone in &[5usize, 10, 15, 20, 25] {
        let bucket: Vec<Duration> = all_durations
            .iter()
            .filter(|(idx, _)| *idx > milestone.saturating_sub(5) && *idx <= milestone)
            .map(|(_, d)| *d)
            .collect();
        if !bucket.is_empty() {
            print_stats(
                &format!(
                    "accumulated_history(checkpoints {}-{})",
                    milestone - 4,
                    milestone
                ),
                &bucket,
            );
        }
    }
}

#[test]
#[ignore]
fn checkpoint_perf_benchmark_claude_agent() {
    const ITERATIONS: usize = 8;

    println!("\n=== Claude Agent Checkpoint (real fixture) ===");
    let repo = TestRepo::new();
    let file_name = "claude_output.ts";
    fs::write(repo.path().join(file_name), "// initial\n").unwrap();
    repo.stage_all_and_commit("init").unwrap();

    let mut durations = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let d = bench_claude_checkpoint(&repo, file_name, i);
        durations.push(d);
    }
    print_stats("claude_agent_checkpoint", &durations);
}

/// Combined benchmark that produces a single summary table
#[test]
#[ignore]
fn checkpoint_perf_benchmark_summary() {
    println!("\n╔══════════════════════════════════════════════════════════════════════════╗");
    println!("║                    CHECKPOINT PERFORMANCE BENCHMARK                     ║");
    println!("╚══════════════════════════════════════════════════════════════════════════╝\n");

    const ITERS: usize = 8;

    // --- 1. Single file AI (mock_ai) ---
    {
        let repo = TestRepo::new();
        let f = "target.rs";
        fs::write(repo.path().join(f), "init\n").unwrap();
        repo.stage_all_and_commit("init").unwrap();
        let mut ds = Vec::new();
        for i in 0..ITERS {
            ds.push(bench_single_file_ai_checkpoint(&repo, f, i));
        }
        print_stats("1-file AI checkpoint (mock_ai)", &ds);
    }

    // --- 2. Multi-file AI (10 files) ---
    {
        let repo = TestRepo::new();
        for i in 0..10 {
            let name = format!("src/m{}.rs", i);
            fs::create_dir_all(repo.path().join("src")).unwrap();
            fs::write(repo.path().join(&name), format!("// m{}\n", i)).unwrap();
        }
        repo.stage_all_and_commit("init").unwrap();
        let mut ds = Vec::new();
        for i in 0..ITERS {
            ds.push(bench_multi_file_ai_checkpoint(&repo, 10, i));
        }
        print_stats("10-file AI checkpoint (mock_ai)", &ds);
    }

    // --- 3. Single file human ---
    {
        let repo = TestRepo::new();
        let f = "human.rs";
        fs::write(repo.path().join(f), "init\n").unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
        repo.stage_all_and_commit("init ai").unwrap();
        let mut ds = Vec::new();
        for i in 0..ITERS {
            ds.push(bench_single_file_human_checkpoint(&repo, f, i));
        }
        print_stats("1-file human checkpoint", &ds);
    }

    // --- 4. Claude agent ---
    {
        let repo = TestRepo::new();
        let f = "claude.ts";
        fs::write(repo.path().join(f), "// init\n").unwrap();
        repo.stage_all_and_commit("init").unwrap();
        let mut ds = Vec::new();
        for i in 0..ITERS {
            ds.push(bench_claude_checkpoint(&repo, f, i));
        }
        print_stats("1-file Claude agent checkpoint", &ds);
    }

    // --- 5. Accumulated history (20 checkpoints then measure) ---
    {
        let repo = TestRepo::new();
        let f = "accum.rs";
        fs::write(repo.path().join(f), "init\n").unwrap();
        repo.stage_all_and_commit("init").unwrap();
        // Build up 20 checkpoints
        for i in 0..20 {
            fs::write(repo.path().join(f), format!("v{}\ncode\n", i)).unwrap();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
        }
        // Now measure
        let mut ds = Vec::new();
        for i in 20..20 + ITERS {
            fs::write(repo.path().join(f), format!("v{}\ncode\n", i)).unwrap();
            let start = Instant::now();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
            ds.push(start.elapsed());
        }
        print_stats("1-file AI after 20 accumulated checkpoints", &ds);
    }

    // --- 6. Accumulated history (50 checkpoints then measure) ---
    {
        let repo = TestRepo::new();
        let f = "accum50.rs";
        fs::write(repo.path().join(f), "init\n").unwrap();
        repo.stage_all_and_commit("init").unwrap();
        // Build up 50 checkpoints
        for i in 0..50 {
            fs::write(repo.path().join(f), format!("v{}\ncode line\n", i)).unwrap();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
        }
        let mut ds = Vec::new();
        for i in 50..50 + ITERS {
            fs::write(repo.path().join(f), format!("v{}\ncode line\n", i)).unwrap();
            let start = Instant::now();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
            ds.push(start.elapsed());
        }
        print_stats("1-file AI after 50 accumulated checkpoints", &ds);
    }

    // --- 7. Larger file (200 lines, AI checkpoint) ---
    {
        let repo = TestRepo::new();
        let f = "large.rs";
        let mut content = String::new();
        for i in 0..200 {
            content.push_str(&format!("pub fn func_{}() -> i32 {{ {} }}\n", i, i));
        }
        fs::write(repo.path().join(f), &content).unwrap();
        repo.stage_all_and_commit("init").unwrap();
        let mut ds = Vec::new();
        for iter in 0..ITERS {
            let mut new_content = String::new();
            for i in 0..200 {
                new_content.push_str(&format!("pub fn func_{}() -> i32 {{ {} }}\n", i, i + iter));
            }
            fs::write(repo.path().join(f), &new_content).unwrap();
            let start = Instant::now();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
            ds.push(start.elapsed());
        }
        print_stats("200-line file AI checkpoint", &ds);
    }

    // --- 8. Larger file with accumulated history ---
    {
        let repo = TestRepo::new();
        let f = "large_accum.rs";
        let init_content: String = (0..200)
            .map(|i| format!("pub fn func_{}() -> i32 {{ 0 }}\n", i))
            .collect();
        fs::write(repo.path().join(f), &init_content).unwrap();
        repo.stage_all_and_commit("init").unwrap();
        // Build up 20 checkpoints on a 200-line file
        for cp in 0..20 {
            let content: String = (0..200)
                .map(|i| format!("pub fn func_{}() -> i32 {{ {} }}\n", i, cp))
                .collect();
            fs::write(repo.path().join(f), &content).unwrap();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
        }
        let mut ds = Vec::new();
        for iter in 20..20 + ITERS {
            let content: String = (0..200)
                .map(|i| format!("pub fn func_{}() -> i32 {{ {} }}\n", i, iter))
                .collect();
            fs::write(repo.path().join(f), &content).unwrap();
            let start = Instant::now();
            repo.git_ai(&["checkpoint", "mock_ai", f]).unwrap();
            ds.push(start.elapsed());
        }
        print_stats("200-line file AI after 20 accumulated CPs", &ds);
    }

    println!("\n══════════════════════════════════════════════════════════════════════════\n");
}
