//! Benchmark for git-ai status performance on mass deletions.
//!
//! Run with:
//!   cargo test test_status_mass_delete_benchmark --release -- --nocapture --ignored
//!
//! You can tune scale with:
//!   GIT_AI_BENCH_DELETE_FILES=400
//!   GIT_AI_BENCH_DELETE_LINES=200
//!   GIT_AI_BENCH_DELETE_LINE_LEN=80

mod repos;

use repos::test_repo::TestRepo;
use std::env;
use std::fs;
use std::time::Instant;

fn env_usize(key: &str, default_value: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default_value)
}

fn make_content(lines_per_file: usize, line_len: usize) -> String {
    let mut content = String::with_capacity(lines_per_file * (line_len + 1));
    for i in 0..lines_per_file {
        let mut line = format!("line {:06} ", i);
        if line.len() < line_len {
            line.push_str(&"x".repeat(line_len - line.len()));
        } else {
            line.truncate(line_len);
        }
        content.push_str(&line);
        content.push('\n');
    }
    content
}

#[test]
#[ignore] // Run with --ignored flag since this is a benchmark
fn test_status_mass_delete_benchmark() {
    let file_count = env_usize("GIT_AI_BENCH_DELETE_FILES", 400);
    let lines_per_file = env_usize("GIT_AI_BENCH_DELETE_LINES", 200);
    let line_len = env_usize("GIT_AI_BENCH_DELETE_LINE_LEN", 80);
    let bench_dir = "vendor/bench-delete";

    println!("\n========================================");
    println!("git-ai status mass-delete benchmark");
    println!("========================================");
    println!(
        "Files: {}, lines/file: {}, line_len: {}",
        file_count, lines_per_file, line_len
    );

    let repo = TestRepo::new();
    let repo_path = repo.canonical_path();
    let target_dir = repo_path.join(bench_dir);
    fs::create_dir_all(&target_dir).expect("Failed to create bench dir");

    let content = make_content(lines_per_file, line_len);
    let bytes_per_file = content.len();
    let total_bytes = bytes_per_file.saturating_mul(file_count);
    println!(
        "Approx size: {:.2} MB total ({} bytes/file)",
        total_bytes as f64 / (1024.0 * 1024.0),
        bytes_per_file
    );

    for i in 0..file_count {
        let file_path = target_dir.join(format!("file_{:05}.txt", i));
        fs::write(&file_path, &content).expect("Failed to write bench file");
    }

    repo.stage_all_and_commit("Add bench-delete fixture")
        .expect("Initial commit should succeed");

    for i in 0..file_count {
        let file_path = target_dir.join(format!("file_{:05}.txt", i));
        let mut updated = content.clone();
        updated.push_str("checkpoint-marker\n");
        fs::write(&file_path, &updated).expect("Failed to update bench file");
    }

    let checkpoint_start = Instant::now();
    repo.git_ai_with_env(&["checkpoint"], &[])
        .expect("git-ai checkpoint should succeed");
    let checkpoint_duration = checkpoint_start.elapsed();
    println!(
        "git-ai checkpoint duration (setup): {:.2}s",
        checkpoint_duration.as_secs_f64()
    );

    fs::remove_dir_all(&target_dir).expect("Failed to delete bench dir");

    let status_start = Instant::now();
    repo.git_ai_with_env(&["status", "--json"], &[])
        .expect("git-ai status should succeed");
    let status_duration = status_start.elapsed();

    println!(
        "git-ai status duration: {:.2}s (files deleted: {}, lines/file: {}, line_len: {})",
        status_duration.as_secs_f64(),
        file_count,
        lines_per_file,
        line_len
    );
    println!("========================================\n");
}
